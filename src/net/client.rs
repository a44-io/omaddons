//! The real transport: one ureq agent on rustls with the ring provider. ureq follows no
//! redirects (the `Redirecting` layer does, hop by hop through the guard), treats no
//! status as an error, and uses no proxy.

use std::time::Duration;

use super::{Http, Method, Request, Response, TimeoutKind};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::politeness::{API_READ_TIMEOUT, CONNECT_TIMEOUT, DOWNLOAD_TIMEOUT, USER_AGENT};

/// Sends requests over the network.
pub struct UreqTransport {
    agent: ureq::Agent,
    /// The whole-call timeout of an API request (`politeness::API_READ_TIMEOUT`).
    api_timeout: Duration,
    /// The whole-call timeout of a download (`politeness::DOWNLOAD_TIMEOUT`).
    download_timeout: Duration,
}

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent(USER_AGENT)
            // Proxies are not supported (M1): the default would read them from the
            // environment.
            .proxy(None)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            api_timeout: API_READ_TIMEOUT,
            download_timeout: DOWNLOAD_TIMEOUT,
        }
    }

    /// The production transport with shorter timeouts, for this module's tests only.
    #[cfg(test)]
    fn with_timeouts(api: Duration, download: Duration) -> Self {
        Self {
            api_timeout: api,
            download_timeout: download,
            ..Self::new()
        }
    }

    /// The whole-call timeout of a request of `kind`.
    fn timeout_for(&self, kind: TimeoutKind) -> Duration {
        match kind {
            TimeoutKind::Api => self.api_timeout,
            TimeoutKind::Download => self.download_timeout,
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Http for UreqTransport {
    fn send(&self, req: Request) -> Result<Response> {
        let timeout = self.timeout_for(req.timeout);
        let response = match req.method {
            Method::Get | Method::Head => {
                let mut builder = match req.method {
                    Method::Head => self.agent.head(&req.url),
                    _ => self.agent.get(&req.url),
                };
                for (name, value) in &req.headers {
                    builder = builder.header(name, value);
                }
                builder
                    .config()
                    .timeout_global(Some(timeout))
                    .build()
                    .call()
            }
            // A WebDAV method with its one fixed body, which ureq sends only when told
            // non-standard methods are allowed.
            Method::Propfind => {
                let mut builder = ureq::http::Request::builder()
                    .method(Method::Propfind.as_str())
                    .uri(&req.url);
                for (name, value) in &req.headers {
                    builder = builder.header(name, value);
                }
                let request = builder
                    .body(super::PROPFIND_VERSION_NAMES)
                    .map_err(|err| Error::internal(format!("a PROPFIND request: {err}")))?;
                let request = self
                    .agent
                    .configure_request(request)
                    .allow_non_standard_methods(true)
                    .timeout_global(Some(timeout))
                    .build();
                self.agent.run(request)
            }
        }
        .map_err(|err| transport_error(&req.url, &err))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = response.into_body().into_reader();
        Ok(Response::new(status, headers, req.url, Box::new(body)))
    }
}

/// DNS, connect, TLS, timeout and reset failures: `network`, retryable.
fn transport_error(url: &str, err: &ureq::Error) -> Error {
    Error::new(
        ErrorCode::Network,
        format!("request to {} failed: {err}", super::redact(url)),
        Suggestion::text("check the network connection, then retry"),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use super::*;

    /// How long the loopback server waits for anything, so a broken test cannot hang.
    const SERVER_WAIT: Duration = Duration::from_secs(5);

    /// The next connection to `listener`, waiting at most [`SERVER_WAIT`].
    fn accept(listener: &TcpListener) -> std::io::Result<TcpStream> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + SERVER_WAIT;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(SERVER_WAIT))?;
                    return Ok(stream);
                }
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock) => {
                    if Instant::now() > deadline {
                        return Err(err);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// A request head, up to its blank line.
    fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.extend_from_slice(&byte),
                _ => break,
            }
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    /// A plain-HTTP server on a kernel-assigned loopback port: each of `responses`
    /// answers one connection, whose request head it records, then closes it (so no
    /// connection is reused). The handle returns the recorded heads.
    fn serve(responses: Vec<Vec<u8>>) -> (SocketAddr, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for response in responses {
                let Ok(mut stream) = accept(&listener) else {
                    break;
                };
                heads.push(read_head(&mut stream));
                let _ = stream.write_all(&response);
            }
            heads
        });
        (addr, server)
    }

    /// An HTTP/1.1 response that closes its connection.
    fn response(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(body);
        out.into_bytes()
    }

    /// What the module doc promises, from the agent's own configuration: a ureq upgrade
    /// or a builder edit that changes it fails here.
    #[test]
    fn the_agent_is_configured_as_documented() {
        use ureq::config::AutoHeaderValue;
        use ureq::tls::{RootCerts, TlsProvider};

        let transport = UreqTransport::new();
        let config = transport.agent.config();
        assert_eq!(
            config.max_redirects(),
            0,
            "the Redirecting layer follows them"
        );
        assert!(!config.http_status_as_error());
        assert!(
            config.proxy().is_none(),
            "no proxy, none from the environment"
        );
        assert!(
            matches!(config.user_agent(), AutoHeaderValue::Provided(ua) if ua.as_str() == USER_AGENT),
            "{:?}",
            config.user_agent()
        );
        assert_eq!(config.timeouts().connect, Some(CONNECT_TIMEOUT));
        let tls = config.tls_config();
        assert_eq!(tls.provider(), TlsProvider::Rustls);
        assert!(
            matches!(tls.root_certs(), RootCerts::WebPki),
            "the bundled Mozilla roots, not the system store"
        );
        assert!(!tls.disable_verification() && tls.use_sni());
    }

    #[test]
    fn timeouts_follow_the_request_kind() {
        let transport = UreqTransport::new();
        assert_eq!(transport.timeout_for(TimeoutKind::Api), API_READ_TIMEOUT);
        assert_eq!(
            transport.timeout_for(TimeoutKind::Download),
            DOWNLOAD_TIMEOUT
        );
    }

    /// A 302 comes back to the caller: the hop to its `Location` is the `Redirecting`
    /// layer's, through the allowlist guard.
    #[test]
    fn a_redirect_is_returned_not_followed() {
        let elsewhere = TcpListener::bind("127.0.0.1:0").unwrap();
        elsewhere.set_nonblocking(true).unwrap();
        let next = format!("http://{}/next", elsewhere.local_addr().unwrap());
        let (addr, server) = serve(vec![response("302 Found", &[("Location", &next)], "")]);
        let got = UreqTransport::new()
            .send(Request::get(format!("http://{addr}/start"), "test"))
            .unwrap();
        assert_eq!((got.status, got.location()), (302, Some(next.as_str())));
        assert_eq!(server.join().unwrap().len(), 1);
        assert!(
            matches!(elsewhere.accept(), Err(err) if matches!(err.kind(), ErrorKind::WouldBlock)),
            "the redirect target saw no request"
        );
    }

    /// A `PROPFIND` goes out as one, one level deep, with its one fixed body.
    #[test]
    fn a_propfind_sends_its_method_depth_and_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener).unwrap();
            let head = read_head(&mut stream);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            stream.read_exact(&mut body).unwrap();
            let _ = stream.write_all(&response("207 Multi-Status", &[], "<D:multistatus/>"));
            (head, String::from_utf8(body).unwrap())
        });
        let url = format!("http://{addr}/wow/x/tags/");
        let r = UreqTransport::new()
            .send(Request::propfind_version_names(url, "wowace-tags"))
            .unwrap();
        assert_eq!(r.status, 207);
        let (head, body) = server.join().unwrap();
        assert!(
            head.starts_with("PROPFIND /wow/x/tags/ HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            head.to_ascii_lowercase().contains("\r\ndepth: 1\r\n"),
            "{head}"
        );
        assert_eq!(body, super::super::PROPFIND_VERSION_NAMES);
    }

    #[test]
    fn error_statuses_are_responses() {
        let (addr, server) = serve(vec![
            response("404 Not Found", &[], "missing"),
            response("503 Service Unavailable", &[], "busy"),
        ]);
        let transport = UreqTransport::new();
        for (status, body) in [(404, "missing"), (503, "busy")] {
            let got = transport
                .send(Request::get(format!("http://{addr}/{status}"), "test"))
                .unwrap();
            assert_eq!(got.status, status);
            assert_eq!(got.bytes().unwrap(), body.as_bytes());
        }
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn headers_go_out_and_come_back() {
        let (addr, server) = serve(vec![
            response(
                "200 OK",
                &[("ETag", "\"abc\""), ("X-Custom", "yes")],
                "hello",
            ),
            response("200 OK", &[], ""),
        ]);
        let transport = UreqTransport::new();
        let got = transport
            .send(
                Request::get(format!("http://{addr}/x"), "test")
                    .header("accept", "application/json")
                    .header("if-none-match", "\"abc\""),
            )
            .unwrap();
        assert_eq!(
            (got.status, got.header("etag"), got.header("x-custom")),
            (200, Some("\"abc\""), Some("yes"))
        );
        assert_eq!(got.bytes().unwrap(), b"hello");
        let mut head = Request::get(format!("http://{addr}/h"), "test");
        head.method = Method::Head;
        assert_eq!(transport.send(head).unwrap().status, 200);

        let heads = server.join().unwrap();
        let lines =
            |head: &str| -> Vec<String> { head.lines().map(str::to_ascii_lowercase).collect() };
        let get = lines(&heads[0]);
        assert!(get[0].starts_with("get /x "), "{get:?}");
        for line in [
            format!("user-agent: {}", USER_AGENT.to_ascii_lowercase()),
            "accept: application/json".to_owned(),
            "if-none-match: \"abc\"".to_owned(),
        ] {
            assert!(get.contains(&line), "{line} in {get:?}");
        }
        assert!(heads[1].starts_with("HEAD /h "), "{}", heads[1]);
    }

    /// A server that never answers ends the call at its timeout, as a retryable
    /// `network` error (the 5 s bound only proves the timeout fired).
    #[test]
    fn a_stalled_server_times_out_as_network() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let stream = accept(&listener);
            let _ = wait.recv_timeout(SERVER_WAIT);
            drop(stream);
        });
        let transport =
            UreqTransport::with_timeouts(Duration::from_millis(200), Duration::from_millis(200));
        let started = Instant::now();
        let err = transport
            .send(Request::get(format!("http://{addr}/slow"), "test"))
            .unwrap_err();
        let elapsed = started.elapsed();
        let _ = done.send(());
        server.join().unwrap();
        assert_eq!(
            (err.code, err.retryable),
            (ErrorCode::Network, true),
            "{err:?}"
        );
        assert!(elapsed < SERVER_WAIT, "{elapsed:?}");
    }

    /// Nothing listening: a retryable `network` error naming the host, never the query.
    #[test]
    fn a_refused_connection_is_network_and_redacted() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = UreqTransport::new()
            .send(Request::get(
                format!("http://127.0.0.1:{port}/x?token=SECRET"),
                "test",
            ))
            .unwrap_err();
        assert_eq!((err.code, err.retryable), (ErrorCode::Network, true));
        assert!(err.message.contains("127.0.0.1"), "{}", err.message);
        assert!(!err.message.contains("SECRET"), "{}", err.message);
    }
}
