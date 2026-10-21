//! Loopback HTTP transport for exercising the desktop UI against its real Rust backend.

use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::time::timeout;

#[path = "native_origin.rs"]
pub mod native_origin;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_CAPACITY: usize = 256;

#[derive(Debug)]
struct HttpError(u16, &'static str);

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    host: String,
    origin: Option<String>,
    fetch_site: Option<String>,
    fetch_mode: Option<String>,
    fetch_dest: Option<String>,
    content_type: Option<String>,
    content_length: usize,
    body: Vec<u8>,
}

/// Serve the same HTML and JSON-RPC implementation used by the native window.
/// The page must open `/events` before submitting requests to `/rpc`.
pub async fn serve(shared: crate::appserver::Shared, port: u16) -> Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .context("bind desktop preview to loopback")?;
    let port = listener.local_addr()?.port();
    let (rpc_tx, rpc_rx) = mpsc::unbounded_channel();
    let (output_tx, mut output_rx) = mpsc::unbounded_channel::<Value>();
    let (events, _) = broadcast::channel::<Arc<str>>(EVENT_CAPACITY);
    let server = tokio::spawn(crate::appserver::run_channel(shared, rpc_rx, output_tx));
    let publish = events.clone();
    let fanout = tokio::spawn(async move {
        while let Some(value) = output_rx.recv().await {
            // JSON serialization escapes embedded newlines, keeping every event one SSE record.
            let _ = publish.send(Arc::from(format!("data: {value}\n\n")));
        }
    });
    let _tasks = BackgroundTasks([server, fanout]);
    let connections = Arc::new(Semaphore::new(64));
    eprintln!("Desktop preview: http://127.0.0.1:{port}");

    loop {
        let permit = connections.clone().acquire_owned().await?;
        let (stream, _) = listener
            .accept()
            .await
            .context("accept desktop preview connection")?;
        let (rpc_tx, events) = (rpc_tx.clone(), events.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _ = handle_connection(stream, port, rpc_tx, events).await;
        });
    }
}

struct BackgroundTasks([tokio::task::JoinHandle<()>; 2]);

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

async fn handle_connection(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    port: u16,
    rpc: mpsc::UnboundedSender<Value>,
    events: broadcast::Sender<Arc<str>>,
) -> Result<()> {
    let request = match timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(HttpError(status, message))) => {
            return respond(
                &mut stream,
                status,
                "text/plain; charset=utf-8",
                message.as_bytes(),
            )
            .await;
        }
        Err(_) => {
            return respond(
                &mut stream,
                408,
                "text/plain; charset=utf-8",
                b"Request timed out",
            )
            .await
        }
    };
    if let Err(HttpError(status, message)) = validate_origin(&request, port) {
        return respond(
            &mut stream,
            status,
            "text/plain; charset=utf-8",
            message.as_bytes(),
        )
        .await;
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let html = crate::gui::app_html();
            respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
            .await
        }
        ("GET", "/events") => stream_events(stream, events.subscribe()).await,
        ("POST", "/rpc") => {
            if !request.content_type.as_deref().is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            }) {
                return respond(
                    &mut stream,
                    415,
                    "text/plain; charset=utf-8",
                    b"Expected application/json",
                )
                .await;
            }
            let message = match serde_json::from_slice::<Value>(&request.body) {
                Ok(message) if message.is_object() => message,
                _ => {
                    return respond(
                        &mut stream,
                        400,
                        "text/plain; charset=utf-8",
                        b"Expected a JSON-RPC object",
                    )
                    .await
                }
            };
            if rpc.send(message).is_err() {
                return respond(
                    &mut stream,
                    503,
                    "text/plain; charset=utf-8",
                    b"Backend is unavailable",
                )
                .await;
            }
            respond(&mut stream, 202, "application/json", b"{\"accepted\":true}").await
        }
        ("GET" | "POST", _) => {
            respond(&mut stream, 404, "text/plain; charset=utf-8", b"Not found").await
        }
        _ => {
            respond(
                &mut stream,
                405,
                "text/plain; charset=utf-8",
                b"Method not allowed",
            )
            .await
        }
    }
}

async fn read_request(
    reader: &mut (impl AsyncRead + Unpin),
) -> std::result::Result<Request, HttpError> {
    read_request_limited(reader, MAX_BODY_BYTES).await
}

async fn read_request_limited(
    reader: &mut (impl AsyncRead + Unpin),
    body_limit: usize,
) -> std::result::Result<Request, HttpError> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if let Some(start) = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let end = start + 4;
            if end > MAX_HEADER_BYTES {
                return Err(HttpError(431, "Request headers are too large"));
            }
            break end;
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Err(HttpError(431, "Request headers are too large"));
        }
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|_| HttpError(400, "Cannot read request"))?;
        if count == 0 {
            return Err(HttpError(400, "Incomplete request headers"));
        }
        buffer.extend_from_slice(&chunk[..count]);
    };
    let mut request = parse_head(&buffer[..header_end])?;
    if request.content_length > body_limit {
        return Err(HttpError(413, "Request body is too large"));
    }
    if request.method != "POST" && request.content_length != 0 {
        return Err(HttpError(400, "Unexpected request body"));
    }
    request.body.resize(request.content_length, 0);
    let available = (buffer.len() - header_end).min(request.content_length);
    request.body[..available].copy_from_slice(&buffer[header_end..header_end + available]);
    reader
        .read_exact(&mut request.body[available..])
        .await
        .map_err(|_| HttpError(400, "Incomplete request body"))?;
    Ok(request)
}

fn parse_head(bytes: &[u8]) -> std::result::Result<Request, HttpError> {
    let text = std::str::from_utf8(bytes).map_err(|_| HttpError(400, "Invalid request headers"))?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("");
    let path = first.next().unwrap_or("");
    if !is_token(method)
        || !path.starts_with('/')
        || path.bytes().any(|c| c <= 32 || c >= 127)
        || first.next() != Some("HTTP/1.1")
        || first.next().is_some()
    {
        return Err(HttpError(400, "Invalid request line"));
    }
    let mut request = Request {
        method: method.into(),
        path: path.into(),
        host: String::new(),
        origin: None,
        fetch_site: None,
        fetch_mode: None,
        fetch_dest: None,
        content_type: None,
        content_length: 0,
        body: Vec::new(),
    };
    let mut length = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(HttpError(400, "Invalid request header"))?;
        if !is_token(name) || value.bytes().any(|c| (c < 32 && c != b'\t') || c == 127) {
            return Err(HttpError(400, "Invalid request header"));
        }
        let value = value.trim_matches([' ', '\t']);
        match name.to_ascii_lowercase().as_str() {
            "host" => {
                if !request.host.is_empty() || value.is_empty() {
                    return Err(HttpError(400, "Invalid Host header"));
                }
                request.host = value.to_ascii_lowercase();
            }
            "origin" => set_once(&mut request.origin, value)?,
            "sec-fetch-site" => set_once(&mut request.fetch_site, value)?,
            "sec-fetch-mode" => set_once(&mut request.fetch_mode, value)?,
            "sec-fetch-dest" => set_once(&mut request.fetch_dest, value)?,
            "content-type" => set_once(&mut request.content_type, value)?,
            "content-length" => {
                if length.is_some()
                    || value.is_empty()
                    || !value.bytes().all(|c| c.is_ascii_digit())
                {
                    return Err(HttpError(400, "Invalid Content-Length header"));
                }
                let count = value
                    .parse::<usize>()
                    .map_err(|_| HttpError(413, "Request body is too large"))?;
                if count > MAX_BODY_BYTES {
                    return Err(HttpError(413, "Request body is too large"));
                }
                length = Some(count);
            }
            "transfer-encoding" => {
                return Err(HttpError(400, "Transfer-Encoding is not supported"))
            }
            "expect" => return Err(HttpError(417, "Expect is not supported")),
            _ => {}
        }
    }
    request.content_length = length.unwrap_or(0);
    Ok(request)
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

fn set_once(target: &mut Option<String>, value: &str) -> std::result::Result<(), HttpError> {
    if target.is_some() {
        return Err(HttpError(400, "Duplicate request header"));
    }
    *target = Some(value.into());
    Ok(())
}

fn validate_origin(request: &Request, port: u16) -> std::result::Result<(), HttpError> {
    let allowed_host = request.host == format!("127.0.0.1:{port}")
        || request.host == format!("localhost:{port}")
        || (port == 80 && matches!(request.host.as_str(), "127.0.0.1" | "localhost"));
    if !allowed_host {
        return Err(HttpError(
            403,
            "Host must match the loopback preview address",
        ));
    }
    // A browser extension or external link can open the UI as a cross-site
    // navigation. Only the root document gets this exception, never RPC or SSE.
    let top_level_document = request.method == "GET"
        && request.path == "/"
        && request.fetch_mode.as_deref() == Some("navigate")
        && request.fetch_dest.as_deref() == Some("document");
    match request.fetch_site.as_deref() {
        None | Some("same-origin" | "none") => {}
        Some("same-site" | "cross-site") if top_level_document => {}
        _ => return Err(HttpError(403, "Cross-origin requests are not allowed")),
    }
    match &request.origin {
        Some(origin) if origin.eq_ignore_ascii_case(&format!("http://{}", request.host)) => Ok(()),
        None if request.method == "GET" => Ok(()),
        _ => Err(HttpError(
            403,
            "Origin must match the loopback preview address",
        )),
    }
}

async fn respond(
    stream: &mut (impl AsyncWrite + Unpin),
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\n\r\n",
        body.len()
    );
    timeout(WRITE_TIMEOUT, async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.shutdown().await
    })
    .await
    .context("preview response timed out")??;
    Ok(())
}

async fn stream_events(
    mut stream: impl AsyncWrite + Unpin,
    mut events: broadcast::Receiver<Arc<str>>,
) -> Result<()> {
    timeout(WRITE_TIMEOUT, stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-store\r\nConnection: keep-alive\r\nTransfer-Encoding: chunked\r\nX-Content-Type-Options: nosniff\r\nX-Accel-Buffering: no\r\n\r\n"
    )).await.context("preview SSE headers timed out")??;
    write_event_chunk(&mut stream, ": connected\n\n").await?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = events.recv() => match result {
                Ok(event) => write_event_chunk(&mut stream, &event).await?,
                // A lagged consumer must reconnect and reload state. Do not replay an incomplete stream.
                Err(broadcast::error::RecvError::Lagged(_)) | Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => write_event_chunk(&mut stream, ": heartbeat\n\n").await?,
        }
    }
    timeout(WRITE_TIMEOUT, stream.write_all(b"0\r\n\r\n"))
        .await
        .context("preview SSE close timed out")??;
    Ok(())
}

async fn write_event_chunk(stream: &mut (impl AsyncWrite + Unpin), event: &str) -> Result<()> {
    timeout(WRITE_TIMEOUT, async {
        stream
            .write_all(format!("{:x}\r\n", event.len()).as_bytes())
            .await?;
        stream.write_all(event.as_bytes()).await?;
        stream.write_all(b"\r\n").await
    })
    .await
    .context("preview SSE consumer is too slow")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(extra: &str) -> Request {
        parse_head(format!("POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:8999\r\n{extra}\r\n").as_bytes())
            .unwrap()
    }

    #[test]
    fn loopback_rpc_requires_matching_origin_and_port() {
        assert!(validate_origin(&head("Origin: http://127.0.0.1:8999\r\n"), 8999).is_ok());
        for origin in [
            "http://evil.example",
            "http://127.0.0.1:8998",
            "http://localhost:8999",
            "null",
        ] {
            assert_eq!(
                validate_origin(&head(&format!("Origin: {origin}\r\n")), 8999)
                    .unwrap_err()
                    .0,
                403
            );
        }
        assert_eq!(validate_origin(&head(""), 8999).unwrap_err().0, 403);
    }

    #[test]
    fn hostile_hosts_and_cross_site_fetches_are_rejected() {
        let mut request = head("Origin: http://127.0.0.1:8999\r\n");
        for host in [
            "evil.example:8999",
            "127.0.0.1.evil.example:8999",
            "localhost",
            "[::1]:8999",
        ] {
            request.host = host.into();
            assert_eq!(validate_origin(&request, 8999).unwrap_err().0, 403);
        }
        assert_eq!(
            validate_origin(
                &head("Origin: http://127.0.0.1:8999\r\nSec-Fetch-Site: cross-site\r\n"),
                8999
            )
            .unwrap_err()
            .0,
            403
        );
    }

    #[test]
    fn same_origin_gets_work_without_origin_header() {
        let request = parse_head(b"GET /events HTTP/1.1\r\nHost: localhost:8999\r\n\r\n").unwrap();
        assert!(validate_origin(&request, 8999).is_ok());
        let request = parse_head(
            b"GET / HTTP/1.1\r\nHost: localhost:8999\r\nOrigin: http://evil.example\r\n\r\n",
        )
        .unwrap();
        assert_eq!(validate_origin(&request, 8999).unwrap_err().0, 403);
    }

    #[test]
    fn external_links_can_open_only_the_root_document() {
        for host in ["127.0.0.1:8999", "localhost:8999"] {
            for site in ["cross-site", "same-site"] {
                for origin in [String::new(), format!("Origin: http://{host}\r\n")] {
                    let request = parse_head(
                        format!(
                            "GET / HTTP/1.1\r\nHost: {host}\r\n{origin}Sec-Fetch-Site: {site}\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                    assert!(validate_origin(&request, 8999).is_ok());
                }
            }
        }
    }

    #[test]
    fn document_navigation_never_bypasses_host_or_origin_checks() {
        for (host, origin) in [
            ("evil.example:8999", ""),
            ("127.0.0.1:8998", ""),
            ("127.0.0.1:8999", "http://evil.example"),
            ("127.0.0.1:8999", "http://localhost:8999"),
            ("127.0.0.1:8999", "http://127.0.0.1:8998"),
            ("127.0.0.1:8999", "null"),
        ] {
            let request = parse_head(
                format!(
                    "GET / HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nSec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
            assert_eq!(validate_origin(&request, 8999).unwrap_err().0, 403);
        }
    }

    #[test]
    fn cross_site_fetches_embeds_and_backend_routes_remain_forbidden() {
        for site in ["cross-site", "same-site"] {
            for metadata in [
                "",
                "Sec-Fetch-Mode: navigate\r\n",
                "Sec-Fetch-Dest: document\r\n",
                "Sec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n",
                "Sec-Fetch-Mode: no-cors\r\nSec-Fetch-Dest: empty\r\n",
                "Sec-Fetch-Mode: cors\r\nSec-Fetch-Dest: document\r\n",
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: iframe\r\n",
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: frame\r\n",
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: object\r\n",
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: embed\r\n",
            ] {
                let request = parse_head(
                    format!(
                        "GET / HTTP/1.1\r\nHost: 127.0.0.1:8999\r\nSec-Fetch-Site: {site}\r\n{metadata}\r\n"
                    )
                    .as_bytes(),
                )
                .unwrap();
                assert_eq!(validate_origin(&request, 8999).unwrap_err().0, 403);
            }
            for (method, path) in [("GET", "/events"), ("POST", "/rpc"), ("POST", "/")] {
                let request = parse_head(
                    format!(
                        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:8999\r\nOrigin: http://127.0.0.1:8999\r\nSec-Fetch-Site: {site}\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .unwrap();
                assert_eq!(validate_origin(&request, 8999).unwrap_err().0, 403);
            }
        }
    }

    #[test]
    fn ambiguous_framing_and_duplicate_security_headers_are_rejected() {
        for extra in [
            "Content-Length: 1\r\nContent-Length: 1\r\n",
            "Content-Length: -1\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Host: localhost:8999\r\n",
            "Origin: http://127.0.0.1:8999\r\nOrigin: http://evil.example\r\n",
            "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Site: same-origin\r\n",
            "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Mode: cors\r\n",
            "Sec-Fetch-Dest: document\r\nSec-Fetch-Dest: iframe\r\n",
            " Host: evil.example\r\n",
        ] {
            assert!(parse_head(
                format!("POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:8999\r\n{extra}\r\n").as_bytes()
            )
            .is_err());
        }
        assert_eq!(
            parse_head(
                format!(
                    "POST /rpc HTTP/1.1\r\nHost: localhost:8999\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY_BYTES + 1
                )
                .as_bytes()
            )
            .unwrap_err()
            .0,
            413
        );
    }

    #[tokio::test]
    async fn request_reader_preserves_body_bytes_buffered_with_headers() {
        let body = br#"{"id":1,"method":"initialize","params":{}}"#;
        let header = format!(
            "POST /rpc HTTP/1.1\r\nHost: localhost:8999\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        writer.write_all(header.as_bytes()).await.unwrap();
        writer.write_all(body).await.unwrap();
        writer.shutdown().await.unwrap();
        let request = read_request(&mut reader).await.unwrap();
        assert_eq!(request.body, body);
    }

    #[tokio::test]
    async fn request_reader_rejects_oversized_headers_and_truncated_body() {
        let input = vec![b'a'; MAX_HEADER_BYTES + 4];
        let (mut writer, mut reader) = tokio::io::duplex(input.len());
        writer.write_all(&input).await.unwrap();
        writer.shutdown().await.unwrap();
        assert_eq!(read_request(&mut reader).await.unwrap_err().0, 431);

        let (mut writer, mut reader) = tokio::io::duplex(256);
        writer
            .write_all(
                b"POST /rpc HTTP/1.1\r\nHost: localhost:8999\r\nContent-Length: 10\r\n\r\n{}",
            )
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        assert_eq!(read_request(&mut reader).await.unwrap_err().0, 400);
    }

    #[tokio::test]
    async fn transport_allows_external_document_navigation_but_rejects_fetch_and_embed() {
        for (method, path, mode, destination, status) in [
            ("GET", "/", "navigate", "document", 200),
            ("GET", "/", "cors", "empty", 403),
            ("GET", "/", "no-cors", "empty", 403),
            ("GET", "/", "navigate", "iframe", 403),
            ("GET", "/", "navigate", "object", 403),
            ("GET", "/events", "cors", "empty", 403),
            ("GET", "/events", "navigate", "document", 403),
            ("POST", "/rpc", "navigate", "document", 403),
        ] {
            let (mut client, stream) = tokio::io::duplex(4096);
            let (rpc, mut received) = mpsc::unbounded_channel();
            let (events, _) = broadcast::channel(EVENT_CAPACITY);
            let server = tokio::spawn(async move {
                handle_connection(stream, 8999, rpc, events).await.unwrap();
            });
            client
                .write_all(
                    format!(
                        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:8999\r\nSec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: {mode}\r\nSec-Fetch-Dest: {destination}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            timeout(Duration::from_secs(1), client.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            server.await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status} ")));
            if status == 200 {
                assert!(response.contains("Content-Type: text/html; charset=utf-8\r\n"));
                assert!(response.contains("Content-Security-Policy: frame-ancestors 'none'\r\n"));
            }
            assert!(received.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn rpc_transport_accepts_json_and_forwards_it_once() {
        let address = "127.0.0.1:8999";
        let (mut client, stream) = tokio::io::duplex(4096);
        let (rpc, mut received) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let server = tokio::spawn(async move {
            handle_connection(stream, 8999, rpc, events).await.unwrap();
        });
        let body = br#"{"id":3,"method":"model/list"}"#;
        client.write_all(format!("POST /rpc HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        client.write_all(body).await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 202 Accepted\r\n"));
        assert_eq!(received.recv().await.unwrap()["method"], "model/list");
        server.await.unwrap();
        assert!(received.try_recv().is_err());
    }

    #[tokio::test]
    async fn sse_uses_chunked_framing_and_forwards_complete_events() {
        let (mut client, stream) = tokio::io::duplex(4096);
        let (events, receiver) = broadcast::channel(EVENT_CAPACITY);
        events
            .send(Arc::from("data: {\"id\":3,\"result\":{}}\n\n"))
            .unwrap();
        drop(events);
        let server = tokio::spawn(stream_events(stream, receiver));
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        server.await.unwrap().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Transfer-Encoding: chunked\r\n"));
        assert!(response.contains("Content-Type: text/event-stream; charset=utf-8\r\n"));
        assert!(response.contains("data: {\"id\":3,\"result\":{}}\n\n\r\n"));
        assert!(response.ends_with("0\r\n\r\n"));
    }

    #[tokio::test]
    async fn lagged_sse_clients_close_instead_of_receiving_partial_history() {
        let (mut client, stream) = tokio::io::duplex(4096);
        let (events, receiver) = broadcast::channel(2);
        for n in 0..3 {
            events.send(Arc::from(format!("data: {n}\n\n"))).unwrap();
        }
        let server = tokio::spawn(stream_events(stream, receiver));
        let mut response = String::new();
        timeout(Duration::from_secs(1), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap().unwrap();
        assert!(!response.contains("data:"));
        assert!(response.ends_with("0\r\n\r\n"));
    }
}
