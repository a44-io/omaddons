//! Compressed API responses (M2-D14): API requests (`TimeoutKind::Api`) ask for gzip,
//! and a gzip body is decoded as it streams, here, below the guard, so the guard caps
//! and logs decoded bytes (a decompression-bomb guard) while `net.log` still learns the
//! compressed size. Downloads never ask for or decode gzip: an archive is kept as sent.
//! Replay and record transports sit below this layer, so fixtures exercise it.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::read::GzDecoder;

use super::{Http, Request, Response, TimeoutKind};
use crate::error::{Error, ErrorCode, Result, Suggestion};

/// Adds `Accept-Encoding: gzip` to API requests and decodes gzip responses.
pub struct Decoding {
    inner: Box<dyn Http>,
}

impl Decoding {
    pub fn new(inner: Box<dyn Http>) -> Self {
        Self { inner }
    }
}

impl Http for Decoding {
    fn send(&self, mut req: Request) -> Result<Response> {
        let asked = req
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("accept-encoding"));
        if req.timeout == TimeoutKind::Api && !asked {
            req.headers
                .push(("Accept-Encoding".to_owned(), "gzip".to_owned()));
        }
        let url = req.url.clone();
        let mut response = self.inner.send(req)?;
        let encoding = response
            .header("content-encoding")
            .map(|v| v.trim().to_ascii_lowercase());
        match encoding.as_deref() {
            None | Some("" | "identity") => Ok(response),
            Some("gzip") => {
                let wire = Arc::new(AtomicU64::new(0));
                let body = response.take_body();
                let counted = Counting {
                    inner: body,
                    count: Arc::clone(&wire),
                };
                response.set_body(Box::new(GzDecoder::new(counted)));
                // Decoded once, here: nothing above may decode it again. The length
                // stays: it is the wire length, which the guard may refuse up front.
                response
                    .headers
                    .retain(|(name, _)| !name.eq_ignore_ascii_case("content-encoding"));
                response.wire_bytes = Some(wire);
                Ok(response)
            }
            Some(other) => Err(Error::new(
                ErrorCode::Network,
                format!(
                    "{} answered with Content-Encoding {other}, which omaddons does not decode",
                    super::redact(&url)
                ),
                Suggestion::text("retry later; if it persists, report the source's response"),
            )
            .retryable(false)),
        }
    }
}

/// Counts the bytes read through it.
struct Counting {
    inner: Box<dyn Read + Send>,
    count: Arc<AtomicU64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::sync::Mutex;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    pub fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// Answers with `body` and `headers`, recording each request's headers.
    struct Fake {
        body: Vec<u8>,
        headers: Vec<(String, String)>,
        seen: Mutex<Vec<Vec<(String, String)>>>,
    }

    impl Fake {
        fn new(body: Vec<u8>, headers: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                body,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    fn decoding(fake: &Arc<Fake>) -> Decoding {
        Decoding::new(Box::new(Arc::clone(fake)))
    }

    impl Http for Arc<Fake> {
        fn send(&self, req: Request) -> Result<Response> {
            self.seen.lock().unwrap().push(req.headers.clone());
            Ok(Response::new(
                200,
                self.headers.clone(),
                req.url,
                Box::new(Cursor::new(self.body.clone())),
            ))
        }
    }

    fn accepts(fake: &Fake) -> Vec<Option<String>> {
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .map(|h| {
                h.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
                    .map(|(_, v)| v.clone())
            })
            .collect()
    }

    /// Reads `r` one byte at a time.
    fn read_slowly(mut r: impl Read) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match r.read(&mut byte)? {
                0 => return Ok(out),
                _ => out.push(byte[0]),
            }
        }
    }

    const URL: &str = "https://api.mmoui.com/v3/game/WOW/filelist.json";

    #[test]
    fn api_requests_ask_for_gzip_and_downloads_never_do() {
        let fake = Fake::new(b"{}".to_vec(), &[]);
        let decoding = decoding(&fake);
        decoding.send(Request::get(URL, "api")).unwrap();
        decoding
            .send(Request::get(URL, "asset").download())
            .unwrap();
        decoding
            .send(Request::get(URL, "api").header("Accept-Encoding", "identity"))
            .unwrap();
        assert_eq!(
            accepts(&fake),
            [Some("gzip".to_owned()), None, Some("identity".to_owned())]
        );
    }

    #[test]
    fn identity_passes_through_and_gzip_is_decoded_as_it_streams() {
        let plain = Fake::new(b"plain".to_vec(), &[("Content-Encoding", "identity")]);
        let response = decoding(&plain).send(Request::get(URL, "api")).unwrap();
        assert!(response.wire_bytes.is_none());
        assert_eq!(response.bytes().unwrap(), b"plain");

        let json = br#"{"files":[1,2,3]}"#.repeat(100);
        let gz = gzip(&json);
        let fake = Fake::new(
            gz.clone(),
            &[("Content-Encoding", "gzip"), ("Content-Length", "99")],
        );
        let mut response = decoding(&fake).send(Request::get(URL, "api")).unwrap();
        assert_eq!(response.header("content-encoding"), None);
        assert_eq!(
            response.header("content-length"),
            Some("99"),
            "the wire length stays"
        );
        let wire = response.wire_bytes.clone().unwrap();
        let body = read_slowly(response.take_body()).unwrap();
        assert_eq!(body, json);
        assert_eq!(wire.load(Ordering::Relaxed), gz.len() as u64);
    }

    #[test]
    fn other_encodings_and_broken_gzip_fail() {
        for encoding in ["br", "deflate", "zstd", "gzip, br"] {
            let fake = Fake::new(b"x".to_vec(), &[("Content-Encoding", encoding)]);
            let err = decoding(&fake).send(Request::get(URL, "api")).unwrap_err();
            assert_eq!(
                (err.code, err.retryable),
                (ErrorCode::Network, false),
                "{encoding}"
            );
            assert!(err.message.contains(encoding), "{}", err.message);
        }
        let mut gz = gzip(b"truncated body, long enough to cut");
        gz.truncate(gz.len() / 2);
        let fake = Fake::new(gz, &[("Content-Encoding", "gzip")]);
        let response = decoding(&fake).send(Request::get(URL, "api")).unwrap();
        let err = response.bytes().unwrap_err();
        assert_eq!(err.code, ErrorCode::Network);
    }
}
