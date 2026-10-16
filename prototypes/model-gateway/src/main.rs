//! A0-03 transport experiment. Only a test fixture on loopback can be contacted.
//! No user configuration, credentials, DB, real media or arbitrary URL forwarding.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io::{self, BufRead, Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use teacell_model_gateway_poc::{ai_rate_limit, governance::Governance};

const FRAME_LIMIT: usize = 65536;
const CHUNK_SIZE: usize = 8192;
const DEADLINE: Duration = Duration::from_secs(8);
const BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    kind: String,
    id: u32,
    #[serde(default)]
    url: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    seq: usize,
    #[serde(default)]
    data: String,
}
struct Active {
    cancelled: Arc<AtomicBool>,
    credit: mpsc::SyncSender<()>,
    upload: mpsc::SyncSender<Frame>,
}
type Requests = Arc<Mutex<HashMap<u32, Active>>>;
type Output = Arc<Mutex<io::Stdout>>;

#[derive(Clone)]
struct FixtureTransport {
    port: u16,
    scheme: &'static str,
    ca_pem: Option<Arc<Vec<u8>>>,
    revision: u32,
}

struct RequestContext<'a> {
    frame: &'a Frame,
    transport: &'a FixtureTransport,
    out: &'a Output,
    cancelled: &'a AtomicBool,
    credits: &'a mpsc::Receiver<()>,
    governance: &'a Governance,
    deadline: Instant,
}

impl RequestContext<'_> {
    fn check(&self) -> Result<(), &'static str> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("CANCELLED");
        }
        if self.frame.revision != self.transport.revision {
            return Err("CONNECTION_CHANGED");
        }
        self.governance.check(self.frame.revision)?;
        if Instant::now() >= self.deadline {
            return Err("TIMEOUT");
        }
        Ok(())
    }
    async fn wait<F: Future>(&self, future: F) -> Result<F::Output, &'static str> {
        let mut future = std::pin::pin!(future);
        loop {
            self.check()?;
            // Keep the same pinned future while polling control; elapsed polls
            // never resubmit HTTP or discard an in-progress response read.
            if let Ok(result) =
                tokio::time::timeout(Duration::from_millis(20), future.as_mut()).await
            {
                self.check()?;
                return Ok(result);
            }
        }
    }
    async fn credit(&self) -> Result<(), &'static str> {
        loop {
            self.check()?;
            match self.credits.try_recv() {
                Ok(()) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(mpsc::TryRecvError::Disconnected) => return Err("CANCELLED"),
            }
        }
    }
}

fn emit(out: &Output, value: Value) {
    let mut out = out.lock().expect("stdout lock");
    // Only fixed metadata, fixture content and sanitized machine errors are emitted.
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn execute(
    frame: &Frame,
    out: &Output,
    cancelled: &AtomicBool,
    credits: &mpsc::Receiver<()>,
    attempts: &AtomicUsize,
    governance: &Governance,
    transport: &FixtureTransport,
) -> Result<(), &'static str> {
    if frame.body.len() > BODY_LIMIT {
        return Err("BODY_LIMIT");
    }
    let route = match frame.url.as_str() {
        "https://teacell-gateway.invalid/v1/chat/completions" => "/v1/chat/completions",
        "https://teacell-gateway.invalid/v1/messages"
        | "https://teacell-gateway.invalid/v1/messages?beta=true" => "/v1/messages",
        _ => return Err("ROUTE_DENIED"),
    };
    if frame.method != "POST" || frame.revision != transport.revision {
        return Err("CONNECTION_CHANGED");
    }
    let body: Value = serde_json::from_str(&frame.body).map_err(|_| "INVALID_BODY")?;
    if body.get("model").and_then(Value::as_str) != Some("mock") {
        return Err("MODEL_DENIED");
    }
    Governance::check_text_only(&body)?;
    governance.check(frame.revision)?;
    let deadline = Instant::now() + DEADLINE;
    let limiter =
        ai_rate_limit::for_connection("fixture", 1, 100, 100).ok_or("LIMITER_UNAVAILABLE")?;
    let _permit = limiter
        .acquire_until(cancelled, Some(deadline))
        .map_err(|e| e.code())?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("CANCELLED");
    }
    governance.check(frame.revision)?;
    attempts
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            (n < 16).then_some(n + 1)
        })
        .map_err(|_| "REQUEST_BUDGET")?;
    emit(out, json!({"kind":"attempt", "id":frame.id}));
    let context = RequestContext {
        frame,
        transport,
        out,
        cancelled,
        credits,
        governance,
        deadline,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "RUNTIME_FAILED")?;
    runtime.block_on(execute_http(&context, route))
}

async fn execute_http(context: &RequestContext<'_>, route: &str) -> Result<(), &'static str> {
    let frame = context.frame;
    let port = context.transport.port;
    let scheme = context.transport.scheme;
    let out = context.out;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Even protocol-safe client retries would bypass our per-attempt ledger.
        // Explicit retries belong to the governed SDK/user retry path only.
        .retry(reqwest::retry::never())
        .no_proxy()
        .timeout(Duration::from_secs(3));
    if let Some(ca_pem) = &context.transport.ca_pem {
        let certificate = reqwest::Certificate::from_pem(ca_pem).map_err(|_| "TLS_CA_INVALID")?;
        builder = builder.add_root_certificate(certificate);
    }
    let client = builder.build().map_err(|_| "CLIENT_FAILED")?;
    let mut request = client
        .post(format!("{scheme}://127.0.0.1:{port}{route}"))
        .header("content-type", "application/json")
        .body(frame.body.clone());
    // This is a public fixture value compiled only into this experiment, not a real API key.
    request = if route.ends_with("messages") {
        request
            .header("x-api-key", "poc-rust-only-key")
            .header("anthropic-version", "2023-06-01")
    } else {
        request.bearer_auth("poc-rust-only-key")
    };
    let mut response = context
        .wait(request.send())
        .await?
        .map_err(|_| "UPSTREAM_FAILED")?;
    if response.status().is_redirection() {
        return Err("REDIRECT_DENIED");
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    emit(
        out,
        json!({"kind":"head", "id":frame.id, "status":response.status().as_u16(),
        "headers":{"content-type":content_type}}),
    );
    let mut total = 0;
    loop {
        context.credit().await?;
        let bytes = loop {
            match context
                .wait(response.chunk())
                .await?
                .map_err(|_| "STREAM_FAILED")?
            {
                Some(bytes) if !bytes.is_empty() => break bytes,
                Some(_) => continue, // An empty transport frame must not consume a UI pull.
                None => return Ok(()),
            }
        };
        total += bytes.len();
        if total > 1024 * 1024 {
            return Err("RESPONSE_LIMIT");
        }
        for (index, part) in bytes.chunks(CHUNK_SIZE).enumerate() {
            if index > 0 {
                context.credit().await?;
            }
            context.check()?;
            emit(
                out,
                json!({"kind":"chunk","id":frame.id,"data":STANDARD.encode(part)}),
            );
        }
    }
}

fn receive_body(
    frame: &mut Frame,
    uploads: &mpsc::Receiver<Frame>,
    cancelled: &AtomicBool,
    out: &Output,
    governance: &Governance,
) -> Result<(), &'static str> {
    if frame.kind == "start" {
        return Ok(());
    }
    if frame.length == 0 || frame.length > BODY_LIMIT || !frame.body.is_empty() {
        return Err("BODY_LIMIT");
    }
    let deadline = Instant::now() + DEADLINE;
    let mut bytes = Vec::with_capacity(frame.length);
    let mut seq = 0;
    emit(out, json!({"kind":"uploaded","id":frame.id,"seq":seq}));
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("CANCELLED");
        }
        governance.check(frame.revision)?;
        if Instant::now() >= deadline {
            return Err("UPLOAD_TIMEOUT");
        }
        let part = match uploads.recv_timeout(Duration::from_millis(20)) {
            Ok(part) => part,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return Err("UPLOAD_TRUNCATED"),
        };
        if part.seq != seq {
            return Err("UPLOAD_SEQUENCE");
        }
        if part.kind == "commit" {
            if bytes.len() != frame.length || !part.data.is_empty() {
                return Err("UPLOAD_LENGTH");
            }
            frame.body = String::from_utf8(bytes).map_err(|_| "INVALID_BODY")?;
            return Ok(());
        }
        let chunk = STANDARD.decode(&part.data).map_err(|_| "INVALID_BODY")?;
        if chunk.is_empty() || chunk.len() > 16384 || bytes.len() + chunk.len() > frame.length {
            return Err("UPLOAD_LENGTH");
        }
        bytes.extend_from_slice(&chunk);
        seq += 1;
        emit(out, json!({"kind":"uploaded","id":frame.id,"seq":seq}));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() > 5 {
        return Err("too many fixture arguments".into());
    }
    let port: u16 = args.first().ok_or("fixture port required")?.parse()?;
    if port == 0 {
        return Err("invalid fixture port".into());
    }
    let policy_path = args.get(1).filter(|path| path.as_str() != "-");
    let scheme = match args.get(2).map(String::as_str).unwrap_or("http") {
        "http" => "http",
        "https" => "https",
        _ => return Err("invalid fixture scheme".into()),
    };
    let ca_path = args.get(3).filter(|path| path.as_str() != "-");
    let revision: u32 = args.get(4).map(String::as_str).unwrap_or("1").parse()?;
    if revision == 0 {
        return Err("invalid fixture revision".into());
    }
    if ca_path.is_some() && scheme != "https" {
        return Err("TLS CA requires HTTPS fixture".into());
    }
    let transport = FixtureTransport {
        port,
        scheme,
        ca_pem: ca_path.map(std::fs::read).transpose()?.map(Arc::new),
        revision,
    };
    let governance = Governance::fixture(policy_path.map(std::path::PathBuf::from));
    governance
        .check(revision)
        .map_err(|_| "invalid fixture policy")?;
    let out: Output = Arc::new(Mutex::new(io::stdout()));
    let requests: Requests = Arc::new(Mutex::new(HashMap::new()));
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut seen = HashSet::new();
    let mut workers = Vec::new();
    emit(&out, json!({"kind":"ready","protocol":1}));
    let mut input = io::stdin().lock();
    loop {
        let mut bytes = Vec::new();
        let count = (&mut input)
            .take((FRAME_LIMIT + 2) as u64)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            break;
        }
        if count > FRAME_LIMIT + 1 || !bytes.ends_with(b"\n") {
            emit(&out, json!({"kind":"fatal","code":"FRAME_LIMIT"}));
            break;
        }
        let frame: Frame = match serde_json::from_slice(&bytes) {
            Ok(frame) => frame,
            Err(_) => {
                emit(&out, json!({"kind":"fatal","code":"INVALID_FRAME"}));
                break;
            }
        };
        match frame.kind.as_str() {
            "shutdown" => break,
            "pull" => {
                if let Some(active) = requests.lock().expect("requests lock").get(&frame.id) {
                    if let Err(mpsc::TrySendError::Full(_)) = active.credit.try_send(()) {
                        active.cancelled.store(true, Ordering::Relaxed);
                    }
                }
            }
            "cancel" => {
                if let Some(active) = requests.lock().expect("requests lock").get(&frame.id) {
                    active.cancelled.store(true, Ordering::Relaxed);
                }
            }
            "body" | "commit" => {
                if let Some(active) = requests.lock().expect("requests lock").get(&frame.id) {
                    if active.upload.try_send(frame).is_err() {
                        active.cancelled.store(true, Ordering::Relaxed);
                    }
                }
            }
            "start" | "upload" => {
                if seen.contains(&frame.id)
                    || seen.len() >= 64
                    || requests.lock().expect("requests lock").len() >= 8
                {
                    emit(&out, json!({"kind":"fatal","code":"REQUEST_LIMIT"}));
                    break;
                }
                seen.insert(frame.id);
                let cancelled = Arc::new(AtomicBool::new(false));
                let (credit, credits) = mpsc::sync_channel(1);
                let (upload, uploads) = mpsc::sync_channel(1);
                requests.lock().expect("requests lock").insert(
                    frame.id,
                    Active {
                        cancelled: cancelled.clone(),
                        credit,
                        upload,
                    },
                );
                let (out, requests, attempts, governance, transport) = (
                    out.clone(),
                    requests.clone(),
                    attempts.clone(),
                    governance.clone(),
                    transport.clone(),
                );
                workers.push(thread::spawn(move || {
                    let mut frame = frame;
                    let result = receive_body(&mut frame, &uploads, &cancelled, &out, &governance)
                        .and_then(|()| {
                            execute(
                                &frame,
                                &out,
                                &cancelled,
                                &credits,
                                &attempts,
                                &governance,
                                &transport,
                            )
                        });
                    requests.lock().expect("requests lock").remove(&frame.id);
                    match result {
                        Ok(()) => emit(&out, json!({"kind":"end", "id":frame.id})),
                        Err(code) => emit(&out, json!({"kind":"error", "id":frame.id,"code":code})),
                    }
                }));
            }
            _ => {
                emit(&out, json!({"kind":"fatal","code":"UNKNOWN_KIND"}));
                break;
            }
        }
    }
    for active in requests.lock().expect("requests lock").values() {
        active.cancelled.store(true, Ordering::Relaxed);
    }
    for worker in workers {
        worker.join().map_err(|_| "worker failed")?;
    }
    emit(
        &out,
        json!({"kind":"closed", "attempts":attempts.load(Ordering::SeqCst)}),
    );
    Ok(())
}
