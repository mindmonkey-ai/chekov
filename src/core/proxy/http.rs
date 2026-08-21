//! Minimal HTTP/1.1 over `std::net` — no server crate (approved: hand-roll
//! rather than add a dependency).
//!
//! Deliberately narrow: the only client is a coding agent on loopback issuing
//! `Content-Length`-delimited JSON. Chunked *request* bodies and keep-alive are
//! not supported; every response carries `Connection: close`, which is legal
//! HTTP/1.1 and is what makes SSE termination unambiguous without chunking.

use std::io::{BufRead, BufReader, Read, Write};

use crate::error::ChekovError;

/// Cap on a single request body. An agent prompt is large but bounded; without
/// a ceiling a malformed `Content-Length` becomes an OOM.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// A parsed inbound request.
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// Body as UTF-8, or a 400-shaped error.
    pub fn body_str(&self) -> Result<&str, ChekovError> {
        std::str::from_utf8(&self.body).map_err(|e| ChekovError::ProxyBadRequest {
            reason: format!("request body is not valid UTF-8: {e}"),
        })
    }
}

/// A non-streaming response.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    #[must_use]
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }

    /// Error body in the Anthropic/`OpenAI` shared `{"error":{...}}` shape.
    #[must_use]
    pub fn error(status: u16, kind: &str, message: &str) -> Self {
        let payload = serde_json::json!({
            "type": "error",
            "error": { "type": kind, "message": message },
        });
        Self::json(status, payload.to_string())
    }
}

/// Reason phrases for the statuses this proxy emits.
const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    }
}

/// Read one request: request line, headers, then a `Content-Length` body.
pub fn read_request<R: Read>(stream: R) -> Result<HttpRequest, ChekovError> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    read_line(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    if method.is_empty() || path.is_empty() {
        return Err(ChekovError::ProxyBadRequest {
            reason: format!("malformed request line: {:?}", line.trim_end()),
        });
    }
    let len = read_content_length(&mut reader)?;
    let mut body = vec![0_u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| ChekovError::io("reading proxy request body", e))?;
    Ok(HttpRequest { method, path, body })
}

/// Consume headers, returning the declared body length.
fn read_content_length<R: Read>(reader: &mut BufReader<R>) -> Result<usize, ChekovError> {
    let mut len = 0_usize;
    let mut line = String::new();
    loop {
        read_line(reader, &mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return Ok(len);
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            len = parse_length(value.trim())?;
        }
    }
}

fn parse_length(raw: &str) -> Result<usize, ChekovError> {
    let len: usize = raw.parse().map_err(|_| ChekovError::ProxyBadRequest {
        reason: format!("unparseable Content-Length: {raw:?}"),
    })?;
    if len > MAX_BODY_BYTES {
        return Err(ChekovError::ProxyBadRequest {
            reason: format!("body of {len} bytes exceeds the {MAX_BODY_BYTES}-byte ceiling"),
        });
    }
    Ok(len)
}

/// Read one CRLF-terminated line, erroring on a truncated stream.
fn read_line<R: Read>(reader: &mut BufReader<R>, buf: &mut String) -> Result<(), ChekovError> {
    buf.clear();
    let read = reader
        .read_line(buf)
        .map_err(|e| ChekovError::io("reading proxy request line", e))?;
    if read == 0 {
        return Err(ChekovError::ProxyBadRequest {
            reason: "client closed the connection mid-request".to_owned(),
        });
    }
    Ok(())
}

/// Write a complete non-streaming response.
pub fn write_response<W: Write>(mut out: W, res: &HttpResponse) -> Result<(), ChekovError> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         content-type: application/json\r\n\
         content-length: {len}\r\n\
         connection: close\r\n\r\n",
        status = res.status,
        reason = reason(res.status),
        len = res.body.len(),
    );
    out.write_all(head.as_bytes())
        .and_then(|()| out.write_all(res.body.as_bytes()))
        .and_then(|()| out.flush())
        .map_err(|e| ChekovError::io("writing proxy response", e))
}

/// Open an SSE response and keep the socket writable for frames.
pub fn write_sse_head<W: Write>(out: &mut W) -> Result<(), ChekovError> {
    out.write_all(
        b"HTTP/1.1 200 OK\r\n\
          content-type: text/event-stream\r\n\
          cache-control: no-cache\r\n\
          connection: close\r\n\r\n",
    )
    .and_then(|()| out.flush())
    .map_err(|e| ChekovError::io("opening proxy SSE stream", e))
}

/// Write one `event:`/`data:` frame and flush — buffering defeats streaming.
pub fn write_sse_event<W: Write>(out: &mut W, ev: &super::SseEvent) -> Result<(), ChekovError> {
    let frame = format!("event: {}\ndata: {}\n\n", ev.event, ev.data);
    out.write_all(frame.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| ChekovError::io("writing proxy SSE event", e))
}

#[cfg(test)]
mod tests {
    use super::{HttpResponse, MAX_BODY_BYTES, read_request, write_response};

    #[test]
    fn parses_method_path_and_exact_body() {
        let body = r#"{"model":"m"}"#;
        let raw = format!(
            "POST /v1/messages HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let req = read_request(raw.as_slice()).expect("parse");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/messages");
        assert_eq!(req.body_str().expect("utf8"), body);
    }

    #[test]
    fn header_name_match_is_case_insensitive() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi".to_vec();
        let req = read_request(raw.as_slice()).expect("parse");
        assert_eq!(req.body_str().expect("utf8"), "hi");
    }

    #[test]
    fn absent_content_length_reads_empty_body() {
        let raw = b"GET /v1/models HTTP/1.1\r\nhost: x\r\n\r\n".to_vec();
        let req = read_request(raw.as_slice()).expect("parse");
        assert_eq!(req.method, "GET");
        assert!(req.body.is_empty());
    }

    #[test]
    fn oversized_content_length_is_refused_before_allocating() {
        let raw = format!(
            "POST /x HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        )
        .into_bytes();
        let err = read_request(raw.as_slice()).expect_err("must refuse");
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[test]
    fn truncated_body_errors_rather_than_returning_short() {
        let raw = b"POST /x HTTP/1.1\r\ncontent-length: 100\r\n\r\nshort".to_vec();
        assert!(read_request(raw.as_slice()).is_err());
    }

    #[test]
    fn response_declares_byte_length_not_char_length() {
        let mut out = Vec::new();
        write_response(&mut out, &HttpResponse::json(200, "{\"t\":\"café\"}")).expect("write");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("content-length: 13"), "{text}");
        assert!(text.contains("connection: close"), "{text}");
    }

    #[test]
    fn error_body_carries_type_and_message() {
        let res = HttpResponse::error(404, "not_found_error", "no such route");
        assert_eq!(res.status, 404);
        assert!(res.body.contains("not_found_error"), "{}", res.body);
        assert!(res.body.contains("no such route"), "{}", res.body);
    }
}
