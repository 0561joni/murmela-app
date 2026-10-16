//! The local endpoint the Murmela browser helper reports active speakers to.
//!
//! It listens on 127.0.0.1 only, and only while a call is being recorded. It
//! accepts requests from browser extensions (whose `Origin` web pages cannot
//! forge) or without an `Origin`, and refuses web pages. It takes speaker
//! names and timings; nothing it receives leaves this computer, and only the
//! names end up in the transcript.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};

use super::{BrowserReport, MeetingController};

/// The browser helper tries these ports in order.
pub const PORTS: [u16; 5] = [47615, 47616, 47617, 47618, 47619];
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 128 * 1024;
const MAX_CONNECTIONS: usize = 4;

pub struct Bridge {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Bridge {
    pub fn start(controller: MeetingController) -> Result<Self> {
        Self::start_on(controller, &PORTS)
    }

    fn start_on(controller: MeetingController, ports: &[u16]) -> Result<Self> {
        let listener = ports
            .iter()
            .find_map(|port| TcpListener::bind(("127.0.0.1", *port)).ok())
            .ok_or_else(|| anyhow!("none of the browser helper ports is free"))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let thread = thread::Builder::new()
            .name("murmela-call-bridge".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                                continue;
                            }
                            connections.fetch_add(1, Ordering::SeqCst);
                            let controller = controller.clone();
                            let connections = connections.clone();
                            let _ = thread::Builder::new()
                                .name("murmela-call-bridge-conn".into())
                                .spawn(move || {
                                    let _ = handle(stream, &controller);
                                    connections.fetch_sub(1, Ordering::SeqCst);
                                });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(200)),
                    }
                }
            })?;
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    origin: Option<String>,
    body: Vec<u8>,
}

fn handle(stream: TcpStream, controller: &MeetingController) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(_) => return respond(&mut writer, 400, None, "{\"error\":\"bad request\"}"),
    };
    if !allowed_origin(request.origin.as_deref()) {
        return respond(&mut writer, 403, None, "{\"error\":\"forbidden\"}");
    }
    let origin = request.origin.as_deref();
    match (request.method.as_str(), request.path.as_str()) {
        ("OPTIONS", _) => respond(&mut writer, 204, origin, ""),
        ("GET", "/v1/status") => {
            let body = serde_json::json!({
                "app": "murmela",
                "version": 1,
                "recording": controller.is_recording(),
            });
            respond(&mut writer, 200, origin, &body.to_string())
        }
        ("POST", "/v1/report") => {
            let report: BrowserReport = match serde_json::from_slice(&request.body) {
                Ok(report) => report,
                Err(_) => {
                    return respond(&mut writer, 400, origin, "{\"error\":\"invalid report\"}")
                }
            };
            let accepted = controller.receive_browser_report(report);
            respond(
                &mut writer,
                200,
                origin,
                &serde_json::json!({ "accepted": accepted }).to_string(),
            )
        }
        _ => respond(&mut writer, 404, origin, "{\"error\":\"not found\"}"),
    }
}

/// Extensions only. A missing Origin is a local tool or an extension that omits it.
fn allowed_origin(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(origin) => [
            "chrome-extension://",
            "moz-extension://",
            "safari-web-extension://",
        ]
        .iter()
        .any(|scheme| origin.starts_with(scheme)),
    }
}

fn read_request(stream: TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut header_bytes = 0;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    header_bytes += line.len();
    let mut parts = line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("no method"))?
        .to_string();
    let path = parts.next().ok_or_else(|| anyhow!("no path"))?.to_string();
    let mut origin = None;
    let mut length = 0usize;
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        header_bytes += read;
        if read == 0 || header_bytes > MAX_HEADER_BYTES {
            return Err(anyhow!("headers too long or cut off"));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "origin" => origin = Some(value.trim().to_string()),
                "content-length" => length = value.trim().parse()?,
                _ => {}
            }
        }
    }
    if length > MAX_BODY_BYTES {
        return Err(anyhow!("body too large"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method,
        path,
        origin,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, origin: Option<&str>, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(origin) = origin {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Allow-Private-Network: true\r\nVary: Origin\r\n"
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn post(port: u16, origin: Option<&str>, body: &str) -> String {
        let origin = origin
            .map(|origin| format!("Origin: {origin}\r\n"))
            .unwrap_or_default();
        exchange(
            port,
            &format!(
                "POST /v1/report HTTP/1.1\r\nHost: 127.0.0.1\r\n{origin}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    #[test]
    fn extensions_are_answered_and_web_pages_refused() {
        let dir = std::env::temp_dir().join(format!("murmela-bridge-{}", uuid::Uuid::new_v4()));
        let controller = MeetingController::new(dir.clone());
        // Port 0: any free port, so tests never clash with a running Murmela.
        let bridge = Bridge::start_on(controller, &[0]).unwrap();
        let port = bridge.port();

        let status = exchange(
            port,
            "GET /v1/status HTTP/1.1\r\nOrigin: chrome-extension://abcdefghijklmnop\r\n\r\n",
        );
        assert!(status.starts_with("HTTP/1.1 200"));
        assert!(status.contains("Access-Control-Allow-Origin: chrome-extension://abcdefghijklmnop"));
        assert!(status.contains("\"recording\":false"));

        let body = r#"{"platform":"meet","events":[{"t":1,"name":"Ada","kind":"speaking"}]}"#;
        assert!(post(port, Some("https://evil.example"), body).starts_with("HTTP/1.1 403"));
        assert!(post(port, Some("null"), body).starts_with("HTTP/1.1 403"));
        let accepted = post(port, Some("moz-extension://1234"), body);
        assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");
        assert!(
            accepted.contains("\"accepted\":false"),
            "nothing is recording"
        );
        assert!(post(port, None, "not json").starts_with("HTTP/1.1 400"));

        let preflight = exchange(
            port,
            "OPTIONS /v1/report HTTP/1.1\r\nOrigin: chrome-extension://x\r\n\r\n",
        );
        assert!(preflight.starts_with("HTTP/1.1 204"));
        assert!(preflight.contains("Access-Control-Allow-Methods"));

        let oversized = format!(
            "POST /v1/report HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert!(exchange(port, &oversized).starts_with("HTTP/1.1 400"));

        drop(bridge);
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "the port closes with the bridge"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
