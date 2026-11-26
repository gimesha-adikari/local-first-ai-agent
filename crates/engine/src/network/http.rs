//! Minimal HTTP/1.1 framing for the trusted transport. One request per
//! connection, no redirects, no compression, no upgrades and explicit bounds
//! on every header and body.

use zeroize::Zeroizing;

use super::Failure;
use crate::credentials::Secret;

pub(super) const USER_AGENT: &str = "local-first-ai-agent";

pub(super) fn encode_request(
    method: &str,
    authority: &str,
    path: &str,
    content_type: Option<&str>,
    body: &[u8],
    credential: Option<&Secret>,
) -> Zeroizing<Vec<u8>> {
    let mut head = Zeroizing::new(Vec::with_capacity(512 + body.len()));
    let mut line = |text: &str| {
        head.extend_from_slice(text.as_bytes());
        head.extend_from_slice(b"\r\n");
    };
    line(&format!("{method} {path} HTTP/1.1"));
    line(&format!("Host: {authority}"));
    line(&format!("User-Agent: {USER_AGENT}"));
    line("Accept: application/json, text/event-stream");
    line("Accept-Encoding: identity");
    line("Connection: close");
    if let Some(content_type) = content_type {
        line(&format!("Content-Type: {content_type}"));
    }
    if method != "GET" || !body.is_empty() {
        line(&format!("Content-Length: {}", body.len()));
    }
    if let Some(secret) = credential {
        head.extend_from_slice(b"Authorization: Bearer ");
        head.extend_from_slice(secret.expose().as_bytes());
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(b"\r\n");
    head.extend_from_slice(body);
    head
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Framing {
    Empty,
    Length(u64),
    Chunked,
    Close,
}

#[derive(Debug)]
pub(super) struct Head {
    pub status: u16,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub framing: Framing,
}

const MAX_HEADERS: usize = 64;
const MAX_LOCATION: usize = 2048;

fn header_text(value: &[u8]) -> Result<&str, Failure> {
    std::str::from_utf8(value)
        .map(str::trim)
        .map_err(|_| Failure::Protocol)
}

/// Parse a response head. `Ok(None)` means more bytes are needed.
/// Informational responses are consumed and skipped.
pub(super) fn parse_head(buffer: &mut Vec<u8>, max_head: usize) -> Result<Option<Head>, Failure> {
    loop {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut response = httparse::Response::new(&mut headers);
        let consumed = match response.parse(buffer) {
            Ok(httparse::Status::Complete(consumed)) => consumed,
            Ok(httparse::Status::Partial) => {
                return if buffer.len() > max_head {
                    Err(Failure::TooLarge)
                } else {
                    Ok(None)
                };
            }
            Err(httparse::Error::TooManyHeaders) => return Err(Failure::TooLarge),
            Err(_) => return Err(Failure::Protocol),
        };
        if consumed > max_head {
            return Err(Failure::TooLarge);
        }
        let status = response.code.ok_or(Failure::Protocol)?;
        if status == 101 {
            return Err(Failure::Protocol);
        }
        if (100..200).contains(&status) {
            buffer.drain(..consumed);
            continue;
        }
        let mut length: Option<u64> = None;
        let mut chunked = false;
        let mut content_type = None;
        let mut location = None;
        for header in response.headers.iter() {
            let name = header.name.to_ascii_lowercase();
            match name.as_str() {
                "content-length" => {
                    let value: u64 = header_text(header.value)?
                        .parse()
                        .map_err(|_| Failure::Protocol)?;
                    if length.is_some_and(|known| known != value) {
                        return Err(Failure::Protocol);
                    }
                    length = Some(value);
                }
                "transfer-encoding" => {
                    if !header_text(header.value)?.eq_ignore_ascii_case("chunked") || chunked {
                        return Err(Failure::Protocol);
                    }
                    chunked = true;
                }
                "content-encoding" => {
                    if !header_text(header.value)?.eq_ignore_ascii_case("identity") {
                        return Err(Failure::Protocol);
                    }
                }
                "content-type" => {
                    let value = header_text(header.value)?;
                    content_type = Some(value.chars().take(128).collect());
                }
                "location" => {
                    let value = header_text(header.value)?;
                    if value.len() <= MAX_LOCATION && value.bytes().all(|b| b.is_ascii_graphic()) {
                        location = Some(value.to_owned());
                    }
                }
                _ => {}
            }
        }
        // Both framings at once is a request-smuggling shape; refuse it.
        if chunked && length.is_some() {
            return Err(Failure::Protocol);
        }
        let framing = if status == 204 || status == 304 {
            Framing::Empty
        } else if chunked {
            Framing::Chunked
        } else if let Some(length) = length {
            Framing::Length(length)
        } else {
            Framing::Close
        };
        buffer.drain(..consumed);
        return Ok(Some(Head {
            status,
            content_type,
            location,
            framing,
        }));
    }
}

const MAX_CHUNK_LINE: usize = 1024;
const MAX_TRAILERS: usize = 8 * 1024;

#[derive(Debug)]
enum Chunk {
    Size,
    Data(u64),
    DataEnd,
    Trailers(usize),
}

/// Incremental body decoder with a total size bound.
#[derive(Debug)]
pub(super) struct Decoder {
    framing: Framing,
    chunk: Chunk,
    remaining: u64,
    total: usize,
    max_body: usize,
    done: bool,
}

impl Decoder {
    pub fn new(framing: Framing, max_body: usize) -> Result<Self, Failure> {
        let remaining = match framing {
            Framing::Length(length) if length > max_body as u64 => return Err(Failure::TooLarge),
            Framing::Length(length) => length,
            _ => 0,
        };
        Ok(Self {
            framing,
            chunk: Chunk::Size,
            remaining,
            total: 0,
            max_body,
            done: matches!(framing, Framing::Empty | Framing::Length(0)),
        })
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn take(
        &mut self,
        buffer: &mut Vec<u8>,
        count: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), Failure> {
        self.total = self.total.saturating_add(count);
        if self.total > self.max_body {
            return Err(Failure::TooLarge);
        }
        out.extend(buffer.drain(..count));
        Ok(())
    }

    /// Move decoded body bytes from `buffer` to `out`.
    pub fn decode(&mut self, buffer: &mut Vec<u8>, out: &mut Vec<u8>) -> Result<(), Failure> {
        while !self.done && !buffer.is_empty() {
            match self.framing {
                Framing::Empty => self.done = true,
                Framing::Close => {
                    let count = buffer.len();
                    self.take(buffer, count, out)?;
                }
                Framing::Length(_) => {
                    let count = buffer.len().min(self.remaining as usize);
                    self.take(buffer, count, out)?;
                    self.remaining -= count as u64;
                    self.done = self.remaining == 0;
                }
                Framing::Chunked => match self.chunk {
                    Chunk::Size => {
                        let Some(end) = find_line(buffer) else {
                            return if buffer.len() > MAX_CHUNK_LINE {
                                Err(Failure::Protocol)
                            } else {
                                Ok(())
                            };
                        };
                        let line =
                            std::str::from_utf8(&buffer[..end]).map_err(|_| Failure::Protocol)?;
                        let size = line.split(';').next().unwrap_or_default().trim();
                        if size.is_empty()
                            || size.len() > 15
                            || !size.bytes().all(|b| b.is_ascii_hexdigit())
                        {
                            return Err(Failure::Protocol);
                        }
                        let size = u64::from_str_radix(size, 16).map_err(|_| Failure::Protocol)?;
                        buffer.drain(..end + 2);
                        self.chunk = if size == 0 {
                            Chunk::Trailers(0)
                        } else {
                            Chunk::Data(size)
                        };
                    }
                    Chunk::Data(left) => {
                        let count = buffer.len().min(left as usize);
                        self.take(buffer, count, out)?;
                        let left = left - count as u64;
                        self.chunk = if left == 0 {
                            Chunk::DataEnd
                        } else {
                            Chunk::Data(left)
                        };
                    }
                    Chunk::DataEnd => {
                        if buffer.len() < 2 {
                            return Ok(());
                        }
                        if &buffer[..2] != b"\r\n" {
                            return Err(Failure::Protocol);
                        }
                        buffer.drain(..2);
                        self.chunk = Chunk::Size;
                    }
                    Chunk::Trailers(seen) => {
                        let Some(end) = find_line(buffer) else {
                            return if buffer.len() > MAX_CHUNK_LINE {
                                Err(Failure::Protocol)
                            } else {
                                Ok(())
                            };
                        };
                        let seen = seen + end + 2;
                        if seen > MAX_TRAILERS {
                            return Err(Failure::TooLarge);
                        }
                        buffer.drain(..end + 2);
                        if end == 0 {
                            self.done = true;
                        } else {
                            self.chunk = Chunk::Trailers(seen);
                        }
                    }
                },
            }
        }
        Ok(())
    }

    /// The connection closed. Only a close-delimited body may end this way.
    pub fn finish_at_eof(&mut self) -> Result<(), Failure> {
        if self.done || self.framing == Framing::Close {
            self.done = true;
            Ok(())
        } else {
            Err(Failure::Protocol)
        }
    }
}

fn find_line(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|pair| pair == b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Result<Option<Head>, Failure> {
        parse_head(&mut text.as_bytes().to_vec(), 64 * 1024)
    }

    #[test]
    fn parses_framing_and_rejects_ambiguous_heads() {
        let parsed = head("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.framing, Framing::Chunked);
        assert_eq!(parsed.content_type.as_deref(), Some("text/event-stream"));
        assert_eq!(
            head("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Empty
        );
        assert!(
            head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n")
                .unwrap()
                .is_none()
        );
        for bad in [
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n",
            "HTTP/1.1 101 Switching Protocols\r\n\r\n",
            "garbage\r\n\r\n",
        ] {
            assert!(head(bad).is_err(), "{bad:?}");
        }
        let many = format!(
            "HTTP/1.1 200 OK\r\n{}\r\n",
            (0..80).map(|i| format!("X-{i}: v\r\n")).collect::<String>()
        );
        assert_eq!(head(&many).unwrap_err(), Failure::TooLarge);
        let long = format!("HTTP/1.1 200 OK\r\nX: {}\r\n", "a".repeat(70 * 1024));
        assert_eq!(head(&long).unwrap_err(), Failure::TooLarge);
    }

    #[test]
    fn decodes_chunked_bodies_incrementally_within_bounds() {
        let wire = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nTrailer: x\r\n\r\n";
        let mut decoder = Decoder::new(Framing::Chunked, 1024).unwrap();
        let mut out = Vec::new();
        let mut buffer = Vec::new();
        for byte in wire {
            buffer.push(*byte);
            decoder.decode(&mut buffer, &mut out).unwrap();
        }
        assert!(decoder.is_done());
        assert_eq!(out, b"hello world");

        let mut small = Decoder::new(Framing::Chunked, 4).unwrap();
        let mut buffer = b"5\r\nhello\r\n".to_vec();
        assert_eq!(
            small.decode(&mut buffer, &mut Vec::new()),
            Err(Failure::TooLarge)
        );
        assert_eq!(
            Decoder::new(Framing::Length(10), 4).unwrap_err(),
            Failure::TooLarge
        );
        let mut bad = Decoder::new(Framing::Chunked, 1024).unwrap();
        assert_eq!(
            bad.decode(&mut b"zz\r\n".to_vec(), &mut Vec::new()),
            Err(Failure::Protocol)
        );
        let mut truncated = Decoder::new(Framing::Length(10), 1024).unwrap();
        truncated
            .decode(&mut b"abc".to_vec(), &mut Vec::new())
            .unwrap();
        assert_eq!(truncated.finish_at_eof(), Err(Failure::Protocol));
    }

    #[test]
    fn request_encoding_puts_the_credential_only_in_one_header() {
        let secret = Secret::new(Zeroizing::new("sk-fixture".to_owned())).unwrap();
        let bytes = encode_request(
            "POST",
            "api.example.com",
            "/v1/chat/completions",
            Some("application/json"),
            b"{}",
            Some(&secret),
        );
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            text.starts_with("POST /v1/chat/completions HTTP/1.1\r\nHost: api.example.com\r\n")
        );
        assert_eq!(text.matches("sk-fixture").count(), 1);
        assert!(text.contains("Authorization: Bearer sk-fixture\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\n{}"));
    }
}
