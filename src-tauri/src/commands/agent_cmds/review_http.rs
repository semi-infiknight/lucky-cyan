//! One bounded HTTP request per loopback connection. A review client never
//! holds the command listener or a desk lock while uploading a request.

use serde_json::{json, Value};
use std::{
    io::{self, ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(10);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_HEADER_BYTES: usize = 4_096;
const MAX_HEADER_COUNT: usize = 32;
// Both 2,000-byte notes can expand to six JSON bytes per byte.
const MAX_BODY_BYTES: usize = 25_600;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

type ReviewResult = Result<Value, (u16, String)>;

pub(super) fn bind() -> io::Result<TcpListener> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub(super) fn serve(
    listener: TcpListener,
    open: &AtomicBool,
    authorization: &str,
    mut dispatch: impl FnMut(&[u8]) -> ReviewResult,
) -> io::Result<()> {
    while open.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                let deadline = Deadline::new(open, REQUEST_TIMEOUT);
                if peer.ip().is_loopback() && stream.set_nonblocking(true).is_ok() {
                    let result = match read_request(&mut stream, &deadline, authorization) {
                        Ok(body) if deadline.check().is_ok() => dispatch(&body),
                        Ok(_) => Err((408, "Review request timed out".into())),
                        Err(RequestError::Rejected(status, message)) => {
                            Err((status, message.into()))
                        }
                        Err(RequestError::Io(error)) if error.kind() == ErrorKind::TimedOut => {
                            Err((408, "Review request timed out".into()))
                        }
                        Err(RequestError::Io(_)) => continue,
                    };
                    let _ = respond(&mut stream, open, result);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::Interrupted
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                ) =>
            {
                continue
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct Deadline<'a> {
    open: &'a AtomicBool,
    expires: Instant,
}

impl<'a> Deadline<'a> {
    fn new(open: &'a AtomicBool, timeout: Duration) -> Self {
        Self {
            open,
            expires: Instant::now() + timeout,
        }
    }

    fn check(&self) -> io::Result<Duration> {
        if !self.open.load(Ordering::Acquire) {
            return Err(ErrorKind::ConnectionAborted.into());
        }
        let remaining = self.expires.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        Ok(remaining)
    }

    fn io(&self, mut operation: impl FnMut() -> io::Result<usize>) -> io::Result<usize> {
        loop {
            let remaining = self.check()?;
            match operation() {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(POLL.min(remaining))
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
}

enum RequestError {
    Io(io::Error),
    Rejected(u16, &'static str),
}

impl From<io::Error> for RequestError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

const BAD_REQUEST: RequestError = RequestError::Rejected(400, "Invalid review request");
const ACCESS_DENIED: RequestError = RequestError::Rejected(403, "Review access denied");

fn framing(buf: &[u8], authorization: &str) -> Result<Option<(usize, usize)>, RequestError> {
    let mut fields = [httparse::EMPTY_HEADER; MAX_HEADER_COUNT];
    let mut request = httparse::Request::new(&mut fields);
    let end = match request.parse(buf) {
        Ok(httparse::Status::Complete(end)) => end,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(httparse::Error::TooManyHeaders) => {
            return Err(RequestError::Rejected(
                431,
                "Review request headers are too large",
            ));
        }
        Err(_) => return Err(BAD_REQUEST),
    };
    if request.version != Some(1) {
        return Err(BAD_REQUEST);
    }
    if request.method != Some("POST") || request.path != Some("/v1/review") {
        return Err(RequestError::Rejected(404, "Unknown review operation"));
    }

    let mut auth = None;
    let mut length = None;
    let mut host = false;
    for field in request.headers {
        if field.name.eq_ignore_ascii_case("authorization") {
            if auth.replace(field.value).is_some() {
                return Err(ACCESS_DENIED);
            }
        } else if field.name.eq_ignore_ascii_case("origin") {
            return Err(ACCESS_DENIED);
        } else if field.name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(BAD_REQUEST);
        } else if field.name.eq_ignore_ascii_case("expect") {
            return Err(RequestError::Rejected(
                417,
                "Send the review request body directly",
            ));
        } else if field.name.eq_ignore_ascii_case("host") {
            if host || field.value.is_empty() {
                return Err(BAD_REQUEST);
            }
            host = true;
        } else if field.name.eq_ignore_ascii_case("content-length") {
            if length.is_some()
                || field.value.is_empty()
                || !field.value.iter().all(u8::is_ascii_digit)
            {
                return Err(BAD_REQUEST);
            }
            length = std::str::from_utf8(field.value)
                .ok()
                .and_then(|value| value.parse::<usize>().ok());
            if length.is_none() {
                return Err(RequestError::Rejected(
                    413,
                    "Review request body is too large",
                ));
            }
        }
    }
    if auth != Some(authorization.as_bytes()) {
        return Err(ACCESS_DENIED);
    }
    if !host {
        return Err(BAD_REQUEST);
    }
    let length = length.ok_or(RequestError::Rejected(
        411,
        "Send a fixed review request length",
    ))?;
    if !(1..=MAX_BODY_BYTES).contains(&length) {
        return Err(RequestError::Rejected(
            413,
            "Review request size is outside the allowed range",
        ));
    }
    Ok(Some((end, length)))
}

fn read_request(
    stream: &mut TcpStream,
    deadline: &Deadline<'_>,
    authorization: &str,
) -> Result<Vec<u8>, RequestError> {
    let mut header = [0; MAX_HEADER_BYTES];
    let mut received = 0;
    let (end, length) = loop {
        if let Some(frame) = framing(&header[..received], authorization)? {
            break frame;
        }
        if received == header.len() {
            return Err(RequestError::Rejected(
                431,
                "Review request headers are too large",
            ));
        }
        let bytes = deadline.io(|| stream.read(&mut header[received..]))?;
        if bytes == 0 {
            return Err(io::Error::from(ErrorKind::UnexpectedEof).into());
        }
        received += bytes;
    };

    let already_read = received - end;
    if already_read > length {
        return Err(BAD_REQUEST);
    }
    let mut body = vec![0; length];
    body[..already_read].copy_from_slice(&header[end..received]);
    let mut received = already_read;
    while received < body.len() {
        let bytes = deadline.io(|| stream.read(&mut body[received..]))?;
        if bytes == 0 {
            return Err(io::Error::from(ErrorKind::UnexpectedEof).into());
        }
        received += bytes;
    }
    Ok(body)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        413 => "Content Too Large",
        417 => "Expectation Failed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

#[derive(Default)]
struct ResponseBody(Vec<u8>);

impl Write for ResponseBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESPONSE_BYTES - self.0.len() {
            return Err(ErrorKind::InvalidData.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn respond(stream: &mut TcpStream, open: &AtomicBool, result: ReviewResult) -> io::Result<()> {
    let (mut status, value) = match result {
        Ok(value) => (200, value),
        Err((status, message)) => (status, json!({"error": message})),
    };
    let mut body = ResponseBody::default();
    if serde_json::to_writer(&mut body, &value).is_err() {
        status = 413;
        body.0 = br#"{"error":"Review response is too large"}"#.to_vec();
    }
    let header = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status), body.0.len(),
    );
    let deadline = Deadline::new(open, RESPONSE_TIMEOUT);
    for mut bytes in [header.as_bytes(), body.0.as_slice()] {
        while !bytes.is_empty() {
            let written = deadline.io(|| stream.write(bytes))?;
            if written == 0 {
                return Err(ErrorKind::WriteZero.into());
            }
            bytes = &bytes[written..];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::SocketAddr,
        sync::{atomic::AtomicUsize, Arc},
        thread::JoinHandle,
    };

    const AUTH: &str = "Bearer rehearsal-review";

    struct TestServer {
        address: SocketAddr,
        open: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestServer {
        fn start(dispatch: impl FnMut(&[u8]) -> ReviewResult + Send + 'static) -> Self {
            let listener = bind().unwrap();
            let address = listener.local_addr().unwrap();
            let open = Arc::new(AtomicBool::new(true));
            let worker_open = Arc::clone(&open);
            let thread =
                thread::spawn(move || serve(listener, &worker_open, AUTH, dispatch).unwrap());
            Self {
                address,
                open,
                thread: Some(thread),
            }
        }

        fn revoke(&mut self) {
            self.open.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }

        fn connect(&self) -> TcpStream {
            connect(self.address)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.revoke();
        }
    }

    fn connect(address: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
    }

    fn request(method_and_path: &str, auth: &str, headers: &str, body: &str) -> String {
        format!("{method_and_path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {auth}\r\n{headers}\r\n{body}")
    }

    fn exchange(
        server: &TestServer,
        method_and_path: &str,
        auth: &str,
        headers: &str,
        body: &str,
    ) -> String {
        let mut client = server.connect();
        client
            .write_all(request(method_and_path, auth, headers, body).as_bytes())
            .unwrap();
        let mut reader = BufReader::new(client);
        let mut response = String::new();
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            response.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = Some(value.trim().parse::<u64>().unwrap());
            }
        }
        let length = length.unwrap();
        assert_eq!(
            reader.take(length).read_to_string(&mut response).unwrap() as u64,
            length
        );
        response
    }

    #[test]
    fn only_authenticated_fixed_length_review_reaches_dispatch() {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&count);
        let server = TestServer::start(move |body| {
            assert_eq!(body, br#"{"command":"lists"}"#);
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(json!({"workspace_id":"rehearsal", "lists":[]}))
        });
        let body = r#"{"command":"lists"}"#;
        let sized = format!("Content-Length: {}\r\n", body.len());
        let good = exchange(&server, "POST /v1/review", AUTH, &sized, body);
        assert!(good.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(good.contains("Cache-Control: no-store\r\n"));
        let (_, response) = good.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(response).unwrap(),
            json!({"workspace_id":"rehearsal", "lists":[]})
        );

        for credential in ["Bearer operator", "Bearer monitor", "Bearer expired-review"] {
            assert!(
                exchange(&server, "POST /v1/review", credential, &sized, body)
                    .starts_with("HTTP/1.1 403 ")
            );
        }
        for method_and_path in [
            "GET /v1/review",
            "POST /v1/command",
            "POST /v1/review?ignored",
        ] {
            assert!(
                exchange(&server, method_and_path, AUTH, &sized, body).starts_with("HTTP/1.1 404 ")
            );
        }
        let extra_header = |header: &str| format!("{sized}{header}\r\n");
        for (headers, status) in [
            (extra_header("Authorization: Bearer rehearsal-review"), 403),
            (extra_header("Origin: http://localhost"), 403),
            (extra_header("Transfer-Encoding: chunked"), 400),
            (extra_header("Content-Length: 19"), 400),
            (extra_header("Expect: 100-continue"), 417),
            ("".into(), 411),
            (format!("Content-Length: {}\r\n", MAX_BODY_BYTES + 1), 413),
            (
                format!("X-Large: {}\r\n", "a".repeat(MAX_HEADER_BYTES)),
                431,
            ),
        ] {
            let response = exchange(&server, "POST /v1/review", AUTH, &headers, "");
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status} ")),
                "{response}"
            );
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_trickling_body_expires_while_the_command_listener_replies() {
        let review = TestServer::start(|_| panic!("partial review upload reached dispatch"));
        let mut upload = review.connect();
        upload
            .write_all(request("POST /v1/review", AUTH, "Content-Length: 128\r\n", "{").as_bytes())
            .unwrap();
        let started = Instant::now();
        let mut trickle = upload.try_clone().unwrap();
        let sender = thread::spawn(move || {
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(45));
                if trickle.write_all(b" ").is_err() {
                    break;
                }
            }
        });

        let command_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let command_address = command_listener.local_addr().unwrap();
        let command_server = tiny_http::Server::from_listener(command_listener, None).unwrap();
        let command_worker = thread::spawn(move || {
            let mut request = command_server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/v1/command");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(body, r#"{"command":"live_status"}"#);
            request
                .respond(tiny_http::Response::from_string("desk status"))
                .unwrap();
        });
        let mut command = connect(command_address);
        let status = r#"{"command":"live_status"}"#;
        command.write_all(format!(
            "POST /v1/command HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{status}",
            status.len(),
        ).as_bytes()).unwrap();
        let mut response = String::new();
        command.read_to_string(&mut response).unwrap();
        assert!(response.ends_with("desk status"));
        command_worker.join().unwrap();
        assert!(started.elapsed() < REQUEST_TIMEOUT);

        let mut timeout = String::new();
        BufReader::new(upload).read_line(&mut timeout).unwrap();
        assert!(timeout.starts_with("HTTP/1.1 408 "));
        assert!(started.elapsed() < REQUEST_TIMEOUT + Duration::from_millis(450));
        sender.join().unwrap();
    }

    #[test]
    fn revocation_drains_a_partial_connection_and_refuses_new_clients() {
        let mut review = TestServer::start(|_| panic!("revoked review request reached dispatch"));
        let mut upload = review.connect();
        upload
            .write_all(request("POST /v1/review", AUTH, "Content-Length: 128\r\n", "{").as_bytes())
            .unwrap();
        thread::sleep(POLL * 2);
        let started = Instant::now();
        review.revoke();
        assert!(started.elapsed() < REQUEST_TIMEOUT / 2);
        assert!(TcpStream::connect(review.address).is_err());
        assert!(matches!(upload.read(&mut [0]), Ok(0) | Err(_)));
    }

    #[test]
    fn oversized_replies_are_replaced_with_a_small_error() {
        let server = TestServer::start(|_| Ok(json!({"note": "n".repeat(MAX_RESPONSE_BYTES)})));
        let reply = exchange(
            &server,
            "POST /v1/review",
            AUTH,
            "Content-Length: 2\r\n",
            "{}",
        );
        assert!(reply.starts_with("HTTP/1.1 413 "));
        assert!(reply.ends_with(r#"{"error":"Review response is too large"}"#));
    }
}
