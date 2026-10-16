//! Strict request-body validation and lease-bound brokering for A3-02.
//!
//! The broker validates Pi 1.0.4 text routes, resolves credentials only inside
//! Rust, and sends requests through an injected transport. Production currently
//! installs the disabled transport until external routing is separately
//! validated; tests inject deterministic in-memory fixtures.

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::services::ai_rate_limit::OwnedRequestPermit;
use crate::services::credentials::{self, CredentialBackend};
use crate::state::Database;

pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 44 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_ACTIVE_ATTEMPTS: usize = 8;
pub const MAX_RUN_ATTEMPTS: u8 = 8;
pub const RUN_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_MESSAGES: usize = 512;
const MAX_SYSTEM_BLOCKS: usize = 64;
const MAX_OUTPUT_TOKENS: u64 = 2048;
const MAX_SSE_LINE_BYTES: usize = 64 * 1024;

const MAX_ENCODED_CHUNK_BYTES: usize = MAX_CHUNK_BYTES.div_ceil(3) * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    OpenAiChat,
    AnthropicMessages,
}

impl Route {
    pub fn from_wire(value: &str) -> Result<Self, &'static str> {
        match value {
            "openai_chat" => Ok(Self::OpenAiChat),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            _ => Err("ROUTE_DENIED"),
        }
    }

    fn from_connection_protocol(value: &str) -> Result<Self, &'static str> {
        match value {
            "openai_chat" => Ok(Self::OpenAiChat),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            _ => Err("ROUTE_DENIED"),
        }
    }

    fn provider(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai",
            Self::AnthropicMessages => "anthropic",
        }
    }

    fn protocol(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }

    pub fn endpoint_url(self, base_url: &str) -> Result<String, &'static str> {
        let mut url = reqwest::Url::parse(base_url).map_err(|_| "ROUTE_DENIED")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("ROUTE_DENIED");
        }
        let path = url.path().trim_end_matches('/');
        let suffix = match self {
            Self::OpenAiChat => "/chat/completions",
            Self::AnthropicMessages => "/messages",
        };
        let path = if path.ends_with("/v1") {
            format!("{path}{suffix}")
        } else {
            format!("{path}/v1{suffix}")
        };
        url.set_path(&path);
        Ok(url.to_string())
    }
}

/// Rust-owned run lease. Only `wire_payload()` crosses into Node; the
/// connection identity and revision remain private so the Host cannot select
/// a destination or claim that a stale run belongs to a newly bound service.
/// The type intentionally has no `Serialize` or `Debug` implementation.
pub struct AgentGatewayLease {
    host_instance_id: String,
    conversation_id: String,
    run_id: String,
    lease_id: String,
    connection_id: String,
    config_revision: String,
    route: Route,
    model: String,
    budget: RunBudget,
}

impl AgentGatewayLease {
    pub fn issue(
        host_instance_id: &str,
        conversation_id: &str,
        run_id: &str,
        connection: &crate::db::ai_connections::AiConnection,
    ) -> Result<Self, &'static str> {
        Self::issue_at(
            host_instance_id,
            conversation_id,
            run_id,
            connection,
            Instant::now(),
        )
    }

    fn issue_at(
        host_instance_id: &str,
        conversation_id: &str,
        run_id: &str,
        connection: &crate::db::ai_connections::AiConnection,
        now: Instant,
    ) -> Result<Self, &'static str> {
        for value in [host_instance_id, conversation_id, run_id] {
            if !crate::services::agent_runtime::protocol::valid_token(value) {
                return Err("REQUEST_INVALID");
            }
        }
        if !connection.enabled {
            return Err("LEASE_REVOKED");
        }
        let route = Route::from_connection_protocol(&connection.protocol)?;
        if connection.id.is_empty()
            || connection.config_revision.is_empty()
            || connection.model.trim().is_empty()
            || connection.model.len() > 256
            || connection.model.contains('\0')
        {
            return Err("REQUEST_INVALID");
        }

        Ok(Self {
            host_instance_id: host_instance_id.to_string(),
            conversation_id: conversation_id.to_string(),
            run_id: run_id.to_string(),
            lease_id: format!("lease_{}", uuid::Uuid::new_v4().simple()),
            connection_id: connection.id.clone(),
            config_revision: connection.config_revision.clone(),
            route,
            model: connection.model.clone(),
            budget: RunBudget::new_at(now, RUN_TIMEOUT),
        })
    }

    /// The exact metadata the Pi SDK needs to select its registered codec.
    /// Credentials, URL, connection ID, and config revision never cross stdio.
    pub fn wire_payload(&self) -> Value {
        json!({
            "leaseId": self.lease_id,
            "provider": self.route.provider(),
            "protocol": self.route.protocol(),
            "model": self.model,
        })
    }

    pub fn matches_scope(
        &self,
        host_instance_id: &str,
        conversation_id: &str,
        run_id: &str,
        lease_id: &str,
    ) -> Result<(), &'static str> {
        if self.host_instance_id != host_instance_id
            || self.conversation_id != conversation_id
            || self.run_id != run_id
            || self.lease_id != lease_id
        {
            return Err("LEASE_REVOKED");
        }
        Ok(())
    }

    /// Recheck the current Agent binding snapshot before a request is committed
    /// to transport. The caller obtains `connection` from the DB without
    /// holding its lock over network I/O; no request budget is consumed here.
    pub fn revalidate_connection(
        &self,
        connection: &crate::db::ai_connections::AiConnection,
    ) -> Result<(), &'static str> {
        if connection.id != self.connection_id || connection.config_revision != self.config_revision
        {
            return Err("CONNECTION_CHANGED");
        }
        if !connection.enabled {
            return Err("LEASE_REVOKED");
        }
        if Route::from_connection_protocol(&connection.protocol)? != self.route
            || connection.model != self.model
        {
            return Err("CONNECTION_CHANGED");
        }
        Ok(())
    }

    /// Final local gate after the shared connection permit has been acquired.
    /// The caller must read `cancelled` and mutate this lease under the same
    /// run-state mutex used by cancellation, so a cancel cannot race the slot
    /// consumption. Stale, cancelled, or expired attempts consume no allowance.
    pub fn consume_attempt_if_current(
        &mut self,
        connection: &crate::db::ai_connections::AiConnection,
        cancelled: bool,
        now: Instant,
    ) -> Result<u8, &'static str> {
        if cancelled {
            return Err("CANCELLED");
        }
        self.revalidate_connection(connection)?;
        self.budget.consume_attempt_at(now)
    }

    pub fn route(&self) -> Route {
        self.route
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }

    pub fn connection_snapshot(&self) -> (&str, &str) {
        (&self.connection_id, &self.config_revision)
    }

    pub fn deadline(&self) -> Instant {
        self.budget.deadline()
    }
}

/// A strictly decoded Host-to-Rust frame. Identity validation is kept separate
/// from the active-lease/state lookup, which must happen before a reply is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostGatewayFrame {
    Open {
        request_id: String,
        conversation_id: String,
        run_id: String,
        lease_id: String,
        gateway_request_id: String,
        route: Route,
    },
    Chunk {
        request_id: String,
        conversation_id: String,
        run_id: String,
        lease_id: String,
        gateway_request_id: String,
        chunk_sequence: u64,
        bytes: Vec<u8>,
    },
    Commit {
        request_id: String,
        conversation_id: String,
        run_id: String,
        lease_id: String,
        gateway_request_id: String,
        chunk_count: u64,
        byte_length: usize,
    },
    Pull {
        request_id: String,
        conversation_id: String,
        run_id: String,
        lease_id: String,
        gateway_request_id: String,
        pull_sequence: u64,
    },
    Cancel {
        request_id: String,
        conversation_id: String,
        run_id: String,
        lease_id: String,
        gateway_request_id: String,
    },
}

/// Parse an exact Host-to-Rust gateway frame envelope and its local fields.
/// This function performs no I/O and does not imply that its lease is valid.
pub fn parse_host_gateway_frame(frame: &Value) -> Result<HostGatewayFrame, &'static str> {
    let kind = frame["type"].as_str().ok_or("REQUEST_INVALID")?;
    let base = [
        "protocol",
        "instance",
        "sequence",
        "type",
        "requestId",
        "conversationId",
        "runId",
        "leaseId",
        "gatewayRequestId",
    ];
    let mut allowed = base.to_vec();
    let extra: &[&str] = match kind {
        "gateway.request.open" => &["route", "method"],
        "gateway.request.chunk" => &["chunkSequence", "base64"],
        "gateway.request.commit" => &["chunkCount", "byteLength"],
        "gateway.response.pull" => &["pullSequence"],
        "gateway.request.cancel" => &[],
        _ => return Err("REQUEST_INVALID"),
    };
    allowed.extend_from_slice(extra);
    if !exact_fields(frame, &allowed)
        || frame["protocol"].as_u64() != Some(1)
        || !frame["instance"]
            .as_str()
            .is_some_and(crate::services::agent_runtime::protocol::valid_token)
        || !frame["sequence"].as_u64().is_some_and(|value| value > 0)
    {
        return Err("REQUEST_INVALID");
    }
    let identity = GatewayFrameIdentity {
        request_id: token_field(frame, "requestId")?,
        conversation_id: token_field(frame, "conversationId")?,
        run_id: token_field(frame, "runId")?,
        lease_id: token_field(frame, "leaseId")?,
        gateway_request_id: token_field(frame, "gatewayRequestId")?,
    };
    match kind {
        "gateway.request.open" => {
            if frame["method"].as_str() != Some("POST") {
                return Err("ROUTE_DENIED");
            }
            let route = Route::from_wire(frame["route"].as_str().ok_or("ROUTE_DENIED")?)?;
            Ok(HostGatewayFrame::Open {
                request_id: identity.request_id,
                conversation_id: identity.conversation_id,
                run_id: identity.run_id,
                lease_id: identity.lease_id,
                gateway_request_id: identity.gateway_request_id,
                route,
            })
        }
        "gateway.request.chunk" => {
            let encoded = frame["base64"].as_str().ok_or("REQUEST_INVALID")?;
            if encoded.is_empty() || encoded.len() > MAX_ENCODED_CHUNK_BYTES {
                return Err("REQUEST_LIMIT");
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "REQUEST_INVALID")?;
            if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
                return Err("REQUEST_LIMIT");
            }
            Ok(HostGatewayFrame::Chunk {
                request_id: identity.request_id,
                conversation_id: identity.conversation_id,
                run_id: identity.run_id,
                lease_id: identity.lease_id,
                gateway_request_id: identity.gateway_request_id,
                chunk_sequence: frame["chunkSequence"].as_u64().ok_or("REQUEST_INVALID")?,
                bytes,
            })
        }
        "gateway.request.commit" => {
            let chunk_count = frame["chunkCount"].as_u64().ok_or("REQUEST_INVALID")?;
            let byte_length = frame["byteLength"].as_u64().ok_or("REQUEST_INVALID")?;
            if chunk_count == 0 || byte_length == 0 || byte_length > MAX_REQUEST_BYTES as u64 {
                return Err("REQUEST_LIMIT");
            }
            Ok(HostGatewayFrame::Commit {
                request_id: identity.request_id,
                conversation_id: identity.conversation_id,
                run_id: identity.run_id,
                lease_id: identity.lease_id,
                gateway_request_id: identity.gateway_request_id,
                chunk_count,
                byte_length: byte_length as usize,
            })
        }
        "gateway.response.pull" => Ok(HostGatewayFrame::Pull {
            request_id: identity.request_id,
            conversation_id: identity.conversation_id,
            run_id: identity.run_id,
            lease_id: identity.lease_id,
            gateway_request_id: identity.gateway_request_id,
            pull_sequence: frame["pullSequence"].as_u64().ok_or("REQUEST_INVALID")?,
        }),
        "gateway.request.cancel" => Ok(HostGatewayFrame::Cancel {
            request_id: identity.request_id,
            conversation_id: identity.conversation_id,
            run_id: identity.run_id,
            lease_id: identity.lease_id,
            gateway_request_id: identity.gateway_request_id,
        }),
        _ => Err("REQUEST_INVALID"),
    }
}

struct GatewayFrameIdentity {
    request_id: String,
    conversation_id: String,
    run_id: String,
    lease_id: String,
    gateway_request_id: String,
}

fn token_field(frame: &Value, key: &str) -> Result<String, &'static str> {
    frame[key]
        .as_str()
        .filter(|value| crate::services::agent_runtime::protocol::valid_token(value))
        .map(ToOwned::to_owned)
        .ok_or("REQUEST_INVALID")
}

fn exact_fields(frame: &Value, allowed: &[&str]) -> bool {
    frame.as_object().is_some_and(|object| {
        object.keys().all(|key| allowed.contains(&key.as_str()))
            && allowed.iter().all(|key| object.contains_key(*key))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptPhase {
    Uploading,
    Committed,
    Streaming,
    Ended,
    Cancelled,
    Failed,
}

/// Per-attempt flow-control state. It owns at most one request body (2 MiB)
/// and does not perform transport work; callers must validate lease/revision
/// immediately before handing `commit_body()` to the Rust transport.
pub struct GatewayAttemptState {
    route: Route,
    model: String,
    phase: AttemptPhase,
    request_body: Vec<u8>,
    next_chunk_sequence: u64,
    chunk_credit_outstanding: bool,
    next_pull_sequence: u64,
    pull_outstanding: Option<u64>,
    response_bytes: usize,
}

impl GatewayAttemptState {
    pub fn new(route: Route, model: impl Into<String>) -> Result<Self, &'static str> {
        let model = model.into();
        if model.is_empty() || model.len() > 256 {
            return Err("REQUEST_INVALID");
        }
        Ok(Self {
            route,
            model,
            phase: AttemptPhase::Uploading,
            request_body: Vec::new(),
            next_chunk_sequence: 0,
            chunk_credit_outstanding: false,
            next_pull_sequence: 0,
            pull_outstanding: None,
            response_bytes: 0,
        })
    }

    /// Issue the one allowed outstanding upload credit.
    pub fn issue_chunk_credit(&mut self) -> Result<(u64, usize), &'static str> {
        if self.phase != AttemptPhase::Uploading || self.chunk_credit_outstanding {
            return Err("REQUEST_INVALID");
        }
        self.chunk_credit_outstanding = true;
        Ok((self.next_chunk_sequence, MAX_CHUNK_BYTES))
    }

    /// Consume the current upload credit and append one bounded chunk.
    pub fn accept_chunk(&mut self, sequence: u64, bytes: &[u8]) -> Result<(), &'static str> {
        if self.phase != AttemptPhase::Uploading
            || !self.chunk_credit_outstanding
            || sequence != self.next_chunk_sequence
        {
            return Err("REQUEST_INVALID");
        }
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err("REQUEST_LIMIT");
        }
        let next_len = self
            .request_body
            .len()
            .checked_add(bytes.len())
            .ok_or("REQUEST_LIMIT")?;
        if next_len > MAX_REQUEST_BYTES {
            return Err("REQUEST_LIMIT");
        }
        self.request_body.extend_from_slice(bytes);
        self.next_chunk_sequence = self
            .next_chunk_sequence
            .checked_add(1)
            .ok_or("REQUEST_LIMIT")?;
        self.chunk_credit_outstanding = false;
        Ok(())
    }

    /// Validate upload totals and the provider-specific strict JSON shape.
    /// The caller must still recheck the live lease, config revision, cancel
    /// state, deadline, shared limiter, and budget before network I/O.
    pub fn commit_body(
        &mut self,
        chunk_count: u64,
        byte_length: usize,
    ) -> Result<&[u8], &'static str> {
        if self.phase != AttemptPhase::Uploading
            || chunk_count != self.next_chunk_sequence
            || byte_length != self.request_body.len()
            || byte_length == 0
        {
            return Err("REQUEST_INVALID");
        }
        validate_request(self.route, &self.model, &self.request_body)?;
        // The Host receives the next chunk credit after every accepted chunk,
        // including the final one. A matching commit closes that unused credit.
        self.chunk_credit_outstanding = false;
        self.phase = AttemptPhase::Committed;
        Ok(&self.request_body)
    }

    pub fn begin_streaming(&mut self, status: u16, content_type: &str) -> Result<(), &'static str> {
        if self.phase != AttemptPhase::Committed {
            return Err("REQUEST_INVALID");
        }
        if status != 200 || !is_event_stream_content_type(content_type) {
            self.phase = AttemptPhase::Failed;
            return Err(if (300..400).contains(&status) {
                "REDIRECT_DENIED"
            } else {
                "UPSTREAM_HTTP"
            });
        }
        self.phase = AttemptPhase::Streaming;
        Ok(())
    }

    /// Request exactly one response block, preserving reader backpressure.
    pub fn issue_pull(&mut self, sequence: u64) -> Result<(), &'static str> {
        if self.phase != AttemptPhase::Streaming
            || self.pull_outstanding.is_some()
            || sequence != self.next_pull_sequence
        {
            return Err("REQUEST_INVALID");
        }
        self.pull_outstanding = Some(sequence);
        Ok(())
    }

    pub fn accept_response_chunk(
        &mut self,
        sequence: u64,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        if self.phase != AttemptPhase::Streaming || self.pull_outstanding != Some(sequence) {
            return Err("REQUEST_INVALID");
        }
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err("REQUEST_LIMIT");
        }
        let next_len = self
            .response_bytes
            .checked_add(bytes.len())
            .ok_or("REQUEST_LIMIT")?;
        if next_len > MAX_RESPONSE_BYTES {
            return Err("REQUEST_LIMIT");
        }
        self.response_bytes = next_len;
        self.pull_outstanding = None;
        self.next_pull_sequence = self
            .next_pull_sequence
            .checked_add(1)
            .ok_or("REQUEST_LIMIT")?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), &'static str> {
        if self.phase != AttemptPhase::Streaming {
            return Err("REQUEST_INVALID");
        }
        // EOF fulfills the currently outstanding pull without a data chunk.
        self.pull_outstanding = None;
        self.phase = AttemptPhase::Ended;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), &'static str> {
        if matches!(
            self.phase,
            AttemptPhase::Ended | AttemptPhase::Cancelled | AttemptPhase::Failed
        ) {
            return Err("REQUEST_INVALID");
        }
        self.phase = AttemptPhase::Cancelled;
        self.chunk_credit_outstanding = false;
        self.pull_outstanding = None;
        self.request_body.clear();
        Ok(())
    }

    pub fn fail(&mut self) {
        if !matches!(self.phase, AttemptPhase::Ended | AttemptPhase::Cancelled) {
            self.phase = AttemptPhase::Failed;
        }
        self.chunk_credit_outstanding = false;
        self.pull_outstanding = None;
        self.request_body.clear();
    }
}

fn is_event_stream_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Sanitized usage snapshot produced only by Rust from supported provider SSE
/// events. Missing counters remain null; malformed or incomplete accounting
/// makes the whole snapshot unavailable instead of reporting a false zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

/// Incremental SSE accounting parser. It sees the same bytes that are streamed
/// to Pi, but retains only one bounded line/event and a four-counter snapshot.
/// Unknown provider events and malformed JSON are ignored for accounting; a
/// malformed supported usage value invalidates the final snapshot.
pub struct SseUsageTracker {
    route: Route,
    line: Vec<u8>,
    event_data: Vec<Vec<u8>>,
    event_bytes: usize,
    dropping_line: bool,
    usage: UsageSnapshot,
    saw_input: bool,
    saw_output: bool,
    invalid: bool,
}

impl SseUsageTracker {
    pub fn new(route: Route) -> Self {
        Self {
            route,
            line: Vec::new(),
            event_data: Vec::new(),
            event_bytes: 0,
            dropping_line: false,
            usage: UsageSnapshot {
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            saw_input: false,
            saw_output: false,
            invalid: false,
        }
    }

    /// Consume an arbitrary network chunk. Chunk boundaries may split UTF-8,
    /// SSE fields, CRLF pairs, or JSON values.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        for byte in bytes {
            if *byte == b'\n' {
                if !self.dropping_line {
                    self.finish_line()?;
                }
                self.line.clear();
                self.dropping_line = false;
                continue;
            }
            if self.dropping_line {
                continue;
            }
            if self.line.len() >= MAX_SSE_LINE_BYTES {
                self.line.clear();
                self.dropping_line = true;
                self.invalid = true;
                return Err("STREAM_FAILED");
            }
            self.line.push(*byte);
        }
        Ok(())
    }

    /// Flush a final unterminated line and dispatch its event at EOF.
    pub fn finish(&mut self) -> Result<(), &'static str> {
        if !self.line.is_empty() {
            self.finish_line()?;
            self.line.clear();
        }
        self.dispatch_event();
        Ok(())
    }

    pub fn snapshot(&self) -> Option<UsageSnapshot> {
        if self.invalid || !self.saw_input || !self.saw_output {
            None
        } else {
            Some(self.usage)
        }
    }

    fn finish_line(&mut self) -> Result<(), &'static str> {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        if self.line.is_empty() {
            self.dispatch_event();
            return Ok(());
        }
        if self.line[0] == b':' {
            return Ok(());
        }
        let Some(separator) = self.line.iter().position(|byte| *byte == b':') else {
            return Ok(());
        };
        let (field, value_with_separator) = self.line.split_at(separator);
        if field != b"data" {
            return Ok(());
        }
        let value = &value_with_separator[1..];
        let value = value.strip_prefix(b" ").unwrap_or(value);
        self.event_bytes = self
            .event_bytes
            .checked_add(value.len())
            .ok_or("REQUEST_LIMIT")?;
        if self.event_bytes > MAX_SSE_LINE_BYTES || self.event_data.len() >= 16 {
            return Err("REQUEST_LIMIT");
        }
        self.event_data.push(value.to_vec());
        Ok(())
    }

    fn dispatch_event(&mut self) {
        if self.event_data.is_empty() {
            self.event_bytes = 0;
            return;
        }
        let payload = self.event_data.iter().enumerate().fold(
            Vec::with_capacity(self.event_bytes),
            |mut joined, (index, line)| {
                if index > 0 {
                    joined.push(b'\n');
                }
                joined.extend_from_slice(line);
                joined
            },
        );
        self.event_data.clear();
        self.event_bytes = 0;
        if payload == b"[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
            return;
        };
        match self.route {
            Route::OpenAiChat => self.observe_openai_usage(&value),
            Route::AnthropicMessages => self.observe_anthropic_usage(&value),
        }
    }

    fn observe_openai_usage(&mut self, value: &Value) {
        let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) else {
            return;
        };
        let Some(usage) = usage.as_object() else {
            self.invalid = true;
            return;
        };
        self.saw_input = true;
        self.saw_output = true;
        self.usage.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
        self.usage.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
        self.usage.cache_read_tokens = None;
        self.usage.cache_write_tokens = None;
        if self.usage.input_tokens.is_none() || self.usage.output_tokens.is_none() {
            self.invalid = true;
        }
    }

    fn observe_anthropic_usage(&mut self, value: &Value) {
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let Some(usage) = value.pointer("/message/usage") else {
                    return;
                };
                self.saw_input = true;
                self.usage.input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
                self.usage.cache_read_tokens =
                    usage.get("cache_read_input_tokens").and_then(Value::as_u64);
                self.usage.cache_write_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64);
                if self.usage.input_tokens.is_none()
                    || invalid_present_counter(usage, "cache_read_input_tokens")
                    || invalid_present_counter(usage, "cache_creation_input_tokens")
                {
                    self.invalid = true;
                }
            }
            Some("message_delta") => {
                let Some(usage) = value.get("usage") else {
                    return;
                };
                self.saw_output = true;
                self.usage.output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
                if self.usage.output_tokens.is_none() {
                    self.invalid = true;
                }
            }
            _ => {}
        }
    }
}

fn invalid_present_counter(object: &Value, key: &str) -> bool {
    object
        .get(key)
        .is_some_and(|value| value.as_u64().is_none())
}

/// The mutable budget is intended to live behind one run-level mutex. Waiting
/// for the shared connection limiter does not call `consume_attempt`; callers
/// invoke it only after permit acquisition and the final lease/revision check.
pub struct RunBudget {
    deadline: Instant,
    attempts_used: u8,
}

impl RunBudget {
    pub fn new_at(now: Instant, timeout: Duration) -> Self {
        Self {
            deadline: now + timeout.min(RUN_TIMEOUT),
            attempts_used: 0,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn attempts_used(&self) -> u8 {
        self.attempts_used
    }

    pub fn consume_attempt(&mut self) -> Result<u8, &'static str> {
        self.consume_attempt_at(Instant::now())
    }

    pub fn consume_attempt_at(&mut self, now: Instant) -> Result<u8, &'static str> {
        if now >= self.deadline {
            return Err("TIMEOUT");
        }
        if self.attempts_used >= MAX_RUN_ATTEMPTS {
            return Err("BUDGET_EXCEEDED");
        }
        self.attempts_used += 1;
        Ok(self.attempts_used)
    }
}

/// Validate the exact text-only Pi request shape and leased model identity.
///
/// The caller owns the original bytes and must send those bytes only after it
/// rechecks the lease, binding, connection revision, cancellation, and budget.
pub fn validate_request(
    route: Route,
    leased_model: &str,
    bytes: &[u8],
) -> Result<(), &'static str> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("REQUEST_LIMIT");
    }
    if leased_model.is_empty() || leased_model.len() > 256 {
        return Err("REQUEST_INVALID");
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "REQUEST_INVALID")?;
    match route {
        Route::OpenAiChat => validate_openai(&value, leased_model),
        Route::AnthropicMessages => validate_anthropic(&value, leased_model),
    }
}

fn validate_openai(value: &Value, leased_model: &str) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("REQUEST_INVALID")?;
    require_fields(
        object,
        &[
            "model",
            "messages",
            "stream",
            "stream_options",
            "max_tokens",
            "max_completion_tokens",
            "temperature",
        ],
        &["model", "messages", "stream"],
    )?;
    require_model_stream(object, leased_model)?;
    validate_messages(object.get("messages").ok_or("REQUEST_INVALID")?, true)?;

    if let Some(options) = object.get("stream_options") {
        let options = options.as_object().ok_or("REQUEST_INVALID")?;
        require_fields(options, &["include_usage"], &["include_usage"])?;
        if options.get("include_usage") != Some(&Value::Bool(true)) {
            return Err("REQUEST_INVALID");
        }
    }
    validate_token_limit(object, &["max_tokens", "max_completion_tokens"])?;
    if let Some(temperature) = object.get("temperature") {
        validate_temperature(temperature, 2.0)?;
    }
    Ok(())
}

fn validate_anthropic(value: &Value, leased_model: &str) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("REQUEST_INVALID")?;
    require_fields(
        object,
        &[
            "model",
            "messages",
            "system",
            "stream",
            "max_tokens",
            "temperature",
        ],
        &["model", "messages", "stream", "max_tokens"],
    )?;
    require_model_stream(object, leased_model)?;
    validate_token_limit(object, &["max_tokens"])?;
    validate_messages(object.get("messages").ok_or("REQUEST_INVALID")?, false)?;
    if let Some(system) = object.get("system") {
        validate_text_blocks(system, MAX_SYSTEM_BLOCKS)?;
    }
    if let Some(temperature) = object.get("temperature") {
        validate_temperature(temperature, 1.0)?;
    }
    Ok(())
}

fn require_model_stream(
    object: &Map<String, Value>,
    leased_model: &str,
) -> Result<(), &'static str> {
    if object.get("model").and_then(Value::as_str) != Some(leased_model)
        || object.get("stream") != Some(&Value::Bool(true))
    {
        return Err("REQUEST_INVALID");
    }
    Ok(())
}

fn validate_messages(value: &Value, openai_roles: bool) -> Result<(), &'static str> {
    let messages = value.as_array().ok_or("REQUEST_INVALID")?;
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err("REQUEST_LIMIT");
    }
    for message in messages {
        let object = message.as_object().ok_or("REQUEST_INVALID")?;
        require_fields(object, &["role", "content"], &["role", "content"])?;
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .ok_or("REQUEST_INVALID")?;
        if !(matches!(role, "user" | "assistant")
            || (openai_roles && matches!(role, "system" | "developer")))
        {
            return Err("REQUEST_INVALID");
        }
        let content = object.get("content").ok_or("REQUEST_INVALID")?;
        if openai_roles {
            if !content.is_string() {
                if !matches!(role, "user" | "assistant") {
                    return Err("REQUEST_INVALID");
                }
                validate_text_blocks(content, MAX_MESSAGES)?;
            }
        } else if !content.is_string() {
            validate_text_blocks(content, MAX_MESSAGES)?;
        }
    }
    Ok(())
}

fn validate_text_blocks(value: &Value, max_blocks: usize) -> Result<(), &'static str> {
    let blocks = value.as_array().ok_or("REQUEST_INVALID")?;
    if blocks.len() > max_blocks {
        return Err("REQUEST_LIMIT");
    }
    for block in blocks {
        let object = block.as_object().ok_or("REQUEST_INVALID")?;
        require_fields(object, &["type", "text"], &["type", "text"])?;
        if object.get("type").and_then(Value::as_str) != Some("text")
            || !object.get("text").is_some_and(Value::is_string)
        {
            return Err("REQUEST_INVALID");
        }
    }
    Ok(())
}

fn validate_token_limit(
    object: &Map<String, Value>,
    candidates: &[&str],
) -> Result<(), &'static str> {
    let present: Vec<_> = candidates
        .iter()
        .filter(|key| object.contains_key(**key))
        .collect();
    if present.len() != 1 {
        return Err("REQUEST_INVALID");
    }
    let value = object
        .get(*present[0])
        .and_then(Value::as_u64)
        .ok_or("REQUEST_INVALID")?;
    if !(1..=MAX_OUTPUT_TOKENS).contains(&value) {
        return Err("REQUEST_INVALID");
    }
    Ok(())
}

fn validate_temperature(value: &Value, max: f64) -> Result<(), &'static str> {
    let value = value.as_f64().ok_or("REQUEST_INVALID")?;
    if !value.is_finite() || !(0.0..=max).contains(&value) {
        return Err("REQUEST_INVALID");
    }
    Ok(())
}

fn require_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), &'static str> {
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err("REQUEST_INVALID");
    }
    Ok(())
}

pub type GatewayReplySink = Arc<dyn Fn(Value) + Send + Sync + 'static>;

/// Rust-only request passed to the selected transport. It is never serialized
/// or sent to the Agent Host; in particular, `authorization` stays in Rust.
pub struct GatewayUpstreamRequest {
    pub route: Route,
    pub endpoint: String,
    pub model: String,
    pub body: Vec<u8>,
    pub authorization: Option<String>,
    pub anthropic_version: Option<&'static str>,
}

pub struct GatewayTransportResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Box<dyn Read + Send>,
    /// Must be idempotent and interrupt a blocked `body.read`, allowing
    /// cancellation and lease revocation to release the response promptly.
    pub cancel: GatewayCancelHook,
}

pub type GatewayCancelHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// The A3-02 transport seam. Production currently uses the disabled adapter;
/// local end-to-end tests inject a deterministic fixture transport.
pub trait GatewayTransport: Send + Sync + 'static {
    fn enabled(&self) -> bool {
        true
    }

    fn open(
        &self,
        request: GatewayUpstreamRequest,
    ) -> Result<GatewayTransportResponse, &'static str>;
}

pub struct DisabledGatewayTransport;

impl GatewayTransport for DisabledGatewayTransport {
    fn enabled(&self) -> bool {
        false
    }

    fn open(
        &self,
        _request: GatewayUpstreamRequest,
    ) -> Result<GatewayTransportResponse, &'static str> {
        Err("UPSTREAM_UNAVAILABLE")
    }
}

#[derive(Clone)]
struct GatewayRequestIdentity {
    conversation_id: String,
    run_id: String,
    lease_id: String,
    gateway_request_id: String,
}

struct GatewayActiveAttempt {
    identity: GatewayRequestIdentity,
    route: Route,
    state: Mutex<GatewayAttemptState>,
    response: Mutex<Option<Box<dyn Read + Send>>>,
    cancel_hook: Mutex<Option<GatewayCancelHook>>,
    usage: Mutex<SseUsageTracker>,
    permit: Mutex<Option<OwnedRequestPermit>>,
    cancelled: AtomicBool,
}

#[derive(Default)]
struct GatewayBrokerState {
    leases: HashMap<String, Arc<Mutex<AgentGatewayLease>>>,
    attempts: HashMap<String, Arc<GatewayActiveAttempt>>,
    last_host_request_sequence: u64,
    last_gateway_request_sequence: u64,
}

/// Bridges only the fixed A3-02 frames. The broker owns leases, checks the
/// current Agent binding before committing and while streaming, and never puts
/// the credential or endpoint on stdio.
pub struct GatewayBroker {
    instance_id: String,
    db: Arc<Database>,
    credential_backend: Arc<dyn CredentialBackend>,
    transport: Arc<dyn GatewayTransport>,
    state: Mutex<GatewayBrokerState>,
}

impl GatewayBroker {
    pub fn new(
        instance_id: String,
        db: Arc<Database>,
        credential_backend: Arc<dyn CredentialBackend>,
        transport: Arc<dyn GatewayTransport>,
    ) -> Self {
        Self {
            instance_id,
            db,
            credential_backend,
            transport,
            state: Mutex::new(GatewayBrokerState::default()),
        }
    }

    pub fn register_lease(&self, lease: AgentGatewayLease) -> Result<(), &'static str> {
        if lease.host_instance_id != self.instance_id {
            return Err("LEASE_REVOKED");
        }
        let mut state = self.state.lock().map_err(|_| "LEASE_REVOKED")?;
        if state.leases.contains_key(&lease.lease_id) {
            return Err("LEASE_REVOKED");
        }
        state
            .leases
            .insert(lease.lease_id.clone(), Arc::new(Mutex::new(lease)));
        Ok(())
    }

    pub fn issue_run_lease(
        &self,
        conversation_id: &str,
        run_id: &str,
    ) -> crate::error::AppResult<Value> {
        let connection = credentials::resolve_agent_model_connection_metadata(&self.db, None)?;
        let lease =
            AgentGatewayLease::issue(&self.instance_id, conversation_id, run_id, &connection)
                .map_err(|_| {
                    crate::error::AppError::conflict("Agent 服务无法为本次回答签发模型租约")
                })?;
        let payload = lease.wire_payload();
        self.register_lease(lease)
            .map_err(|_| crate::error::AppError::conflict("Agent 模型租约当前不可用"))?;
        Ok(payload)
    }

    pub fn revoke_run(&self, conversation_id: &str, run_id: &str) {
        let (leases, attempts) = self.remove_matching(|identity| {
            identity.conversation_id == conversation_id && identity.run_id == run_id
        });
        for attempt in attempts {
            cancel_active_attempt(&attempt);
        }
        drop(leases);
    }

    pub fn revoke_conversation(&self, conversation_id: &str) {
        let (leases, attempts) =
            self.remove_matching(|identity| identity.conversation_id == conversation_id);
        for attempt in attempts {
            cancel_active_attempt(&attempt);
        }
        drop(leases);
    }

    pub fn revoke_all(&self) {
        let (leases, attempts) = self.remove_matching(|_| true);
        for attempt in attempts {
            cancel_active_attempt(&attempt);
        }
        drop(leases);
    }

    #[cfg(test)]
    pub fn lease_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.leases.len())
            .unwrap_or(0)
    }

    fn remove_matching(
        &self,
        matches: impl Fn(&GatewayRequestIdentity) -> bool,
    ) -> (
        Vec<Arc<Mutex<AgentGatewayLease>>>,
        Vec<Arc<GatewayActiveAttempt>>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return (Vec::new(), Vec::new());
        };
        let lease_ids = state
            .leases
            .iter()
            .filter_map(|(lease_id, lease)| {
                lease
                    .lock()
                    .ok()
                    .and_then(|lease| (matches_lease(&lease, &matches)).then(|| lease_id.clone()))
            })
            .collect::<Vec<_>>();
        let leases = lease_ids
            .into_iter()
            .filter_map(|lease_id| state.leases.remove(&lease_id))
            .collect::<Vec<_>>();
        let attempt_ids = state
            .attempts
            .iter()
            .filter(|(_, attempt)| matches(&attempt.identity))
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        let attempts = attempt_ids
            .into_iter()
            .filter_map(|request_id| state.attempts.remove(&request_id))
            .collect::<Vec<_>>();
        (leases, attempts)
    }

    pub fn handle_host_frame(
        self: &Arc<Self>,
        frame: Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        if frame["instance"].as_str() != Some(self.instance_id.as_str()) {
            return Err("IDENTITY_MISMATCH");
        }
        let request = parse_host_gateway_frame(&frame)?;
        let identity = identity_from_request(&request);
        let Some(sequence) = gateway_host_request_sequence(
            frame["requestId"].as_str().unwrap_or_default(),
            &self.instance_id,
        ) else {
            reply_error(&reply, &self.instance_id, &frame, "REQUEST_INVALID", None);
            return Ok(());
        };
        let sequence_invalid = {
            let mut state = self.state.lock().map_err(|_| "LEASE_REVOKED")?;
            if sequence <= state.last_host_request_sequence {
                true
            } else {
                state.last_host_request_sequence = sequence;
                false
            }
        };
        if sequence_invalid {
            reply_error(&reply, &self.instance_id, &frame, "REQUEST_INVALID", None);
            return Ok(());
        }
        match request {
            HostGatewayFrame::Open { route, .. } => {
                self.open_attempt(identity, route, &frame, reply)
            }
            HostGatewayFrame::Chunk {
                chunk_sequence,
                bytes,
                ..
            } => self.accept_upload(identity, chunk_sequence, &bytes, &frame, reply),
            HostGatewayFrame::Commit {
                chunk_count,
                byte_length,
                ..
            } => self.commit_attempt(identity, chunk_count, byte_length, &frame, reply),
            HostGatewayFrame::Pull { pull_sequence, .. } => {
                self.pull_response(identity, pull_sequence, &frame, reply)
            }
            HostGatewayFrame::Cancel { .. } => self.cancel_attempt(identity, &frame, reply),
        }
    }

    fn open_attempt(
        self: &Arc<Self>,
        identity: GatewayRequestIdentity,
        route: Route,
        frame: &Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        let sequence = gateway_attempt_sequence(&identity.gateway_request_id, &self.instance_id);
        let lease = {
            let mut state = self.state.lock().map_err(|_| "LEASE_REVOKED")?;
            let Some(sequence) = sequence else {
                reply_error(&reply, &self.instance_id, frame, "REQUEST_INVALID", None);
                return Ok(());
            };
            if sequence != state.last_gateway_request_sequence.saturating_add(1) {
                reply_error(&reply, &self.instance_id, frame, "REQUEST_INVALID", None);
                return Ok(());
            }
            state.last_gateway_request_sequence = sequence;
            if state.attempts.len() >= MAX_ACTIVE_ATTEMPTS {
                reply_error(&reply, &self.instance_id, frame, "REQUEST_LIMIT", None);
                return Ok(());
            }
            state.leases.get(&identity.lease_id).cloned()
        };
        let Some(lease) = lease else {
            reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
            return Ok(());
        };
        let model = {
            let lease = lease.lock().map_err(|_| "LEASE_REVOKED")?;
            if lease
                .matches_scope(
                    &self.instance_id,
                    &identity.conversation_id,
                    &identity.run_id,
                    &identity.lease_id,
                )
                .is_err()
            {
                reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
                return Ok(());
            }
            if lease.route() != route {
                reply_error(&reply, &self.instance_id, frame, "ROUTE_DENIED", None);
                return Ok(());
            }
            lease.model().to_string()
        };
        let mut attempt_state =
            GatewayAttemptState::new(route, model).map_err(|_| "REQUEST_INVALID")?;
        let (next_chunk, max_bytes) = attempt_state
            .issue_chunk_credit()
            .map_err(|_| "REQUEST_INVALID")?;
        let attempt = Arc::new(GatewayActiveAttempt {
            identity: identity.clone(),
            route,
            state: Mutex::new(attempt_state),
            response: Mutex::new(None),
            cancel_hook: Mutex::new(None),
            usage: Mutex::new(SseUsageTracker::new(route)),
            permit: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        });
        {
            let mut state = self.state.lock().map_err(|_| "LEASE_REVOKED")?;
            if !state.leases.contains_key(&identity.lease_id)
                || state.attempts.contains_key(&identity.gateway_request_id)
            {
                reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
                return Ok(());
            }
            state
                .attempts
                .insert(identity.gateway_request_id.clone(), Arc::clone(&attempt));
        }
        let mut credit = reply_envelope(&self.instance_id, frame, "gateway.request.credit");
        credit["nextChunk"] = json!(next_chunk);
        credit["maxBytes"] = json!(max_bytes);
        reply(credit);
        Ok(())
    }

    fn accept_upload(
        &self,
        identity: GatewayRequestIdentity,
        sequence: u64,
        bytes: &[u8],
        frame: &Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        let Some(attempt) = self.find_attempt(&identity) else {
            reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
            return Ok(());
        };
        let result = {
            let mut state = attempt.state.lock().map_err(|_| "REQUEST_INVALID")?;
            state
                .accept_chunk(sequence, bytes)
                .and_then(|()| state.issue_chunk_credit())
        };
        match result {
            Ok((next_chunk, max_bytes)) => {
                let mut credit = reply_envelope(&self.instance_id, frame, "gateway.request.credit");
                credit["nextChunk"] = json!(next_chunk);
                credit["maxBytes"] = json!(max_bytes);
                reply(credit);
            }
            Err(code) => {
                self.finish_attempt(&identity.gateway_request_id, &attempt);
                reply_error(&reply, &self.instance_id, frame, code, None);
            }
        }
        Ok(())
    }

    fn commit_attempt(
        self: &Arc<Self>,
        identity: GatewayRequestIdentity,
        chunk_count: u64,
        byte_length: usize,
        frame: &Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        let Some(attempt) = self.find_attempt(&identity) else {
            reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
            return Ok(());
        };
        let body = {
            let mut state = attempt.state.lock().map_err(|_| "REQUEST_INVALID")?;
            match state.commit_body(chunk_count, byte_length) {
                Ok(body) => body.to_vec(),
                Err(code) => {
                    drop(state);
                    self.finish_attempt(&identity.gateway_request_id, &attempt);
                    reply_error(&reply, &self.instance_id, frame, code, None);
                    return Ok(());
                }
            }
        };
        let broker = Arc::clone(self);
        let request_frame = frame.clone();
        thread::Builder::new()
            .name("agent-gateway-open".to_string())
            .spawn(move || {
                broker.open_upstream(attempt, identity, body, request_frame, reply);
            })
            .map_err(|_| "gateway_worker_unavailable")?;
        Ok(())
    }

    fn open_upstream(
        self: &Arc<Self>,
        attempt: Arc<GatewayActiveAttempt>,
        identity: GatewayRequestIdentity,
        body: Vec<u8>,
        frame: Value,
        reply: GatewayReplySink,
    ) {
        let fail = |code| {
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(&reply, &self.instance_id, &frame, code, None);
        };
        if attempt.cancelled.load(Ordering::Acquire) {
            fail("CANCELLED");
            return;
        }
        if !self.transport.enabled() {
            fail("UPSTREAM_UNAVAILABLE");
            return;
        }
        let lease = match self.lease_for(&identity) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                fail("LEASE_REVOKED");
                return;
            }
            Err(code) => {
                fail(code);
                return;
            }
        };
        let (connection_id, revision, deadline) = match lease.lock() {
            Ok(lease) => {
                let (connection_id, revision) = lease.connection_snapshot();
                (
                    connection_id.to_string(),
                    revision.to_string(),
                    lease.deadline(),
                )
            }
            Err(_) => {
                fail("LEASE_REVOKED");
                return;
            }
        };
        let expected = Some((connection_id.as_str(), revision.as_str()));
        let connection =
            match credentials::resolve_agent_model_connection_metadata(&self.db, expected) {
                Ok(connection) => connection,
                Err(error) => {
                    fail(gateway_error_for_app(&error));
                    return;
                }
            };
        let limiter = match crate::services::ai_rate_limit::for_connection(
            &connection.id,
            connection.max_concurrency,
            connection.requests_per_minute,
            connection.requests_per_hour,
        ) {
            Ok(limiter) => limiter,
            Err(_) => {
                fail("UPSTREAM_UNAVAILABLE");
                return;
            }
        };
        let permit = match limiter.acquire_until_owned(&attempt.cancelled, Some(deadline)) {
            Ok(permit) => permit,
            Err(error) => {
                fail(if error.code() == "TIMEOUT" {
                    "TIMEOUT"
                } else {
                    "CANCELLED"
                });
                return;
            }
        };
        let resolved = match credentials::resolve_agent_model_connection_with_credential(
            &self.db,
            self.credential_backend.as_ref(),
            expected,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                fail(gateway_error_for_app(&error));
                return;
            }
        };
        if resolved.connection.deployment == "cloud"
            && resolved
                .credential()
                .is_none_or(|secret| secret.trim().is_empty())
        {
            fail("UPSTREAM_UNAVAILABLE");
            return;
        }
        let endpoint = match attempt.route.endpoint_url(&resolved.connection.base_url) {
            Ok(endpoint) => endpoint,
            Err(code) => {
                fail(code);
                return;
            }
        };
        let lease_state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                fail("LEASE_REVOKED");
                return;
            }
        };
        let current_lease = lease_state.leases.get(&identity.lease_id);
        if !current_lease.is_some_and(|current| Arc::ptr_eq(current, &lease)) {
            drop(lease_state);
            fail("LEASE_REVOKED");
            return;
        }
        let attempt_number = {
            let mut lease = match lease.lock() {
                Ok(lease) => lease,
                Err(_) => {
                    drop(lease_state);
                    fail("LEASE_REVOKED");
                    return;
                }
            };
            match lease.consume_attempt_if_current(
                &resolved.connection,
                attempt.cancelled.load(Ordering::Acquire),
                Instant::now(),
            ) {
                Ok(number) => number,
                Err(code) => {
                    drop(lease);
                    drop(lease_state);
                    fail(code);
                    return;
                }
            }
        };
        drop(lease_state);
        tracing::debug!(
            operation = "agent_gateway",
            attempt = attempt_number,
            route = attempt.route.protocol()
        );
        if let Ok(mut permit_slot) = attempt.permit.lock() {
            *permit_slot = Some(permit);
        } else {
            fail("UPSTREAM_UNAVAILABLE");
            return;
        }
        let credential = resolved.credential().map(str::to_string);
        let request = GatewayUpstreamRequest {
            route: attempt.route,
            endpoint,
            model: resolved.connection.model.clone(),
            body,
            authorization: match attempt.route {
                Route::OpenAiChat => credential.map(|secret| format!("Bearer {secret}")),
                Route::AnthropicMessages => credential,
            },
            anthropic_version: (attempt.route == Route::AnthropicMessages).then_some("2023-06-01"),
        };
        let response = match self.transport.open(request) {
            Ok(response) => response,
            Err(code) => {
                fail(allowed_gateway_error(code));
                return;
            }
        };
        if attempt.cancelled.load(Ordering::Acquire) {
            fail("CANCELLED");
            return;
        }
        let result = attempt
            .state
            .lock()
            .map_err(|_| "STREAM_FAILED")
            .and_then(|mut state| state.begin_streaming(response.status, &response.content_type));
        if let Err(code) = result {
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(
                &reply,
                &self.instance_id,
                &frame,
                code,
                (code == "UPSTREAM_HTTP").then_some(response.status),
            );
            return;
        }
        let cancel_hook = Arc::clone(&response.cancel);
        let installed = match (attempt.cancel_hook.lock(), attempt.response.lock()) {
            (Ok(mut cancel_slot), Ok(mut response_slot))
                if !attempt.cancelled.load(Ordering::Acquire) =>
            {
                *cancel_slot = Some(cancel_hook);
                *response_slot = Some(response.body);
                true
            }
            _ => false,
        };
        if !installed {
            (response.cancel)();
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(&reply, &self.instance_id, &frame, "CANCELLED", None);
            return;
        }
        if attempt.cancelled.load(Ordering::Acquire) {
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(&reply, &self.instance_id, &frame, "CANCELLED", None);
            return;
        }
        let mut head = reply_envelope(&self.instance_id, &frame, "gateway.response.head");
        head["status"] = json!(200);
        head["contentType"] = json!("text/event-stream");
        reply(head);
    }

    fn pull_response(
        self: &Arc<Self>,
        identity: GatewayRequestIdentity,
        sequence: u64,
        frame: &Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        let Some(attempt) = self.find_attempt(&identity) else {
            reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
            return Ok(());
        };
        let issue = attempt
            .state
            .lock()
            .map_err(|_| "REQUEST_INVALID")?
            .issue_pull(sequence);
        if let Err(code) = issue {
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(&reply, &self.instance_id, frame, code, None);
            return Ok(());
        }
        let broker = Arc::clone(self);
        let request_frame = frame.clone();
        thread::Builder::new()
            .name("agent-gateway-pull".to_string())
            .spawn(move || {
                broker.read_response_chunk(attempt, identity, sequence, request_frame, reply);
            })
            .map_err(|_| "gateway_worker_unavailable")?;
        Ok(())
    }

    fn read_response_chunk(
        &self,
        attempt: Arc<GatewayActiveAttempt>,
        identity: GatewayRequestIdentity,
        sequence: u64,
        frame: Value,
        reply: GatewayReplySink,
    ) {
        if attempt.cancelled.load(Ordering::Acquire) {
            self.finish_attempt(&identity.gateway_request_id, &attempt);
            reply_error(&reply, &self.instance_id, &frame, "CANCELLED", None);
            return;
        }
        if let Err(code) = self.ensure_attempt_connection_current(&identity) {
            self.revoke_run(&identity.conversation_id, &identity.run_id);
            reply_error(&reply, &self.instance_id, &frame, code, None);
            return;
        }
        let mut bytes = vec![0u8; MAX_CHUNK_BYTES];
        let read = match attempt.response.lock() {
            Ok(mut response) => match response.as_mut() {
                Some(response) => response.read(&mut bytes),
                None => Err(std::io::Error::other("response unavailable")),
            },
            Err(_) => Err(std::io::Error::other("response lock unavailable")),
        };
        match read {
            Ok(0) => {
                if let Err(code) = self.ensure_attempt_connection_current(&identity) {
                    self.revoke_run(&identity.conversation_id, &identity.run_id);
                    reply_error(&reply, &self.instance_id, &frame, code, None);
                    return;
                }
                let usage = match attempt.usage.lock() {
                    Ok(mut usage) => {
                        if usage.finish().is_err() {
                            self.finish_attempt(&identity.gateway_request_id, &attempt);
                            reply_error(&reply, &self.instance_id, &frame, "STREAM_FAILED", None);
                            return;
                        }
                        serde_json::to_value(usage.snapshot()).unwrap_or(Value::Null)
                    }
                    Err(_) => Value::Null,
                };
                let finished = attempt
                    .state
                    .lock()
                    .is_ok_and(|mut state| state.finish().is_ok());
                if !finished {
                    self.finish_attempt(&identity.gateway_request_id, &attempt);
                    reply_error(&reply, &self.instance_id, &frame, "STREAM_FAILED", None);
                    return;
                }
                self.finish_attempt(&identity.gateway_request_id, &attempt);
                let mut end = reply_envelope(&self.instance_id, &frame, "gateway.response.end");
                end["usage"] = usage;
                reply(end);
            }
            Ok(read) => {
                bytes.truncate(read);
                if let Err(code) = self.ensure_attempt_connection_current(&identity) {
                    self.revoke_run(&identity.conversation_id, &identity.run_id);
                    reply_error(&reply, &self.instance_id, &frame, code, None);
                    return;
                }
                let usage_result = attempt
                    .usage
                    .lock()
                    .map_err(|_| "STREAM_FAILED")
                    .and_then(|mut usage| usage.push(&bytes));
                let accept_result = usage_result.and_then(|()| {
                    attempt
                        .state
                        .lock()
                        .map_err(|_| "STREAM_FAILED")?
                        .accept_response_chunk(sequence, &bytes)
                });
                if let Err(code) = accept_result {
                    self.finish_attempt(&identity.gateway_request_id, &attempt);
                    reply_error(&reply, &self.instance_id, &frame, code, None);
                    return;
                }
                if attempt.cancelled.load(Ordering::Acquire) {
                    self.finish_attempt(&identity.gateway_request_id, &attempt);
                    reply_error(&reply, &self.instance_id, &frame, "CANCELLED", None);
                    return;
                }
                let mut chunk = reply_envelope(&self.instance_id, &frame, "gateway.response.chunk");
                chunk["pullSequence"] = json!(sequence);
                chunk["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
                reply(chunk);
            }
            Err(_) => {
                let code = if attempt.cancelled.load(Ordering::Acquire) {
                    "CANCELLED"
                } else {
                    "STREAM_FAILED"
                };
                self.finish_attempt(&identity.gateway_request_id, &attempt);
                reply_error(&reply, &self.instance_id, &frame, code, None);
            }
        }
    }

    fn cancel_attempt(
        &self,
        identity: GatewayRequestIdentity,
        frame: &Value,
        reply: GatewayReplySink,
    ) -> Result<(), &'static str> {
        let attempt = self
            .state
            .lock()
            .map_err(|_| "LEASE_REVOKED")?
            .attempts
            .remove(&identity.gateway_request_id);
        if let Some(attempt) = attempt {
            if !same_identity(&attempt.identity, &identity) {
                if let Ok(mut state) = self.state.lock() {
                    state
                        .attempts
                        .insert(attempt.identity.gateway_request_id.clone(), attempt);
                }
                reply_error(&reply, &self.instance_id, frame, "LEASE_REVOKED", None);
                return Ok(());
            }
            cancel_active_attempt(&attempt);
        }
        reply(reply_envelope(
            &self.instance_id,
            frame,
            "gateway.request.cancelled",
        ));
        Ok(())
    }

    fn lease_for(
        &self,
        identity: &GatewayRequestIdentity,
    ) -> Result<Option<Arc<Mutex<AgentGatewayLease>>>, &'static str> {
        let state = self.state.lock().map_err(|_| "LEASE_REVOKED")?;
        let Some(lease) = state.leases.get(&identity.lease_id).cloned() else {
            return Ok(None);
        };
        let matches = lease
            .lock()
            .map_err(|_| "LEASE_REVOKED")?
            .matches_scope(
                &self.instance_id,
                &identity.conversation_id,
                &identity.run_id,
                &identity.lease_id,
            )
            .is_ok();
        Ok(matches.then_some(lease))
    }

    /// Revalidate a live stream without reading the keyring. Connection edits
    /// revoke the entire run lease, so later pulls cannot continue on a stale
    /// binding or silently move to a replacement connection.
    fn ensure_attempt_connection_current(
        &self,
        identity: &GatewayRequestIdentity,
    ) -> Result<(), &'static str> {
        let lease = self.lease_for(identity)?.ok_or("LEASE_REVOKED")?;
        let (connection_id, revision, deadline) = {
            let lease = lease.lock().map_err(|_| "LEASE_REVOKED")?;
            let (connection_id, revision) = lease.connection_snapshot();
            (
                connection_id.to_string(),
                revision.to_string(),
                lease.deadline(),
            )
        };
        if Instant::now() >= deadline {
            return Err("TIMEOUT");
        }
        let connection = credentials::resolve_agent_model_connection_metadata(
            &self.db,
            Some((&connection_id, &revision)),
        )
        .map_err(|error| gateway_error_for_app(&error))?;
        let result = lease
            .lock()
            .map_err(|_| "LEASE_REVOKED")?
            .revalidate_connection(&connection);
        result
    }

    fn find_attempt(&self, identity: &GatewayRequestIdentity) -> Option<Arc<GatewayActiveAttempt>> {
        let state = self.state.lock().ok()?;
        let attempt = state.attempts.get(&identity.gateway_request_id)?.clone();
        same_identity(&attempt.identity, identity).then_some(attempt)
    }

    fn finish_attempt(&self, gateway_request_id: &str, attempt: &Arc<GatewayActiveAttempt>) {
        attempt.cancelled.store(true, Ordering::Release);
        if let Ok(mut state) = attempt.state.lock() {
            state.fail();
        }
        cancel_stream_resource(attempt);
        if let Ok(mut permit) = attempt.permit.lock() {
            permit.take();
        }
        if let Ok(mut state) = self.state.lock() {
            if state
                .attempts
                .get(gateway_request_id)
                .is_some_and(|current| Arc::ptr_eq(current, attempt))
            {
                state.attempts.remove(gateway_request_id);
            }
        }
    }
}

fn matches_lease(
    lease: &AgentGatewayLease,
    matches: &impl Fn(&GatewayRequestIdentity) -> bool,
) -> bool {
    matches(&GatewayRequestIdentity {
        conversation_id: lease.conversation_id.clone(),
        run_id: lease.run_id.clone(),
        lease_id: lease.lease_id.clone(),
        gateway_request_id: String::new(),
    })
}

fn identity_from_request(request: &HostGatewayFrame) -> GatewayRequestIdentity {
    match request {
        HostGatewayFrame::Open {
            conversation_id,
            run_id,
            lease_id,
            gateway_request_id,
            ..
        }
        | HostGatewayFrame::Chunk {
            conversation_id,
            run_id,
            lease_id,
            gateway_request_id,
            ..
        }
        | HostGatewayFrame::Commit {
            conversation_id,
            run_id,
            lease_id,
            gateway_request_id,
            ..
        }
        | HostGatewayFrame::Pull {
            conversation_id,
            run_id,
            lease_id,
            gateway_request_id,
            ..
        }
        | HostGatewayFrame::Cancel {
            conversation_id,
            run_id,
            lease_id,
            gateway_request_id,
            ..
        } => GatewayRequestIdentity {
            conversation_id: conversation_id.clone(),
            run_id: run_id.clone(),
            lease_id: lease_id.clone(),
            gateway_request_id: gateway_request_id.clone(),
        },
    }
}

fn same_identity(left: &GatewayRequestIdentity, right: &GatewayRequestIdentity) -> bool {
    left.conversation_id == right.conversation_id
        && left.run_id == right.run_id
        && left.lease_id == right.lease_id
        && left.gateway_request_id == right.gateway_request_id
}

fn gateway_attempt_sequence(id: &str, instance_id: &str) -> Option<u64> {
    let prefix = "gateway_attempt_";
    let suffix = format!("_{}", &instance_id[..instance_id.len().min(12)]);
    let sequence = id.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    if sequence.is_empty()
        || !sequence
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
    {
        return None;
    }
    let parsed = u64::from_str_radix(sequence, 36).ok()?;
    (parsed > 0 && base36(parsed) == sequence).then_some(parsed)
}

fn gateway_host_request_sequence(id: &str, instance_id: &str) -> Option<u64> {
    let suffix = format!("_{}", &instance_id[..instance_id.len().min(12)]);
    let sequence = id.strip_prefix("gateway_")?.strip_suffix(&suffix)?;
    if sequence.is_empty()
        || !sequence
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
    {
        return None;
    }
    let parsed = u64::from_str_radix(sequence, 36).ok()?;
    (parsed > 0 && base36(parsed) == sequence).then_some(parsed)
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut encoded = Vec::with_capacity(13);
    while value > 0 {
        encoded.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    encoded.reverse();
    String::from_utf8(encoded).expect("base36 digits are valid UTF-8")
}

fn reply_envelope(instance_id: &str, request: &Value, kind: &str) -> Value {
    json!({
        "protocol": 1,
        "instance": instance_id,
        "type": kind,
        "requestId": request["requestId"],
        "conversationId": request["conversationId"],
        "runId": request["runId"],
        "leaseId": request["leaseId"],
        "gatewayRequestId": request["gatewayRequestId"],
    })
}

fn reply_error(
    reply: &GatewayReplySink,
    instance_id: &str,
    request: &Value,
    code: &'static str,
    status: Option<u16>,
) {
    let mut frame = reply_envelope(instance_id, request, "gateway.response.error");
    frame["code"] = json!(allowed_gateway_error(code));
    if let Some(status) = status.filter(|status| (100..=599).contains(status)) {
        frame["status"] = json!(status);
    }
    reply(frame);
}

fn cancel_active_attempt(attempt: &GatewayActiveAttempt) {
    attempt.cancelled.store(true, Ordering::Release);
    if let Ok(mut state) = attempt.state.lock() {
        let _ = state.cancel();
    }
    cancel_stream_resource(attempt);
    if let Ok(mut permit) = attempt.permit.lock() {
        permit.take();
    }
}

fn cancel_stream_resource(attempt: &GatewayActiveAttempt) {
    let cancel = attempt
        .cancel_hook
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(cancel) = cancel {
        cancel();
    }
    if let Ok(mut response) = attempt.response.lock() {
        response.take();
    }
}

fn gateway_error_for_app(error: &crate::error::AppError) -> &'static str {
    match error.code() {
        "CONFLICT" | "NOT_FOUND" | "UNAUTHORIZED" => "CONNECTION_CHANGED",
        "CANCELLED" => "CANCELLED",
        "TIMEOUT" => "TIMEOUT",
        "INVALID_ARG" | "UNSUPPORTED" => "ROUTE_DENIED",
        _ => "UPSTREAM_UNAVAILABLE",
    }
}

pub(super) fn is_allowed_gateway_error(code: &str) -> bool {
    allowed_gateway_error(code) == code
}

fn allowed_gateway_error(code: &str) -> &'static str {
    match code {
        "ROUTE_DENIED" => "ROUTE_DENIED",
        "LEASE_REVOKED" => "LEASE_REVOKED",
        "CONNECTION_CHANGED" => "CONNECTION_CHANGED",
        "REQUEST_INVALID" => "REQUEST_INVALID",
        "REQUEST_LIMIT" => "REQUEST_LIMIT",
        "BUDGET_EXCEEDED" => "BUDGET_EXCEEDED",
        "UPSTREAM_UNAVAILABLE" => "UPSTREAM_UNAVAILABLE",
        "UPSTREAM_HTTP" => "UPSTREAM_HTTP",
        "REDIRECT_DENIED" => "REDIRECT_DENIED",
        "STREAM_FAILED" => "STREAM_FAILED",
        "CANCELLED" => "CANCELLED",
        "TIMEOUT" => "TIMEOUT",
        _ => "UPSTREAM_UNAVAILABLE",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        base36, gateway_host_request_sequence, parse_host_gateway_frame, validate_request,
        AgentGatewayLease, GatewayAttemptState, GatewayBroker, GatewayTransport,
        GatewayTransportResponse, GatewayUpstreamRequest, HostGatewayFrame, Route, SseUsageTracker,
        UsageSnapshot, MAX_CHUNK_BYTES, MAX_ENCODED_CHUNK_BYTES, MAX_REQUEST_BYTES,
        MAX_RUN_ATTEMPTS,
    };
    use crate::services::credentials::CredentialBackend;
    use crate::state::Database;
    use base64::Engine;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct FixtureCredentials {
        entries: Mutex<HashMap<String, String>>,
        reads: AtomicUsize,
    }

    impl CredentialBackend for FixtureCredentials {
        fn get(&self, id: &str) -> crate::error::AppResult<Option<String>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.entries.lock().unwrap().get(id).cloned())
        }

        fn save(&self, id: &str, secret: &str) -> crate::error::AppResult<()> {
            self.entries
                .lock()
                .unwrap()
                .insert(id.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, id: &str) -> crate::error::AppResult<()> {
            self.entries.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct ObservedRequest {
        endpoint: String,
        model: String,
        authorization: String,
        body: Vec<u8>,
    }

    #[derive(Default)]
    struct FixtureTransport {
        observed: Mutex<Option<ObservedRequest>>,
    }

    impl GatewayTransport for FixtureTransport {
        fn open(
            &self,
            request: GatewayUpstreamRequest,
        ) -> Result<GatewayTransportResponse, &'static str> {
            *self.observed.lock().unwrap() = Some(ObservedRequest {
                endpoint: request.endpoint,
                model: request.model,
                authorization: request.authorization.unwrap_or_default(),
                body: request.body,
            });
            Ok(GatewayTransportResponse {
                status: 200,
                content_type: "text/event-stream; charset=utf-8".to_string(),
                body: Box::new(Cursor::new(
                    b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n".to_vec(),
                )),
                cancel: Arc::new(|| {}),
            })
        }
    }

    struct BlockingTransport {
        read_started: mpsc::SyncSender<()>,
    }

    struct BlockingResponseBody {
        read_started: Option<mpsc::SyncSender<()>>,
        cancel_receiver: mpsc::Receiver<()>,
    }

    impl std::io::Read for BlockingResponseBody {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(started) = self.read_started.take() {
                let _ = started.send(());
            }
            self.cancel_receiver
                .recv()
                .map_err(|_| std::io::Error::other("fixture cancelled"))?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "fixture cancelled",
            ))
        }
    }

    impl GatewayTransport for BlockingTransport {
        fn open(
            &self,
            _request: GatewayUpstreamRequest,
        ) -> Result<GatewayTransportResponse, &'static str> {
            let (cancel_sender, cancel_receiver) = mpsc::channel();
            Ok(GatewayTransportResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                body: Box::new(BlockingResponseBody {
                    read_started: Some(self.read_started.clone()),
                    cancel_receiver,
                }),
                cancel: Arc::new(move || {
                    let _ = cancel_sender.send(());
                }),
            })
        }
    }

    fn fixture_broker(
        transport: Arc<dyn GatewayTransport>,
    ) -> (Arc<GatewayBroker>, Arc<FixtureCredentials>) {
        let connection_id = "connection_fixture_gateway_0001";
        let conn = crate::db::init_memory().unwrap();
        crate::db::ai_connections::upsert_with_limits(
            &conn,
            connection_id,
            "Gateway fixture",
            "cloud",
            "openai_chat",
            "https://api.example.test/v1",
            "fixture-model",
            Some(connection_id),
            1,
            10,
            20,
        )
        .unwrap();
        crate::db::ai_connections::bind_usage(&conn, "agent", connection_id).unwrap();
        let db = Arc::new(Database::new(conn));
        let credentials = Arc::new(FixtureCredentials::default());
        credentials
            .save(connection_id, "fixture-secret-never-on-stdio")
            .unwrap();
        let broker = Arc::new(GatewayBroker::new(
            "instance_0123456789abcdef".to_string(),
            db,
            credentials.clone(),
            transport,
        ));
        (broker, credentials)
    }

    fn broker_frame(sequence: u64, kind: &str, run_id: &str, lease_id: &str) -> serde_json::Value {
        let instance = "instance_0123456789abcdef";
        let suffix = &instance[..12];
        json!({
            "protocol": 1,
            "instance": instance,
            "sequence": sequence,
            "type": kind,
            "requestId": format!("gateway_{}_{}", base36(sequence), suffix),
            "conversationId": "conversation_0123456789abcdef",
            "runId": run_id,
            "leaseId": lease_id,
            "gatewayRequestId": "gateway_attempt_1_".to_string() + suffix,
        })
    }

    fn broker_exchange(broker: &Arc<GatewayBroker>, frame: serde_json::Value) -> serde_json::Value {
        let (sender, receiver) = mpsc::sync_channel(1);
        broker
            .handle_host_frame(
                frame,
                Arc::new(move |response| {
                    let _ = sender.send(response);
                }),
            )
            .unwrap();
        receiver.recv_timeout(Duration::from_secs(3)).unwrap()
    }

    #[test]
    fn broker_rejects_reused_host_request_ids_without_consuming_the_next_id() {
        let (broker, _) = fixture_broker(Arc::new(FixtureTransport::default()));
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();

        let mut open = broker_frame(1, "gateway.request.open", run_id, lease_id);
        open["route"] = json!("openai_chat");
        open["method"] = json!("POST");
        let request_id = open["requestId"].clone();
        assert_eq!(
            broker_exchange(&broker, open)["type"],
            "gateway.request.credit"
        );

        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"mock only"}],"stream":true,"max_tokens":2048}"#;
        let mut replay = broker_frame(2, "gateway.request.chunk", run_id, lease_id);
        replay["requestId"] = request_id;
        replay["chunkSequence"] = json!(0);
        replay["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(body));
        let rejected = broker_exchange(&broker, replay);
        assert_eq!(rejected["type"], "gateway.response.error");
        assert_eq!(rejected["code"], "REQUEST_INVALID");

        let mut next = broker_frame(2, "gateway.request.chunk", run_id, lease_id);
        next["chunkSequence"] = json!(0);
        next["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(body));
        assert_eq!(
            broker_exchange(&broker, next)["type"],
            "gateway.request.credit"
        );
    }

    fn commit_fixture_request(
        broker: &Arc<GatewayBroker>,
        run_id: &str,
        lease_id: &str,
    ) -> serde_json::Value {
        let mut open = broker_frame(1, "gateway.request.open", run_id, lease_id);
        open["route"] = json!("openai_chat");
        open["method"] = json!("POST");
        assert_eq!(
            broker_exchange(broker, open)["type"],
            "gateway.request.credit"
        );

        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"mock only"}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":2048}"#;
        let mut chunk = broker_frame(2, "gateway.request.chunk", run_id, lease_id);
        chunk["chunkSequence"] = json!(0);
        chunk["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(body));
        assert_eq!(
            broker_exchange(broker, chunk)["type"],
            "gateway.request.credit"
        );

        let mut commit = broker_frame(3, "gateway.request.commit", run_id, lease_id);
        commit["chunkCount"] = json!(1);
        commit["byteLength"] = json!(body.len());
        broker_exchange(broker, commit)
    }

    #[test]
    fn rust_broker_streams_a_fixture_response_and_keeps_the_credential_off_stdio() {
        let transport = Arc::new(FixtureTransport::default());
        let (broker, credentials) = fixture_broker(transport.clone());
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();
        let wire_lease = serde_json::to_string(&lease).unwrap();
        assert!(!wire_lease.contains("fixture-secret"));
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 0);

        let mut open = broker_frame(1, "gateway.request.open", run_id, lease_id);
        open["route"] = json!("openai_chat");
        open["method"] = json!("POST");
        let mut replies = vec![broker_exchange(&broker, open)];
        assert_eq!(replies[0]["type"], "gateway.request.credit");
        assert_eq!(replies[0]["nextChunk"], 0);

        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"mock only"}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":2048}"#;
        let mut chunk = broker_frame(2, "gateway.request.chunk", run_id, lease_id);
        chunk["chunkSequence"] = json!(0);
        chunk["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(body));
        replies.push(broker_exchange(&broker, chunk));
        assert_eq!(replies[1]["type"], "gateway.request.credit");
        assert_eq!(replies[1]["nextChunk"], 1);

        let mut commit = broker_frame(3, "gateway.request.commit", run_id, lease_id);
        commit["chunkCount"] = json!(1);
        commit["byteLength"] = json!(body.len());
        replies.push(broker_exchange(&broker, commit));
        assert_eq!(
            replies[2]["type"], "gateway.response.head",
            "commit reply: {:#}",
            replies[2]
        );
        assert_eq!(replies[2]["status"], 200);

        let mut pull = broker_frame(4, "gateway.response.pull", run_id, lease_id);
        pull["pullSequence"] = json!(0);
        replies.push(broker_exchange(&broker, pull));
        assert_eq!(replies[3]["type"], "gateway.response.chunk");
        let streamed = base64::engine::general_purpose::STANDARD
            .decode(replies[3]["base64"].as_str().unwrap())
            .unwrap();
        assert!(String::from_utf8(streamed)
            .unwrap()
            .contains("prompt_tokens\":2"));

        let mut pull_end = broker_frame(5, "gateway.response.pull", run_id, lease_id);
        pull_end["pullSequence"] = json!(1);
        replies.push(broker_exchange(&broker, pull_end));
        assert_eq!(replies[4]["type"], "gateway.response.end");
        assert_eq!(replies[4]["usage"]["inputTokens"], 2);
        assert_eq!(replies[4]["usage"]["outputTokens"], 1);
        assert!(replies
            .iter()
            .all(|frame| !frame.to_string().contains("fixture-secret-never-on-stdio")));

        let observed = transport.observed.lock().unwrap().take().unwrap();
        assert_eq!(
            observed.endpoint,
            "https://api.example.test/v1/chat/completions"
        );
        assert_eq!(observed.model, "fixture-model");
        assert_eq!(
            observed.authorization,
            "Bearer fixture-secret-never-on-stdio"
        );
        assert_eq!(observed.body, body);
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 1);
        broker.revoke_run(conversation_id, run_id);
        assert!(broker.state.lock().unwrap().leases.is_empty());
    }

    #[test]
    fn disabled_production_transport_rejects_before_reading_credentials() {
        let (broker, credentials) = fixture_broker(Arc::new(super::DisabledGatewayTransport));
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let response = commit_fixture_request(&broker, run_id, lease["leaseId"].as_str().unwrap());

        assert_eq!(response["type"], "gateway.response.error");
        assert_eq!(response["code"], "UPSTREAM_UNAVAILABLE");
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 0);
        assert_eq!(broker.lease_count(), 1);
    }

    #[test]
    fn connection_revision_change_revokes_an_open_stream_before_next_chunk() {
        let transport = Arc::new(FixtureTransport::default());
        let (broker, credentials) = fixture_broker(transport);
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();
        let response = commit_fixture_request(&broker, run_id, lease_id);
        assert_eq!(response["type"], "gateway.response.head");

        {
            let conn = broker.db.lock().unwrap();
            crate::db::ai_connections::upsert_with_limits(
                &conn,
                "connection_fixture_gateway_0001",
                "Gateway fixture",
                "cloud",
                "openai_chat",
                "https://api.example.test/v1",
                "replacement-model",
                Some("connection_fixture_gateway_0001"),
                1,
                10,
                20,
            )
            .unwrap();
        }

        let mut pull = broker_frame(4, "gateway.response.pull", run_id, lease_id);
        pull["pullSequence"] = json!(0);
        let response = broker_exchange(&broker, pull);
        assert_eq!(response["type"], "gateway.response.error");
        assert_eq!(response["code"], "CONNECTION_CHANGED");
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 1);
        assert_eq!(broker.lease_count(), 0);
    }

    #[test]
    fn cancelling_a_blocked_stream_interrupts_read_and_replies_promptly() {
        let (started_sender, started_receiver) = mpsc::sync_channel(1);
        let transport = Arc::new(BlockingTransport {
            read_started: started_sender,
        });
        let (broker, _) = fixture_broker(transport);
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();
        let response = commit_fixture_request(&broker, run_id, lease_id);
        assert_eq!(response["type"], "gateway.response.head");

        let mut pull = broker_frame(4, "gateway.response.pull", run_id, lease_id);
        pull["pullSequence"] = json!(0);
        let (pull_sender, pull_receiver) = mpsc::sync_channel(1);
        let pull_broker = Arc::clone(&broker);
        std::thread::spawn(move || {
            pull_broker
                .handle_host_frame(
                    pull,
                    Arc::new(move |reply| {
                        let _ = pull_sender.send(reply);
                    }),
                )
                .unwrap();
        });
        started_receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap();

        let cancel = broker_frame(5, "gateway.request.cancel", run_id, lease_id);
        let response = broker_exchange(&broker, cancel);
        assert_eq!(response["type"], "gateway.request.cancelled");
        assert_eq!(
            pull_receiver.recv_timeout(Duration::from_secs(3)).unwrap()["code"],
            "CANCELLED"
        );
    }

    fn agent_connection() -> crate::db::ai_connections::AiConnection {
        crate::db::ai_connections::AiConnection {
            id: "connection_fixture_0123456789".to_string(),
            name: "Local fixture".to_string(),
            deployment: "cloud".to_string(),
            protocol: "openai_chat".to_string(),
            base_url: "http://127.0.0.1:43129".to_string(),
            model: "fixture-model".to_string(),
            api_key_ref: Some("connection_fixture_0123456789".to_string()),
            config_revision: "revision_fixture_0123456789".to_string(),
            max_concurrency: 1,
            requests_per_minute: 10,
            requests_per_hour: 20,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn gateway_frame(kind: &str) -> serde_json::Value {
        json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 1,
            "requestId": "request_0123456789abcdef",
            "type": kind,
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "leaseId": "lease_0123456789abcdef",
            "gatewayRequestId": "gateway_0123456789abcdef",
        })
    }

    #[test]
    fn agent_lease_wire_payload_contains_only_model_codec_metadata() {
        let now = Instant::now();
        let connection = agent_connection();
        let lease = AgentGatewayLease::issue_at(
            "instance_0123456789abcdef",
            "conversation_0123456789abcdef",
            "run_0123456789abcdef",
            &connection,
            now,
        )
        .unwrap();
        let payload = lease.wire_payload();
        let object = payload.as_object().unwrap();

        assert_eq!(object.len(), 4);
        assert_eq!(payload["provider"], "openai");
        assert_eq!(payload["protocol"], "openai_chat");
        assert_eq!(payload["model"], "fixture-model");
        assert!(payload["leaseId"].as_str().unwrap().starts_with("lease_"));
        for forbidden in [
            "connectionId",
            "configRevision",
            "baseUrl",
            "apiKeyRef",
            "credential",
        ] {
            assert!(!object.contains_key(forbidden));
        }
        assert_eq!(
            lease.matches_scope(
                "instance_0123456789abcdef",
                "conversation_0123456789abcdef",
                "run_0123456789abcdef",
                payload["leaseId"].as_str().unwrap(),
            ),
            Ok(())
        );
        assert_eq!(
            lease.matches_scope(
                "instance_0123456789abcdef",
                "conversation_other_0123456789",
                "run_0123456789abcdef",
                payload["leaseId"].as_str().unwrap(),
            ),
            Err("LEASE_REVOKED")
        );
    }

    #[test]
    fn lease_rechecks_binding_revision_enabled_protocol_and_model_before_budget() {
        let now = Instant::now();
        let connection = agent_connection();
        let mut lease = AgentGatewayLease::issue_at(
            "instance_0123456789abcdef",
            "conversation_0123456789abcdef",
            "run_0123456789abcdef",
            &connection,
            now,
        )
        .unwrap();
        let mut changed = connection.clone();
        changed.config_revision.push_str("_changed");
        assert_eq!(
            lease.consume_attempt_if_current(&changed, false, now),
            Err("CONNECTION_CHANGED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        let mut rebound = connection.clone();
        rebound.id = "another_connection_0123456789".to_string();
        assert_eq!(
            lease.consume_attempt_if_current(&rebound, false, now),
            Err("CONNECTION_CHANGED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        let mut disabled = connection.clone();
        disabled.enabled = false;
        assert_eq!(
            lease.consume_attempt_if_current(&disabled, false, now),
            Err("LEASE_REVOKED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        let mut changed_model = connection.clone();
        changed_model.model = "different-model".to_string();
        assert_eq!(
            lease.consume_attempt_if_current(&changed_model, false, now),
            Err("CONNECTION_CHANGED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        let mut changed_protocol = connection.clone();
        changed_protocol.protocol = "anthropic_messages".to_string();
        assert_eq!(
            lease.consume_attempt_if_current(&changed_protocol, false, now),
            Err("CONNECTION_CHANGED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        assert_eq!(
            lease.consume_attempt_if_current(&connection, true, now),
            Err("CANCELLED")
        );
        assert_eq!(lease.budget.attempts_used(), 0);

        assert_eq!(
            lease.consume_attempt_if_current(&connection, false, now),
            Ok(1)
        );
        assert_eq!(lease.budget.attempts_used(), 1);
    }

    #[test]
    fn lease_issue_fails_closed_for_disabled_and_unlisted_connection_protocols() {
        let now = Instant::now();
        let mut disabled = agent_connection();
        disabled.enabled = false;
        assert_eq!(
            AgentGatewayLease::issue_at(
                "instance_0123456789abcdef",
                "conversation_0123456789abcdef",
                "run_0123456789abcdef",
                &disabled,
                now,
            )
            .err(),
            Some("LEASE_REVOKED")
        );

        let mut unsupported = agent_connection();
        unsupported.protocol = "openai_responses".to_string();
        assert_eq!(
            AgentGatewayLease::issue_at(
                "instance_0123456789abcdef",
                "conversation_0123456789abcdef",
                "run_0123456789abcdef",
                &unsupported,
                now,
            )
            .err(),
            Some("ROUTE_DENIED")
        );
    }

    #[test]
    fn gateway_open_frame_accepts_only_fixed_post_routes_and_exact_fields() {
        let mut frame = gateway_frame("gateway.request.open");
        frame["route"] = json!("openai_chat");
        frame["method"] = json!("POST");
        assert!(matches!(
            parse_host_gateway_frame(&frame),
            Ok(HostGatewayFrame::Open {
                route: Route::OpenAiChat,
                ..
            })
        ));

        frame["route"] = json!("https://example.invalid");
        assert_eq!(parse_host_gateway_frame(&frame), Err("ROUTE_DENIED"));
        frame["route"] = json!("anthropic_messages");
        frame["method"] = json!("GET");
        assert_eq!(parse_host_gateway_frame(&frame), Err("ROUTE_DENIED"));
        frame["method"] = json!("POST");
        frame["url"] = json!("https://example.invalid/steal");
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_INVALID"));
    }

    #[test]
    fn gateway_upload_chunks_are_canonical_bounded_base64_with_typed_sequences() {
        let bytes = b"fixture request chunk";
        let mut frame = gateway_frame("gateway.request.chunk");
        frame["chunkSequence"] = json!(0);
        frame["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
        assert!(matches!(
            parse_host_gateway_frame(&frame),
            Ok(HostGatewayFrame::Chunk { bytes: decoded, .. }) if decoded == bytes
        ));

        frame["base64"] = json!("%%%%");
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_INVALID"));
        frame["base64"] = json!("x".repeat(MAX_ENCODED_CHUNK_BYTES + 1));
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_LIMIT"));

        frame["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(vec![
            b'x';
            MAX_CHUNK_BYTES
                + 1
        ]));
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_LIMIT"));
    }

    #[test]
    fn gateway_commit_is_bounded_and_pull_cancel_are_exact_field_frames() {
        let mut commit = gateway_frame("gateway.request.commit");
        commit["chunkCount"] = json!(1);
        commit["byteLength"] = json!(8);
        assert!(matches!(
            parse_host_gateway_frame(&commit),
            Ok(HostGatewayFrame::Commit {
                chunk_count: 1,
                byte_length: 8,
                ..
            })
        ));
        commit["byteLength"] = json!(MAX_REQUEST_BYTES + 1);
        assert_eq!(parse_host_gateway_frame(&commit), Err("REQUEST_LIMIT"));

        let mut pull = gateway_frame("gateway.response.pull");
        pull["pullSequence"] = json!(0);
        assert!(matches!(
            parse_host_gateway_frame(&pull),
            Ok(HostGatewayFrame::Pull {
                pull_sequence: 0,
                ..
            })
        ));
        pull["extra"] = json!(true);
        assert_eq!(parse_host_gateway_frame(&pull), Err("REQUEST_INVALID"));

        let cancel = gateway_frame("gateway.request.cancel");
        assert!(matches!(
            parse_host_gateway_frame(&cancel),
            Ok(HostGatewayFrame::Cancel { .. })
        ));
    }

    #[test]
    fn gateway_frame_identities_must_be_opaque_tokens() {
        let mut frame = gateway_frame("gateway.request.cancel");
        frame["leaseId"] = json!("C:\\Users\\private");
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_INVALID"));
        frame["leaseId"] = json!("lease_0123456789abcdef");
        frame["protocol"] = json!(2);
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_INVALID"));
        frame["protocol"] = json!(1);
        frame["instance"] = json!("short");
        assert_eq!(parse_host_gateway_frame(&frame), Err("REQUEST_INVALID"));

        assert_eq!(
            gateway_host_request_sequence("gateway_1_instance_012", "instance_0123456789abcdef"),
            Some(1)
        );
        assert_eq!(
            gateway_host_request_sequence("gateway_2_instance_012", "instance_0123456789abcdef"),
            Some(2)
        );
        assert_eq!(
            gateway_host_request_sequence("gateway_01_instance_012", "instance_0123456789abcdef"),
            None
        );
    }

    #[test]
    fn upload_credit_and_commit_enforce_contiguous_chunks_and_strict_request_body() {
        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"hello fixture"}],"stream":true,"max_tokens":2048}"#;
        let mut attempt = GatewayAttemptState::new(Route::OpenAiChat, "fixture-model").unwrap();
        assert_eq!(attempt.issue_chunk_credit(), Ok((0, MAX_CHUNK_BYTES)));
        assert_eq!(attempt.issue_chunk_credit(), Err("REQUEST_INVALID"));
        assert_eq!(attempt.accept_chunk(1, body), Err("REQUEST_INVALID"));
        attempt.accept_chunk(0, body).unwrap();
        assert_eq!(attempt.accept_chunk(1, body), Err("REQUEST_INVALID"));
        assert_eq!(
            attempt.commit_body(1, body.len() + 1),
            Err("REQUEST_INVALID")
        );
        assert_eq!(attempt.commit_body(2, body.len()), Err("REQUEST_INVALID"));
        assert_eq!(attempt.commit_body(1, body.len()).unwrap(), body);
        assert_eq!(attempt.issue_chunk_credit(), Err("REQUEST_INVALID"));
    }

    #[test]
    fn gateway_attempt_pulls_one_response_chunk_at_a_time_and_ends_once() {
        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":1}"#;
        let mut attempt = GatewayAttemptState::new(Route::OpenAiChat, "fixture-model").unwrap();
        attempt.issue_chunk_credit().unwrap();
        attempt.accept_chunk(0, body).unwrap();
        attempt.commit_body(1, body.len()).unwrap();
        attempt
            .begin_streaming(200, " Text/Event-Stream ; charset=utf-8 ")
            .unwrap();

        attempt.issue_pull(0).unwrap();
        assert_eq!(attempt.issue_pull(1), Err("REQUEST_INVALID"));
        assert_eq!(
            attempt.accept_response_chunk(1, b"x"),
            Err("REQUEST_INVALID")
        );
        attempt.accept_response_chunk(0, b"fixture chunk").unwrap();
        attempt.issue_pull(1).unwrap();
        attempt.accept_response_chunk(1, b"fixture end").unwrap();
        attempt.finish().unwrap();
        assert_eq!(attempt.issue_pull(2), Err("REQUEST_INVALID"));
        assert_eq!(attempt.finish(), Err("REQUEST_INVALID"));
    }

    #[test]
    fn redirect_and_cancel_are_terminal_and_drop_buffered_request_data() {
        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":1}"#;
        let mut redirected = GatewayAttemptState::new(Route::OpenAiChat, "fixture-model").unwrap();
        redirected.issue_chunk_credit().unwrap();
        redirected.accept_chunk(0, body).unwrap();
        redirected.commit_body(1, body.len()).unwrap();
        assert_eq!(
            redirected.begin_streaming(302, "text/event-stream"),
            Err("REDIRECT_DENIED")
        );
        assert_eq!(redirected.issue_pull(0), Err("REQUEST_INVALID"));

        let mut cancelled = GatewayAttemptState::new(Route::OpenAiChat, "fixture-model").unwrap();
        cancelled.issue_chunk_credit().unwrap();
        cancelled.accept_chunk(0, body).unwrap();
        cancelled.cancel().unwrap();
        assert!(cancelled.request_body.is_empty());
        assert_eq!(cancelled.commit_body(1, body.len()), Err("REQUEST_INVALID"));
        assert_eq!(cancelled.cancel(), Err("REQUEST_INVALID"));
    }

    #[test]
    fn run_budget_only_consumes_eight_attempts_before_its_fixed_deadline() {
        let now = Instant::now();
        let mut budget = super::RunBudget::new_at(now, Duration::from_secs(600));
        assert_eq!(budget.deadline(), now + super::RUN_TIMEOUT);
        for attempt in 1..=MAX_RUN_ATTEMPTS {
            assert_eq!(budget.consume_attempt_at(now), Ok(attempt));
        }
        assert_eq!(budget.attempts_used(), MAX_RUN_ATTEMPTS);
        assert_eq!(budget.consume_attempt_at(now), Err("BUDGET_EXCEEDED"));

        let mut expired = super::RunBudget::new_at(now, Duration::ZERO);
        assert_eq!(expired.consume_attempt_at(now), Err("TIMEOUT"));
        assert_eq!(expired.attempts_used(), 0);
    }

    #[test]
    fn accepts_the_exact_pi_openai_chat_fixture_body() {
        let body = r#"{"model":"fixture-model","messages":[{"role":"system","content":"茶馆隔离 Host。当前运行仅使用离线测试模型，不提供业务工具。"},{"role":"user","content":[{"type":"text","text":"hello fixture"}]}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":2048}"#.as_bytes();
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", body),
            Ok(())
        );
    }

    #[test]
    fn accepts_the_exact_pi_anthropic_messages_fixture_body() {
        let body = br#"{"model":"fixture-model","messages":[{"role":"user","content":"hello fixture"}],"max_tokens":2048,"stream":true}"#;
        assert_eq!(
            validate_request(Route::AnthropicMessages, "fixture-model", body),
            Ok(())
        );
    }

    #[test]
    fn rejects_non_text_unknown_fields_wrong_model_and_unbounded_tokens() {
        let cases = [
            r#"{"model":"fixture-model","messages":[{"role":"user","content":[{"type":"image","source":{"data":"x"}}]}],"stream":true,"max_tokens":10}"#,
            r#"{"model":"other","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":2049}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"tools":[]}"#,
        ];
        for body in cases {
            assert_eq!(
                validate_request(Route::AnthropicMessages, "fixture-model", body.as_bytes()),
                Err("REQUEST_INVALID")
            );
        }

        let openai_image = br#"{"model":"fixture-model","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]}],"stream":true,"max_tokens":10}"#;
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", openai_image),
            Err("REQUEST_INVALID")
        );
    }

    #[test]
    fn rejects_malformed_and_oversized_request_bodies() {
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", b"{"),
            Err("REQUEST_INVALID")
        );
        assert_eq!(
            validate_request(
                Route::OpenAiChat,
                "fixture-model",
                &vec![b' '; MAX_REQUEST_BYTES + 1]
            ),
            Err("REQUEST_LIMIT")
        );
    }

    #[test]
    fn closed_routes_cannot_be_selected_by_host_strings() {
        assert_eq!(Route::from_wire("openai_chat"), Ok(Route::OpenAiChat));
        assert_eq!(
            Route::from_wire("anthropic_messages"),
            Ok(Route::AnthropicMessages)
        );
        assert_eq!(
            Route::from_wire("https://example.invalid"),
            Err("ROUTE_DENIED")
        );
    }

    #[test]
    fn openai_sse_usage_is_parsed_across_arbitrary_chunk_boundaries() {
        let stream = b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":17,\"completion_tokens\":5}}\r\n\r\ndata: [DONE]\n\n";
        let mut tracker = SseUsageTracker::new(Route::OpenAiChat);
        for chunk in stream.chunks(3) {
            tracker.push(chunk).unwrap();
        }
        tracker.finish().unwrap();

        assert_eq!(
            tracker.snapshot(),
            Some(UsageSnapshot {
                input_tokens: Some(17),
                output_tokens: Some(5),
                cache_read_tokens: None,
                cache_write_tokens: None,
            })
        );
    }

    #[test]
    fn anthropic_sse_usage_combines_message_start_and_delta_counters() {
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":23,\"cache_read_input_tokens\":4,\"cache_creation_input_tokens\":2}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut tracker = SseUsageTracker::new(Route::AnthropicMessages);
        for chunk in stream.as_bytes().chunks(11) {
            tracker.push(chunk).unwrap();
        }
        tracker.finish().unwrap();

        assert_eq!(
            tracker.snapshot(),
            Some(UsageSnapshot {
                input_tokens: Some(23),
                output_tokens: Some(9),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(2),
            })
        );
    }

    #[test]
    fn usage_is_unknown_when_missing_incomplete_invalid_or_overlong() {
        let mut missing = SseUsageTracker::new(Route::OpenAiChat);
        missing.push(b"data: {\"choices\":[]}\n\n").unwrap();
        assert_eq!(missing.snapshot(), None);

        let mut incomplete = SseUsageTracker::new(Route::AnthropicMessages);
        incomplete
            .push(b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n")
            .unwrap();
        assert_eq!(incomplete.snapshot(), None);

        let mut invalid = SseUsageTracker::new(Route::OpenAiChat);
        invalid
            .push(b"data: {\"usage\":{\"prompt_tokens\":-1,\"completion_tokens\":0}}\n\n")
            .unwrap();
        invalid.finish().unwrap();
        assert_eq!(invalid.snapshot(), None);

        let mut overlong = SseUsageTracker::new(Route::OpenAiChat);
        assert_eq!(
            overlong.push(&vec![b'x'; super::MAX_SSE_LINE_BYTES + 1]),
            Err("STREAM_FAILED")
        );
    }
}
