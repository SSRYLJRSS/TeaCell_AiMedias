//! Cancellable production transport for the A3-02 model gateway.
//!
//! The transport deliberately accepts only the already-validated Rust request
//! shape. It never accepts a URL, headers, proxy settings, or credentials from
//! the Agent Host; the proxy is Rust-owned user configuration. Environment
//! proxies and redirects are disabled so an upstream cannot silently leave the
//! approved connection target.

use std::future::Future;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use super::gateway::{
    GatewayTransport, GatewayTransportResponse, GatewayUpstreamRequest, Route, MAX_RESPONSE_BYTES,
};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHANNEL_WAIT: Duration = Duration::from_secs(1);

/// 校验用户填写的 Agent 代理地址并返回规范形式（去掉结尾斜杠）。
///
/// 只接受 `http(s)://[host][:port]`：不接受用户名密码（会进日志/错误信息）、
/// query、fragment 和路径。`socks5://` 必须拒绝——`reqwest` 没有启用 `socks`
/// 特性，接受它只会在运行时静默失败。
pub fn validate_agent_proxy_url(value: &str) -> Result<String, &'static str> {
    let url = reqwest::Url::parse(value).map_err(|_| "PROXY_INVALID")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("PROXY_INVALID");
    }
    let path = url.path();
    if !(path.is_empty() || path == "/") {
        return Err("PROXY_INVALID");
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

/// 空串表示直连；非空则必须是合法的显式代理。
fn optional_agent_proxy(proxy: &str) -> Result<Option<String>, &'static str> {
    let trimmed = proxy.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(None);
    }
    validate_agent_proxy_url(trimmed).map(Some)
}

/// HTTP transport used by the production gateway once external routing and
/// cost policy have been approved. It is kept behind the transport seam so
/// tests can use deterministic in-memory fixtures without network access.
pub struct HttpGatewayTransport {
    client: reqwest::Client,
    timeout: Duration,
}

impl HttpGatewayTransport {
    pub fn new(timeout: Duration, proxy: Option<&str>) -> Result<Self, &'static str> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            // 先清空环境变量代理（并关掉自动探测），需要时再加显式代理：
            // 读系统/环境代理属于另一张卡的范围，这里必须“配置什么就用什么”。
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout);
        if let Some(proxy) = proxy {
            let proxy = reqwest::Proxy::all(proxy).map_err(|_| "PROXY_INVALID")?;
            builder = builder.proxy(proxy);
        }
        let client = builder.build().map_err(|_| "CLIENT_FAILED")?;
        Ok(Self { client, timeout })
    }
}

/// 生产外发传输：代理可在运行中切换，不需要重启 Host。
///
/// 这里没有第二道「允许联网」开关：外发是否就绪由绑定服务与服务凭据决定
/// （判定在 `GatewayBroker::preflight`），配置好就能直接对话。在途 attempt
/// 持有旧的 `Arc<HttpGatewayTransport>`，会照常跑完或被取消；新的 attempt 用新
/// 代理。代理是 Rust 侧用户配置，Host 不可见。
pub struct SwitchableGatewayTransport {
    inner: RwLock<Arc<HttpGatewayTransport>>,
    timeout: Duration,
}

impl SwitchableGatewayTransport {
    pub fn new(proxy: &str, timeout: Duration) -> Result<Self, &'static str> {
        let proxy = optional_agent_proxy(proxy)?;
        let transport = HttpGatewayTransport::new(timeout, proxy.as_deref())?;
        Ok(Self {
            inner: RwLock::new(Arc::new(transport)),
            timeout,
        })
    }

    /// 先构建新 client，成功后再原子替换；失败时旧状态完全不变。
    pub fn reconfigure(&self, proxy: &str) -> Result<(), &'static str> {
        let proxy = optional_agent_proxy(proxy)?;
        // 构建 client 不需要锁：避免持写锁做重活，也避免与在途 open 抢锁。
        let transport = HttpGatewayTransport::new(self.timeout, proxy.as_deref())?;
        *self.inner.write().map_err(|_| "CLIENT_FAILED")? = Arc::new(transport);
        Ok(())
    }
}

impl GatewayTransport for SwitchableGatewayTransport {
    fn enabled(&self) -> bool {
        true
    }

    fn open(
        &self,
        request: GatewayUpstreamRequest,
    ) -> Result<GatewayTransportResponse, &'static str> {
        // 只克隆 Arc，读锁随后立即释放：在途请求不再持锁，reconfigure 不会被拖住。
        let inner = {
            let guard = self.inner.read().map_err(|_| "CLIENT_FAILED")?;
            Arc::clone(&guard)
        };
        inner.open(request)
    }
}

struct Head {
    status: u16,
    content_type: String,
}

impl GatewayTransport for HttpGatewayTransport {
    fn open(
        &self,
        request: GatewayUpstreamRequest,
    ) -> Result<GatewayTransportResponse, &'static str> {
        let attempt_cancelled = Arc::clone(&request.cancelled);
        let transport_cancelled = Arc::new(AtomicBool::new(false));
        let (head_tx, head_rx) = mpsc::sync_channel(1);
        let (body_tx, body_rx) = mpsc::sync_channel(1);
        let error = Arc::new(Mutex::new(None));
        let head_sent = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&transport_cancelled);
        let worker_attempt_cancelled = Arc::clone(&attempt_cancelled);
        let worker_error = Arc::clone(&error);
        let worker_head_sent = Arc::clone(&head_sent);
        let client = self.client.clone();

        thread::Builder::new()
            .name("agent-gateway-http".to_string())
            .spawn(move || {
                let result = run_http(
                    client,
                    request,
                    &worker_attempt_cancelled,
                    &worker_cancelled,
                    &head_tx,
                    &body_tx,
                    &worker_head_sent,
                );
                if let Err(code) = result {
                    if !worker_head_sent.load(Ordering::Acquire) {
                        let _ = head_tx.try_send(Err(code));
                    }
                    if let Ok(mut slot) = worker_error.lock() {
                        *slot = Some(code);
                    }
                }
            })
            .map_err(|_| "UPSTREAM_UNAVAILABLE")?;

        // The header wait stays polled so a cancel or lease revocation during
        // connect/TTFB releases the broker worker immediately instead of after
        // the full HTTP timeout. The extra channel wait remains only as the
        // worker's shutdown grace period.
        let head_deadline = Instant::now() + self.timeout.saturating_add(CHANNEL_WAIT);
        let head = loop {
            if is_cancelled(&attempt_cancelled, &transport_cancelled) {
                transport_cancelled.store(true, Ordering::Release);
                return Err("CANCELLED");
            }
            match head_rx.recv_timeout(POLL_INTERVAL) {
                Ok(Ok(head)) => break head,
                Ok(Err(code)) => {
                    transport_cancelled.store(true, Ordering::Release);
                    return Err(code);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= head_deadline {
                        transport_cancelled.store(true, Ordering::Release);
                        return Err("TIMEOUT");
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    transport_cancelled.store(true, Ordering::Release);
                    return Err("UPSTREAM_UNAVAILABLE");
                }
            }
        };

        let cancel_attempt = Arc::clone(&attempt_cancelled);
        let cancel_transport = Arc::clone(&transport_cancelled);
        Ok(GatewayTransportResponse {
            status: head.status,
            content_type: head.content_type,
            body: Box::new(HttpResponseBody {
                receiver: body_rx,
                error,
                attempt_cancelled,
                transport_cancelled,
                current: Vec::new(),
                offset: 0,
            }),
            cancel: Arc::new(move || {
                cancel_attempt.store(true, Ordering::Release);
                cancel_transport.store(true, Ordering::Release);
            }),
        })
    }
}

fn run_http(
    client: reqwest::Client,
    request: GatewayUpstreamRequest,
    attempt_cancelled: &AtomicBool,
    transport_cancelled: &AtomicBool,
    head_tx: &SyncSender<Result<Head, &'static str>>,
    body_tx: &SyncSender<Vec<u8>>,
    head_sent: &AtomicBool,
) -> Result<(), &'static str> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "UPSTREAM_UNAVAILABLE")?;
    runtime.block_on(async {
        let mut builder = client
            .post(&request.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(request.body);
        match request.route {
            Route::OpenAiChat => {
                if let Some(authorization) = request.authorization {
                    builder = builder.header(reqwest::header::AUTHORIZATION, authorization);
                }
            }
            Route::AnthropicMessages => {
                if let Some(api_key) = request.authorization {
                    builder = builder.header("x-api-key", api_key);
                }
                if let Some(version) = request.anthropic_version {
                    builder = builder.header("anthropic-version", version);
                }
            }
        }

        let mut response = await_cancelled(builder.send(), attempt_cancelled, transport_cancelled)
            .await?
            .map_err(|error| {
                if error.is_timeout() {
                    "TIMEOUT"
                } else {
                    "UPSTREAM_UNAVAILABLE"
                }
            })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        head_tx
            .send(Ok(Head {
                status,
                content_type,
            }))
            .map_err(|_| "CANCELLED")?;
        head_sent.store(true, Ordering::Release);

        if !(200..300).contains(&status) {
            return Ok(());
        }

        let mut total = 0usize;
        loop {
            let chunk = await_cancelled(response.chunk(), attempt_cancelled, transport_cancelled)
                .await?
                .map_err(|_| "STREAM_FAILED")?;
            let Some(chunk) = chunk else {
                return Ok(());
            };
            if chunk.is_empty() {
                continue;
            }
            total = total.saturating_add(chunk.len());
            if total > MAX_RESPONSE_BYTES {
                return Err("STREAM_FAILED");
            }
            send_chunk(
                body_tx,
                chunk.to_vec(),
                attempt_cancelled,
                transport_cancelled,
            )?;
        }
    })
}

async fn await_cancelled<F, T>(
    future: F,
    attempt_cancelled: &AtomicBool,
    transport_cancelled: &AtomicBool,
) -> Result<T, &'static str>
where
    F: Future<Output = T>,
{
    let mut future = std::pin::pin!(future);
    loop {
        if is_cancelled(attempt_cancelled, transport_cancelled) {
            return Err("CANCELLED");
        }
        match tokio::time::timeout(POLL_INTERVAL, future.as_mut()).await {
            Ok(value) => return Ok(value),
            Err(_) => continue,
        }
    }
}

fn is_cancelled(attempt_cancelled: &AtomicBool, transport_cancelled: &AtomicBool) -> bool {
    attempt_cancelled.load(Ordering::Acquire) || transport_cancelled.load(Ordering::Acquire)
}

fn send_chunk(
    sender: &SyncSender<Vec<u8>>,
    mut chunk: Vec<u8>,
    attempt_cancelled: &AtomicBool,
    transport_cancelled: &AtomicBool,
) -> Result<(), &'static str> {
    loop {
        if is_cancelled(attempt_cancelled, transport_cancelled) {
            return Err("CANCELLED");
        }
        match sender.try_send(chunk) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(value)) => {
                chunk = value;
                thread::sleep(POLL_INTERVAL);
            }
            Err(TrySendError::Disconnected(_)) => return Err("CANCELLED"),
        }
    }
}

struct HttpResponseBody {
    receiver: Receiver<Vec<u8>>,
    error: Arc<Mutex<Option<&'static str>>>,
    attempt_cancelled: Arc<AtomicBool>,
    transport_cancelled: Arc<AtomicBool>,
    current: Vec<u8>,
    offset: usize,
}

impl Read for HttpResponseBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.offset < self.current.len() {
                let available = &self.current[self.offset..];
                let count = available.len().min(buffer.len());
                buffer[..count].copy_from_slice(&available[..count]);
                self.offset += count;
                return Ok(count);
            }
            if is_cancelled(&self.attempt_cancelled, &self.transport_cancelled) {
                return Ok(0);
            }
            match self.receiver.recv_timeout(POLL_INTERVAL) {
                Ok(chunk) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // A cancel makes the worker drop the body sender, so the
                    // disconnect and the cancel flag race: whichever the reader
                    // observes first must still end the stream as a clean EOF
                    // instead of surfacing "CANCELLED" as an IO error.
                    if is_cancelled(&self.attempt_cancelled, &self.transport_cancelled) {
                        return Ok(0);
                    }
                    if let Ok(error) = self.error.lock() {
                        if let Some(code) = *error {
                            return Err(io::Error::other(code));
                        }
                    }
                    return Ok(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn validate_agent_proxy_url_accepts_host_port_and_strips_trailing_slash() {
        assert_eq!(
            validate_agent_proxy_url("http://127.0.0.1:7890").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            validate_agent_proxy_url("https://proxy.example.test:8443/").unwrap(),
            "https://proxy.example.test:8443"
        );
        assert_eq!(
            validate_agent_proxy_url("http://proxy.example.test").unwrap(),
            "http://proxy.example.test"
        );
    }

    #[test]
    fn validate_agent_proxy_url_rejects_credentials_path_query_fragment_and_socks() {
        for value in [
            "socks5://127.0.0.1:1080",
            "ftp://proxy.example.test",
            "http://user:pw@proxy.example.test",
            "http://user@proxy.example.test",
            "http://proxy.example.test:8080/path",
            "http://proxy.example.test:8080?x=1",
            "http://proxy.example.test:8080#frag",
            "not-a-url",
            "",
        ] {
            assert_eq!(
                validate_agent_proxy_url(value),
                Err("PROXY_INVALID"),
                "value should be rejected: {value:?}"
            );
        }
    }

    fn serve_parts(
        head: Vec<u8>,
        body: Vec<u8>,
        delay_before_body: Duration,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("loopback address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept loopback");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            let mut request = [0u8; 8192];
            let length = stream.read(&mut request).expect("read request");
            stream.write_all(&head).expect("write response head");
            stream.flush().expect("flush response head");
            if !delay_before_body.is_zero() {
                thread::sleep(delay_before_body);
            }
            stream.write_all(&body).expect("write response body");
            stream.flush().expect("flush response body");
            String::from_utf8_lossy(&request[..length]).into_owned()
        });
        (format!("http://{address}/v1/chat/completions"), handle)
    }

    fn serve_silent(delay: Duration) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("loopback address");
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0u8; 8192];
                let _ = stream.read(&mut request);
                thread::sleep(delay);
            }
        });
        (format!("http://{address}/v1/chat/completions"), handle)
    }

    fn request(endpoint: String, cancelled: Arc<AtomicBool>) -> GatewayUpstreamRequest {
        GatewayUpstreamRequest {
            route: Route::OpenAiChat,
            endpoint,
            model: "fixture-model".to_string(),
            body: br#"{"model":"fixture-model","messages":[],"stream":true}"#.to_vec(),
            authorization: Some("Bearer fixture-secret".to_string()),
            anthropic_version: None,
            cancelled,
        }
    }

    #[test]
    fn loopback_http_response_is_streamed_without_leaking_auth_into_the_host() {
        let body = b"data: {\"type\":\"message_stop\"}\n\n";
        let mut head =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: ".to_vec();
        head.extend_from_slice(body.len().to_string().as_bytes());
        head.extend_from_slice(b"\r\n\r\n");
        let (endpoint, server) = serve_parts(head, body.to_vec(), Duration::ZERO);
        let transport = HttpGatewayTransport::new(Duration::from_secs(3), None).expect("transport");
        let mut response = transport
            .open(request(endpoint, Arc::new(AtomicBool::new(false))))
            .expect("open response");

        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "text/event-stream");
        let mut received = Vec::new();
        response
            .body
            .read_to_end(&mut received)
            .expect("read response");
        assert_eq!(received, body);
        let raw_request = server.join().expect("server");
        assert!(raw_request.contains("authorization: Bearer fixture-secret"));
        assert!(raw_request.contains("\"model\":\"fixture-model\""));
    }

    #[test]
    fn cancel_hook_wakes_a_blocked_response_read() {
        let head =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4\r\n\r\n"
                .to_vec();
        let (endpoint, server) = serve_parts(head, b"data".to_vec(), Duration::from_secs(2));
        let transport = HttpGatewayTransport::new(Duration::from_secs(3), None).expect("transport");
        let response = transport
            .open(request(endpoint, Arc::new(AtomicBool::new(false))))
            .expect("open response");

        let started = Instant::now();
        let cancel = Arc::clone(&response.cancel);
        let reader = thread::spawn(move || {
            let mut body = response.body;
            let mut buffer = [0u8; 32];
            body.read(&mut buffer).expect("read after cancel")
        });
        thread::sleep(Duration::from_millis(50));
        cancel();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(reader.join().expect("reader"), 0);
        server.join().expect("server");
    }

    #[test]
    fn cancel_hook_interrupts_a_blocked_header_wait() {
        let (endpoint, server) = serve_silent(Duration::from_millis(400));
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let started = Instant::now();
        let opener = thread::spawn(move || {
            let transport =
                HttpGatewayTransport::new(Duration::from_secs(5), None).expect("transport");
            transport.open(request(endpoint, flag)).map(|_| ())
        });
        thread::sleep(Duration::from_millis(50));
        cancelled.store(true, Ordering::Release);
        let result = opener.join().expect("opener");
        assert_eq!(result.err(), Some("CANCELLED"));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().expect("server");
    }

    #[test]
    fn header_timeout_is_reported_as_timeout() {
        let (endpoint, server) = serve_silent(Duration::from_millis(500));
        let transport =
            HttpGatewayTransport::new(Duration::from_millis(120), None).expect("transport");
        let result = transport
            .open(request(endpoint, Arc::new(AtomicBool::new(false))))
            .map(|_| ());
        assert_eq!(result.err(), Some("TIMEOUT"));
        server.join().expect("server");
    }

    #[test]
    fn aborted_connection_is_reported_as_upstream_unavailable() {
        let (endpoint, server) = serve_silent(Duration::ZERO);
        let transport = HttpGatewayTransport::new(Duration::from_secs(3), None).expect("transport");
        let result = transport
            .open(request(endpoint, Arc::new(AtomicBool::new(false))))
            .map(|_| ());
        assert_eq!(result.err(), Some("UPSTREAM_UNAVAILABLE"));
        server.join().expect("server");
    }

    #[test]
    fn rate_limited_status_is_surfaced_without_streaming_the_body() {
        let body = br#"{"error":"rate limited"}"#.to_vec();
        let mut head =
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: "
                .to_vec();
        head.extend_from_slice(body.len().to_string().as_bytes());
        head.extend_from_slice(b"\r\n\r\n");
        let (endpoint, server) = serve_parts(head, body, Duration::ZERO);
        let transport = HttpGatewayTransport::new(Duration::from_secs(3), None).expect("transport");
        let mut response = transport
            .open(request(endpoint, Arc::new(AtomicBool::new(false))))
            .expect("open response");

        assert_eq!(response.status, 429);
        let mut received = Vec::new();
        response
            .body
            .read_to_end(&mut received)
            .expect("read response");
        assert!(received.is_empty());
        server.join().expect("server");
    }
}
