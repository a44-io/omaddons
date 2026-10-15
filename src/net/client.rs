//! The real transport: one ureq agent on rustls with the ring provider. ureq follows no
//! redirects (the `Redirecting` layer does, hop by hop through the guard), treats no
//! status as an error, and uses no proxy.

use super::{Http, Method, Request, Response, TimeoutKind};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::politeness::{API_READ_TIMEOUT, CONNECT_TIMEOUT, DOWNLOAD_TIMEOUT, USER_AGENT};

/// Sends requests over the network.
pub struct UreqTransport {
    agent: ureq::Agent,
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
        let timeout = match req.timeout {
            TimeoutKind::Api => API_READ_TIMEOUT,
            TimeoutKind::Download => DOWNLOAD_TIMEOUT,
        };
        let mut builder = match req.method {
            Method::Get => self.agent.get(&req.url),
            Method::Head => self.agent.head(&req.url),
        };
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
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
