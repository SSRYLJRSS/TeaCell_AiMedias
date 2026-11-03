//! Strict request-body validation and lease-bound brokering for A3-02.
//!
//! The broker validates Pi 1.0.4 text routes, resolves credentials only inside
//! Rust, and sends requests through an injected transport. Production installs a
//! `SwitchableGatewayTransport` that always forwards once a service is bound and
//! its credential resolves — there is no user-visible online/offline switch
//! (2026-11-01 ruling); only Rust-owned proxy configuration can be swapped live.
//! Tests inject deterministic in-memory fixtures.

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{AppError, AppResult};
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
/// Upper bound (ceiling) for a single turn's `max_tokens`. The Host requests an
/// adaptive budget (default 4096) that fails safe under long contexts; 8192 is
/// the hard ceiling a leased request may ever declare. Raise together with
/// docs (plans/v2/06-permissions-and-data-safety.md §8.1).
pub const MAX_OUTPUT_TOKENS: u64 = 8192;
const MAX_DECLARED_TOOLS: usize = 20;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 4096;
const MAX_TOOL_CALLS: usize = 20;
const MAX_TOOL_CALL_ID_BYTES: usize = 128;
const MAX_TOOL_ARGUMENTS_BYTES: usize = 32 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 32 * 1024;
const MAX_REASONING_DETAILS: usize = 64;
const MAX_REASONING_DETAIL_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 64 * 1024;

/// Conservative model-window default when a connection has no sane value.
const DEFAULT_CONTEXT_WINDOW: i64 = 32768;
/// Adaptive per-turn reply budget default (must stay ≤ MAX_OUTPUT_TOKENS).
const DEFAULT_OUTPUT_BUDGET: i64 = 4096;
const MAX_CONTEXT_WINDOW: i64 = 4_000_000;

fn normalized_context_window(value: i64) -> u32 {
    if (1024..=MAX_CONTEXT_WINDOW).contains(&value) {
        value as u32
    } else {
        DEFAULT_CONTEXT_WINDOW as u32
    }
}

fn normalized_output_budget(value: i64) -> u32 {
    let ceiling = MAX_OUTPUT_TOKENS as i64;
    if (1..=ceiling).contains(&value) {
        value as u32
    } else {
        DEFAULT_OUTPUT_BUDGET.min(ceiling) as u32
    }
}

/// Thinking levels recognized end-to-end (`off`..`xhigh`). Anything else is
/// dropped rather than forwarded, so an invalid UI value cannot reach a provider.
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];

fn normalized_thinking_level(level: Option<&str>) -> Option<String> {
    level
        .map(str::trim)
        .filter(|value| THINKING_LEVELS.contains(value))
        .map(str::to_ascii_lowercase)
}

/// Keys that must never appear in a chat request body, compared after
/// normalization (lowercase + strip every non-alphanumeric character) so that
/// `Authorization`, `api-key`, `api_key` and `x-api-key` all collapse onto a
/// covered token. The scan is deliberately shallow (only the direct keys of the
/// top-level object, each message, each content block and each tool envelope),
/// never descending into tool `parameters`/`input_schema` so that legitimate
/// argument names are not false-positived.
const BLACKLIST_KEYS: &[&str] = &[
    "apikey",
    "authorization",
    "auth",
    "headers",
    "header",
    "url",
    "endpoint",
    "baseurl",
    "baseuri",
    "proxy",
    "httpproxy",
    "httpsproxy",
    "noproxy",
    "secret",
    "password",
    "passwd",
    "credential",
    "credentials",
    "token",
    "accesstoken",
    "refreshtoken",
    "bearer",
];

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn reject_blacklisted_keys(object: &Map<String, Value>) -> Result<(), &'static str> {
    for key in object.keys() {
        if BLACKLIST_KEYS.contains(&normalize_key(key).as_str()) {
            return Err("REQUEST_INVALID");
        }
    }
    Ok(())
}

/// Reject parameters that amplify cost beyond the per-attempt token budget or
/// legacy function-calling shapes that would bypass the registered tool names.
fn reject_cost_or_legacy(object: &Map<String, Value>) -> Result<(), &'static str> {
    if object.contains_key("functions") || object.contains_key("function_call") {
        return Err("REQUEST_INVALID");
    }
    if object.contains_key("best_of") {
        return Err("REQUEST_INVALID");
    }
    if let Some(n) = object.get("n") {
        if n.as_u64() != Some(1) {
            return Err("REQUEST_INVALID");
        }
    }
    Ok(())
}

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
        // 与连接测试、打标、超级搜索同一规则：用户填写的 base URL 就是连接
        // 测试验证过的地址，接口路径直接追加，不再自行补 `/v1`。自动补 `/v1`
        // 会让智谱（`/api/paas/v4`）、火山方舟（`/api/v3`）等非 `/v1` 路径的
        // 服务在助手里 404，而同一服务在打标里却正常。
        let path = format!("{path}{suffix}");
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
    /// Real model context window from the connection (Host declares it to Pi so
    /// pi-ai clamps the reply budget against the true window, not a guess).
    context_window: u32,
    /// Adaptive per-turn reply budget for this model (never above MAX_OUTPUT_TOKENS).
    max_output_tokens: u32,
    /// Whether the connection declares reasoning support; gates thinking-level passthrough.
    reasoning: bool,
    /// Optional per-run thinking level (off/minimal/low/medium/high/xhigh). Only
    /// forwarded when the connection supports reasoning; else dropped.
    thinking_level: Option<String>,
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
            context_window: normalized_context_window(connection.context_window),
            max_output_tokens: normalized_output_budget(connection.max_output_tokens),
            reasoning: connection.reasoning_supported,
            thinking_level: None,
            budget: RunBudget::new_at(now, RUN_TIMEOUT),
        })
    }

    /// The exact metadata the Pi SDK needs to select its registered codec and
    /// configure its window/reply budget. Credentials, URL, connection ID, and
    /// config revision never cross stdio.
    pub fn wire_payload(&self) -> Value {
        let mut payload = json!({
            "leaseId": self.lease_id,
            "provider": self.route.provider(),
            "protocol": self.route.protocol(),
            "model": self.model,
            "contextWindow": self.context_window,
            "maxOutputTokens": self.max_output_tokens,
            "reasoning": self.reasoning,
        });
        if self.reasoning {
            if let (Some(level), Some(object)) =
                (self.thinking_level.as_ref(), payload.as_object_mut())
            {
                object.insert("thinkingLevel".to_string(), Value::String(level.clone()));
            }
        }
        payload
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

/// Validate a Pi request body against the leased model identity and the
/// conversation-layer blacklist policy.
///
/// The conversation layer is now a **denylist + passthrough**: unknown message
/// and generation parameters are forwarded to the user's own provider, while
/// credentials/endpoints/headers, cost amplifiers and legacy function shapes are
/// rejected, and message `content` stays text-only so the untrusted Node host can
/// never base64-exfiltrate arbitrary local bytes. Capability safety is enforced
/// downstream (`capabilities::execute` + permission + approval), not here.
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
    // Denylist + passthrough: no top-level allowlist. Required identity/stream
    // fields are checked by `require_model_stream`, `messages` by
    // `validate_messages`, and the token budget by `validate_token_limit`.
    reject_blacklisted_keys(object)?;
    reject_cost_or_legacy(object)?;
    require_model_stream(object, leased_model)?;
    validate_messages(object.get("messages").ok_or("REQUEST_INVALID")?, true)?;
    if let Some(tools) = object.get("tools") {
        validate_declared_tools(tools, Route::OpenAiChat)?;
    }

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
    // Denylist + passthrough, same policy as the OpenAI route. `max_tokens` is
    // still required (checked by `validate_token_limit`), and `system` stays
    // text-only.
    reject_blacklisted_keys(object)?;
    reject_cost_or_legacy(object)?;
    require_model_stream(object, leased_model)?;
    validate_token_limit(object, &["max_tokens"])?;
    validate_messages(object.get("messages").ok_or("REQUEST_INVALID")?, false)?;
    if let Some(tools) = object.get("tools") {
        validate_declared_tools(tools, Route::AnthropicMessages)?;
    }
    if let Some(system) = object.get("system") {
        validate_text_blocks(system, MAX_SYSTEM_BLOCKS)?;
    }
    if let Some(temperature) = object.get("temperature") {
        validate_temperature(temperature, 1.0)?;
    }
    Ok(())
}

/// Tool declarations keep the capability gate (registered names, unique, bounded
/// description) but the argument schema is forwarded as an arbitrary JSON object
/// (`parameters`/`input_schema`); real safety lives in `capabilities::execute`.
/// An empty `tools` array is allowed because pi emits `tools: []` when a turn has
/// tool history but no request tools.
fn validate_declared_tools(value: &Value, route: Route) -> Result<(), &'static str> {
    let tools = value.as_array().ok_or("REQUEST_INVALID")?;
    if tools.len() > MAX_DECLARED_TOOLS {
        return Err("REQUEST_INVALID");
    }
    let registered = super::capabilities::supported_tool_names();
    let mut seen: Vec<&str> = Vec::with_capacity(tools.len());
    for tool in tools {
        let object = tool.as_object().ok_or("REQUEST_INVALID")?;
        let (name, description, schema) = match route {
            Route::OpenAiChat => {
                require_fields(object, &["type", "function"], &["type", "function"])?;
                if object.get("type").and_then(Value::as_str) != Some("function") {
                    return Err("REQUEST_INVALID");
                }
                let function = object
                    .get("function")
                    .and_then(Value::as_object)
                    .ok_or("REQUEST_INVALID")?;
                require_fields(
                    function,
                    &["name", "description", "parameters"],
                    &["name", "description", "parameters"],
                )?;
                (
                    function.get("name").and_then(Value::as_str),
                    function.get("description").and_then(Value::as_str),
                    function.get("parameters"),
                )
            }
            Route::AnthropicMessages => {
                require_fields(
                    object,
                    &[
                        "name",
                        "description",
                        "eager_input_streaming",
                        "input_schema",
                    ],
                    &["name", "description", "input_schema"],
                )?;
                (
                    object.get("name").and_then(Value::as_str),
                    object.get("description").and_then(Value::as_str),
                    object.get("input_schema"),
                )
            }
        };
        let name = name.ok_or("REQUEST_INVALID")?;
        let description = description.ok_or("REQUEST_INVALID")?;
        let schema = schema.and_then(Value::as_object).ok_or("REQUEST_INVALID")?;
        if !registered.contains(&name) || seen.contains(&name) {
            return Err("REQUEST_INVALID");
        }
        seen.push(name);
        if description.is_empty()
            || description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || description.contains('\0')
        {
            return Err("REQUEST_INVALID");
        }
        let encoded = serde_json::to_vec(schema).map_err(|_| "REQUEST_INVALID")?;
        if encoded.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err("REQUEST_LIMIT");
        }
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
        if openai_roles {
            validate_openai_message(object)?;
        } else {
            validate_anthropic_message(object)?;
        }
    }
    Ok(())
}

/// OpenAI Chat messages: `system`/`developer`/`user`/`assistant`, plus the
/// `tool` result role that Pi sends after a capability call. Message-level extra
/// keys are forwarded (denylist-filtered) so reasoning metadata such as
/// `reasoning_details`/`reasoning`/`reasoning_content` can be replayed, but
/// `content` stays text-only and tool calls still go through the capability gate.
fn validate_openai_message(object: &Map<String, Value>) -> Result<(), &'static str> {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !matches!(role, "system" | "developer" | "user" | "assistant" | "tool") {
        return Err("REQUEST_INVALID");
    }
    if !object.contains_key("content") {
        return Err("REQUEST_INVALID");
    }
    reject_blacklisted_keys(object)?;
    match role {
        "system" | "developer" => {
            if !object.get("content").is_some_and(Value::is_string) {
                return Err("REQUEST_INVALID");
            }
        }
        "user" => {
            let content = object.get("content").ok_or("REQUEST_INVALID")?;
            if !content.is_string() {
                validate_text_blocks(content, MAX_MESSAGES)?;
            }
        }
        "assistant" => validate_openai_assistant(object)?,
        "tool" => validate_openai_tool_result(object)?,
        _ => return Err("REQUEST_INVALID"),
    }
    Ok(())
}

fn validate_openai_assistant(object: &Map<String, Value>) -> Result<(), &'static str> {
    if !object.contains_key("content") {
        return Err("REQUEST_INVALID");
    }
    let content = object.get("content").ok_or("REQUEST_INVALID")?;
    let tool_calls = object.get("tool_calls");
    match content {
        Value::Null => {
            if tool_calls.is_none() {
                return Err("REQUEST_INVALID");
            }
        }
        Value::String(_) => {}
        other => validate_text_blocks(other, MAX_MESSAGES)?,
    }
    if let Some(reasoning) = object.get("reasoning_content") {
        if !reasoning.is_string() {
            return Err("REQUEST_INVALID");
        }
    }
    if let Some(details) = object.get("reasoning_details") {
        validate_reasoning_details(details)?;
    }
    if let Some(calls) = tool_calls {
        validate_tool_calls(calls)?;
    }
    Ok(())
}

/// Replayed provider reasoning metadata (Pi stores Anthropic thinking signatures
/// and OpenAI `reasoning_details` here and replays them on the assistant turn).
/// It is opaque; only bound its size and require each element be an object with a
/// string `type`, so no unbounded blob or opaque non-JSON payload slips through.
fn validate_reasoning_details(value: &Value) -> Result<(), &'static str> {
    let items = value.as_array().ok_or("REQUEST_INVALID")?;
    if items.len() > MAX_REASONING_DETAILS {
        return Err("REQUEST_LIMIT");
    }
    for item in items {
        let object = item.as_object().ok_or("REQUEST_INVALID")?;
        if object.get("type").and_then(Value::as_str).is_none() {
            return Err("REQUEST_INVALID");
        }
        let encoded = serde_json::to_vec(item).map_err(|_| "REQUEST_INVALID")?;
        if encoded.len() > MAX_REASONING_DETAIL_BYTES {
            return Err("REQUEST_LIMIT");
        }
    }
    Ok(())
}

fn validate_openai_tool_result(object: &Map<String, Value>) -> Result<(), &'static str> {
    for required in ["content", "tool_call_id"] {
        if !object.contains_key(required) {
            return Err("REQUEST_INVALID");
        }
    }
    let content = object
        .get("content")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if content.len() > MAX_TOOL_RESULT_BYTES {
        return Err("REQUEST_LIMIT");
    }
    let call_id = object
        .get("tool_call_id")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !is_bounded_identifier(call_id, MAX_TOOL_CALL_ID_BYTES) {
        return Err("REQUEST_INVALID");
    }
    // The result `name` is informational; a restored/retired tool must not break
    // replay, so only require a bounded identifier here.
    if let Some(name) = object.get("name") {
        let Some(name) = name.as_str() else {
            return Err("REQUEST_INVALID");
        };
        if !is_bounded_identifier(name, MAX_TOOL_CALL_ID_BYTES) {
            return Err("REQUEST_INVALID");
        }
    }
    Ok(())
}

fn validate_tool_calls(value: &Value) -> Result<(), &'static str> {
    let calls = value.as_array().ok_or("REQUEST_INVALID")?;
    if calls.is_empty() || calls.len() > MAX_TOOL_CALLS {
        return Err("REQUEST_LIMIT");
    }
    let registered = super::capabilities::supported_tool_names();
    for call in calls {
        let object = call.as_object().ok_or("REQUEST_INVALID")?;
        require_fields(
            object,
            &["id", "type", "function"],
            &["id", "type", "function"],
        )?;
        if object.get("type").and_then(Value::as_str) != Some("function") {
            return Err("REQUEST_INVALID");
        }
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or("REQUEST_INVALID")?;
        if !is_bounded_identifier(id, MAX_TOOL_CALL_ID_BYTES) {
            return Err("REQUEST_INVALID");
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or("REQUEST_INVALID")?;
        require_fields(function, &["name", "arguments"], &["name", "arguments"])?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or("REQUEST_INVALID")?;
        if !registered.contains(&name) {
            return Err("REQUEST_INVALID");
        }
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or("REQUEST_INVALID")?;
        if arguments.is_empty() || arguments.len() > MAX_TOOL_ARGUMENTS_BYTES {
            return Err("REQUEST_LIMIT");
        }
        let parsed: Value = serde_json::from_str(arguments).map_err(|_| "REQUEST_INVALID")?;
        if !parsed.is_object() {
            return Err("REQUEST_INVALID");
        }
    }
    Ok(())
}

/// Anthropic messages: plain text content or blocks. A tool round trip is an
/// assistant `tool_use` block followed by a user `tool_result` block; both are
/// bounded and `tool_use` names must be registered tools.
fn validate_anthropic_message(object: &Map<String, Value>) -> Result<(), &'static str> {
    if !object.contains_key("role") || !object.contains_key("content") {
        return Err("REQUEST_INVALID");
    }
    reject_blacklisted_keys(object)?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !matches!(role, "user" | "assistant") {
        return Err("REQUEST_INVALID");
    }
    let content = object.get("content").ok_or("REQUEST_INVALID")?;
    if content.is_string() {
        return Ok(());
    }
    let blocks = content.as_array().ok_or("REQUEST_INVALID")?;
    if blocks.len() > MAX_MESSAGES {
        return Err("REQUEST_LIMIT");
    }
    for block in blocks {
        let block = block.as_object().ok_or("REQUEST_INVALID")?;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                require_fields(block, &["type", "text"], &["type", "text"])?;
                if !block.get("text").is_some_and(Value::is_string) {
                    return Err("REQUEST_INVALID");
                }
            }
            Some("tool_use") if role == "assistant" => validate_anthropic_tool_use(block)?,
            Some("tool_result") if role == "user" => validate_anthropic_tool_result(block)?,
            Some("thinking") if role == "assistant" => validate_anthropic_thinking(block, false)?,
            Some("redacted_thinking") if role == "assistant" => {
                validate_anthropic_thinking(block, true)?
            }
            _ => return Err("REQUEST_INVALID"),
        }
    }
    Ok(())
}

/// Replayed Anthropic extended-thinking blocks. They are opaque reasoning
/// metadata (text + signature, or redacted `data`); bound their size, keep them
/// assistant-only, and never treat them as user-visible content.
fn validate_anthropic_thinking(
    object: &Map<String, Value>,
    redacted: bool,
) -> Result<(), &'static str> {
    if redacted {
        require_fields(object, &["type", "data"], &["type", "data"])?;
        let data = object
            .get("data")
            .and_then(Value::as_str)
            .ok_or("REQUEST_INVALID")?;
        if data.len() > MAX_REASONING_DETAIL_BYTES {
            return Err("REQUEST_LIMIT");
        }
    } else {
        require_fields(
            object,
            &["type", "thinking", "signature"],
            &["type", "thinking"],
        )?;
        if !object.get("thinking").is_some_and(Value::is_string) {
            return Err("REQUEST_INVALID");
        }
        if let Some(signature) = object.get("signature") {
            let signature = signature.as_str().ok_or("REQUEST_INVALID")?;
            if signature.len() > MAX_REASONING_DETAIL_BYTES {
                return Err("REQUEST_LIMIT");
            }
        }
    }
    Ok(())
}

fn validate_anthropic_tool_use(object: &Map<String, Value>) -> Result<(), &'static str> {
    require_fields(
        object,
        &["type", "id", "name", "input"],
        &["type", "id", "name", "input"],
    )?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !is_bounded_identifier(id, MAX_TOOL_CALL_ID_BYTES) {
        return Err("REQUEST_INVALID");
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !super::capabilities::supported_tool_names().contains(&name) {
        return Err("REQUEST_INVALID");
    }
    let input = object.get("input").ok_or("REQUEST_INVALID")?;
    if !input.is_object() {
        return Err("REQUEST_INVALID");
    }
    let encoded = serde_json::to_vec(input).map_err(|_| "REQUEST_INVALID")?;
    if encoded.len() > MAX_TOOL_ARGUMENTS_BYTES {
        return Err("REQUEST_LIMIT");
    }
    Ok(())
}

fn validate_anthropic_tool_result(object: &Map<String, Value>) -> Result<(), &'static str> {
    require_fields(
        object,
        &["type", "tool_use_id", "content", "is_error"],
        &["type", "tool_use_id", "content"],
    )?;
    let id = object
        .get("tool_use_id")
        .and_then(Value::as_str)
        .ok_or("REQUEST_INVALID")?;
    if !is_bounded_identifier(id, MAX_TOOL_CALL_ID_BYTES) {
        return Err("REQUEST_INVALID");
    }
    if let Some(is_error) = object.get("is_error") {
        if !is_error.is_boolean() {
            return Err("REQUEST_INVALID");
        }
    }
    let content = object.get("content").ok_or("REQUEST_INVALID")?;
    if !content.is_string() {
        validate_text_blocks(content, MAX_MESSAGES)?;
    }
    let encoded = serde_json::to_vec(content).map_err(|_| "REQUEST_INVALID")?;
    if encoded.len() > MAX_TOOL_RESULT_BYTES {
        return Err("REQUEST_LIMIT");
    }
    Ok(())
}

fn is_bounded_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
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
    /// Shared with the active attempt. Transports may poll it while opening
    /// headers or waiting for the next response chunk.
    pub cancelled: Arc<AtomicBool>,
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

/// The A3-02 transport seam. Production installs `SwitchableGatewayTransport`
/// (real HTTP, proxy-only user configuration) without any user-visible on/off
/// switch; `DisabledGatewayTransport` stays as the explicit fail-closed seam so a
/// test or a future degraded wiring can prove "no egress" instead of assuming it.
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
    cancelled: Arc<AtomicBool>,
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
    /// 该传输是否会真的打开网络连接。由构造方显式声明，默认 false（测试替身、
    /// 关断传输都属于这一类），只有生产真实 HTTP 传输才允许置 true。
    real_network: bool,
    /// 迁仓/恢复期间的总仓维护门（A4-05）。Host 的写能力必须与 GUI、MCP 一样被它挡住：
    /// 只挡外部 MCP 客户端等于留下「Pi 可以在总仓搬家中途写入」的洞。
    maintenance: Option<Arc<crate::services::library_migration::LibraryMaintenanceGate>>,
    state: Mutex<GatewayBrokerState>,
}

impl GatewayBroker {
    pub fn new(
        instance_id: String,
        db: Arc<Database>,
        credential_backend: Arc<dyn CredentialBackend>,
        transport: Arc<dyn GatewayTransport>,
    ) -> Self {
        Self::with_real_network(instance_id, db, credential_backend, transport, false)
    }

    /// 显式声明「该传输会真的走网络」，用于准确上报模型外发状态。
    pub fn with_real_network(
        instance_id: String,
        db: Arc<Database>,
        credential_backend: Arc<dyn CredentialBackend>,
        transport: Arc<dyn GatewayTransport>,
        real_network: bool,
    ) -> Self {
        Self {
            instance_id,
            db,
            credential_backend,
            transport,
            real_network,
            maintenance: None,
            state: Mutex::new(GatewayBrokerState::default()),
        }
    }

    /// 接线总仓维护门。构造后链式调用，避免为了一个可选依赖改动所有既有构造点。
    pub fn with_maintenance(
        mut self,
        maintenance: Option<Arc<crate::services::library_migration::LibraryMaintenanceGate>>,
    ) -> Self {
        self.maintenance = maintenance;
        self
    }

    /// Host 能力执行时使用的维护门；未接线时为空，视为未维护。
    pub fn maintenance(
        &self,
    ) -> Option<&Arc<crate::services::library_migration::LibraryMaintenanceGate>> {
        self.maintenance.as_ref()
    }

    pub fn database(&self) -> &Arc<Database> {
        &self.db
    }

    /// 当前注入的传输是否会真的走网络（A3-02）。
    ///
    /// 注意这里**不能**用 `GatewayTransport::enabled()` 代替：那个语义是「这条传输
    /// 能否承载一次尝试」，本地测试替身同样返回 true，却全程不开 socket。若拿它上报
    /// “模型外发已开启”，会把进程内 Mock 误报成联网。
    ///
    /// 目前只有生产传输选择（`select_production_transport`）会构造真实 HTTP 传输，
    /// 因此这里按「是否已启用真实传输」判断，并叠加传输自身的可用性。
    pub fn uses_real_network_transport(&self) -> bool {
        self.real_network && self.transport.enabled()
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

    /// 发送前的廉价检查：网络出口就绪 → 服务绑定 → 云服务密钥存在性。
    ///
    /// 没有「允许联网」这道用户开关：配置好服务就能直接对话。唯一的前置是
    /// 传输本身可承载请求——测试替身恒为就绪，`DisabledGatewayTransport`
    /// 表示当前平台/安装包没有网络出口，仍以失败关闭，绝不明文外发。
    ///
    /// 只读设置与连接元数据，不签发租约、不排队、不发请求，因此失败时界面
    /// 能拿到可操作的错误码，而不是等一次“回答失败”。任何一项不满足都不外发。
    pub fn preflight(&self) -> AppResult<()> {
        if !self.transport.enabled() {
            return Err(AppError::agent_model_network_disabled(
                "Agent 网络出口没有就绪：当前平台或安装包不含 Agent 运行资源，请确认支持范围后重试。",
            ));
        }
        let connection = credentials::resolve_agent_model_connection_metadata(&self.db, None)?;
        if connection.deployment == "cloud" {
            // 走注入的 CredentialBackend（与真正发送时同一后端），测试替身因此
            // 不会被这一步挡下；读到的密钥立即丢弃，不落日志。
            let resolved = credentials::resolve_agent_model_connection_with_credential(
                &self.db,
                self.credential_backend.as_ref(),
                None,
            )?;
            if resolved
                .credential()
                .is_none_or(|secret| secret.trim().is_empty())
            {
                return Err(AppError::agent_credential_missing(format!(
                    "Agent 绑定的云服务「{}」还没有保存密钥，请在 AI 服务里补充后重试。",
                    connection.name
                )));
            }
        }
        Ok(())
    }

    pub fn issue_run_lease(
        &self,
        conversation_id: &str,
        run_id: &str,
    ) -> crate::error::AppResult<Value> {
        self.issue_run_lease_with_thinking(conversation_id, run_id, None)
    }

    /// Issue a lease carrying an optional thinking level. The level is only
    /// forwarded when the resolved connection declares reasoning support; an
    /// unknown/blank level is silently dropped (treated as "off").
    pub fn issue_run_lease_with_thinking(
        &self,
        conversation_id: &str,
        run_id: &str,
        thinking_level: Option<&str>,
    ) -> crate::error::AppResult<Value> {
        let connection = credentials::resolve_agent_model_connection_metadata(&self.db, None)?;
        let mut lease =
            AgentGatewayLease::issue(&self.instance_id, conversation_id, run_id, &connection)
                .map_err(|_| {
                    crate::error::AppError::conflict("Agent 服务无法为本次回答签发模型租约")
                })?;
        lease.thinking_level = normalized_thinking_level(thinking_level);
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
            cancelled: Arc::new(AtomicBool::new(false)),
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
        // Reading the credential deliberately happens outside the DB guard. The
        // Agent connection can therefore be edited while the keyring call is in
        // flight. Re-read the metadata after that call and fail closed if the
        // captured revision is no longer current; otherwise an old secret could
        // be paired with a newly saved endpoint/model.
        let current_connection =
            match credentials::resolve_agent_model_connection_metadata(&self.db, expected) {
                Ok(connection) => connection,
                Err(error) => {
                    fail(gateway_error_for_app(&error));
                    return;
                }
            };
        if current_connection != resolved.connection {
            fail("CONNECTION_CHANGED");
            return;
        }
        let endpoint = match attempt.route.endpoint_url(&current_connection.base_url) {
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
                &current_connection,
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
            model: current_connection.model.clone(),
            body,
            authorization: match attempt.route {
                Route::OpenAiChat => credential.map(|secret| format!("Bearer {secret}")),
                Route::AnthropicMessages => credential,
            },
            anthropic_version: (attempt.route == Route::AnthropicMessages).then_some("2023-06-01"),
            cancelled: Arc::clone(&attempt.cancelled),
        };
        let response = match self.transport.open(request) {
            Ok(response) => response,
            Err(code) => {
                fail(allowed_gateway_error(code));
                return;
            }
        };
        if attempt.cancelled.load(Ordering::Acquire) {
            // The response is not installed on the attempt until below, so the
            // normal finish path cannot reach its transport cancellation hook.
            // Cancel the just-opened response explicitly before dropping it.
            (response.cancel)();
            fail("CANCELLED");
            return;
        }
        let result = attempt
            .state
            .lock()
            .map_err(|_| "STREAM_FAILED")
            .and_then(|mut state| state.begin_streaming(response.status, &response.content_type));
        if let Err(code) = result {
            // The response has not been installed into the attempt yet, so
            // finish_attempt cannot reach its cancellation hook. Explicitly
            // interrupt the transport before dropping the response, including
            // redirects and non-SSE upstream replies.
            (response.cancel)();
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
        MAX_RUN_ATTEMPTS, MAX_TOOL_CALLS,
    };
    use crate::services::credentials::CredentialBackend;
    use crate::state::Database;
    use base64::Engine;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    struct RebindingCredentials {
        db: Arc<Database>,
        entries: Mutex<HashMap<String, String>>,
        rebind_on_get: AtomicBool,
    }

    impl CredentialBackend for RebindingCredentials {
        fn get(&self, id: &str) -> crate::error::AppResult<Option<String>> {
            let value = self.entries.lock().unwrap().get(id).cloned();
            if self.rebind_on_get.swap(false, Ordering::SeqCst) {
                let conn = self.db.lock()?;
                crate::db::ai_connections::upsert_with_limits(
                    &conn,
                    "connection_fixture_gateway_0001",
                    "Gateway fixture replacement",
                    "cloud",
                    "openai_chat",
                    "https://api.example.test/v1",
                    "replacement-model",
                    Some("connection_fixture_gateway_0001"),
                    1,
                    10,
                    20,
                )?;
            }
            Ok(value)
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

    struct RejectedResponseTransport {
        status: u16,
        content_type: &'static str,
        cancel_calls: Arc<AtomicUsize>,
    }

    struct CancelAfterOpenTransport {
        cancel_calls: Arc<AtomicUsize>,
        on_open: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    impl GatewayTransport for CancelAfterOpenTransport {
        fn open(
            &self,
            _request: GatewayUpstreamRequest,
        ) -> Result<GatewayTransportResponse, &'static str> {
            if let Some(on_open) = self.on_open.lock().unwrap().as_ref() {
                on_open();
            }
            let cancel_calls = Arc::clone(&self.cancel_calls);
            Ok(GatewayTransportResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                body: Box::new(Cursor::new(Vec::new())),
                cancel: Arc::new(move || {
                    cancel_calls.fetch_add(1, Ordering::SeqCst);
                }),
            })
        }
    }

    impl GatewayTransport for RejectedResponseTransport {
        fn open(
            &self,
            _request: GatewayUpstreamRequest,
        ) -> Result<GatewayTransportResponse, &'static str> {
            let cancel_calls = Arc::clone(&self.cancel_calls);
            Ok(GatewayTransportResponse {
                status: self.status,
                content_type: self.content_type.to_string(),
                body: Box::new(Cursor::new(Vec::new())),
                cancel: Arc::new(move || {
                    cancel_calls.fetch_add(1, Ordering::SeqCst);
                }),
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
        broker_exchange_within(broker, frame, Duration::from_secs(3))
    }

    /// 同 `broker_exchange`，但可指定等待上限：真实外发测试要走网络，
    /// 3 秒的离线夹具预算不足以覆盖真实上游延迟。
    fn broker_exchange_within(
        broker: &Arc<GatewayBroker>,
        frame: serde_json::Value,
        budget: Duration,
    ) -> serde_json::Value {
        let (sender, receiver) = mpsc::sync_channel(1);
        broker
            .handle_host_frame(
                frame,
                Arc::new(move |response| {
                    let _ = sender.send(response);
                }),
            )
            .unwrap();
        receiver.recv_timeout(budget).unwrap()
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
    fn preflight_reports_network_disabled_before_touching_credentials() {
        let (broker, credentials) = fixture_broker(Arc::new(super::DisabledGatewayTransport));
        let error = broker.preflight().unwrap_err();
        assert_eq!(error.code(), "AGENT_MODEL_NETWORK_DISABLED");
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn preflight_reports_unbound_service_when_nothing_is_bound() {
        let conn = crate::db::init_memory().unwrap();
        let db = Arc::new(Database::new(conn));
        let broker = Arc::new(GatewayBroker::new(
            "instance_0123456789abcdef".to_string(),
            db,
            Arc::new(FixtureCredentials::default()),
            Arc::new(FixtureTransport::default()),
        ));
        let error = broker.preflight().unwrap_err();
        assert_eq!(error.code(), "AGENT_SERVICE_UNBOUND");
    }

    #[test]
    fn preflight_reports_missing_credential_for_a_bound_cloud_service() {
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
        // No credential saved: preflight must fail closed before any transport use.
        let broker = Arc::new(GatewayBroker::new(
            "instance_0123456789abcdef".to_string(),
            db,
            Arc::new(FixtureCredentials::default()),
            Arc::new(FixtureTransport::default()),
        ));
        let error = broker.preflight().unwrap_err();
        assert_eq!(error.code(), "AGENT_CREDENTIAL_MISSING");
    }

    #[test]
    fn connection_rebind_during_credential_read_fails_before_transport_open() {
        let conn = crate::db::init_memory().unwrap();
        crate::db::ai_connections::upsert_with_limits(
            &conn,
            "connection_fixture_gateway_0001",
            "Gateway fixture",
            "cloud",
            "openai_chat",
            "https://api.example.test/v1",
            "fixture-model",
            Some("connection_fixture_gateway_0001"),
            1,
            10,
            20,
        )
        .unwrap();
        crate::db::ai_connections::bind_usage(&conn, "agent", "connection_fixture_gateway_0001")
            .unwrap();
        let db = Arc::new(Database::new(conn));
        let credentials = Arc::new(RebindingCredentials {
            db: Arc::clone(&db),
            entries: Mutex::new(HashMap::from([(
                "connection_fixture_gateway_0001".to_string(),
                "fixture-secret-never-on-stdio".to_string(),
            )])),
            rebind_on_get: AtomicBool::new(true),
        });
        let transport = Arc::new(FixtureTransport::default());
        let broker = Arc::new(GatewayBroker::new(
            "instance_0123456789abcdef".to_string(),
            db,
            credentials,
            transport.clone(),
        ));
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let response = commit_fixture_request(&broker, run_id, lease["leaseId"].as_str().unwrap());

        assert_eq!(response["type"], "gateway.response.error");
        assert_eq!(response["code"], "CONNECTION_CHANGED");
        assert!(transport.observed.lock().unwrap().is_none());
        assert_eq!(broker.lease_count(), 1);
    }

    #[test]
    fn rejected_upstream_response_invokes_transport_cancel_hook() {
        let cancel_calls = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(RejectedResponseTransport {
            status: 302,
            content_type: "text/event-stream",
            cancel_calls: Arc::clone(&cancel_calls),
        });
        let (broker, _) = fixture_broker(transport);
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let response = commit_fixture_request(&broker, run_id, lease["leaseId"].as_str().unwrap());

        assert_eq!(response["type"], "gateway.response.error");
        assert_eq!(response["code"], "REDIRECT_DENIED");
        assert_eq!(cancel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(broker.lease_count(), 1);
    }

    #[test]
    fn cancellation_after_transport_open_invokes_transport_cancel_hook() {
        let cancel_calls = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(CancelAfterOpenTransport {
            cancel_calls: Arc::clone(&cancel_calls),
            on_open: Mutex::new(None),
        });
        let (broker, _) = fixture_broker(transport.clone());
        *transport.on_open.lock().unwrap() = Some(Box::new({
            let broker = Arc::clone(&broker);
            move || {
                let attempt = broker
                    .state
                    .lock()
                    .unwrap()
                    .attempts
                    .values()
                    .next()
                    .cloned()
                    .unwrap();
                attempt.cancelled.store(true, Ordering::Release);
            }
        }));
        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let response = commit_fixture_request(&broker, run_id, lease["leaseId"].as_str().unwrap());

        assert_eq!(response["type"], "gateway.response.error");
        assert_eq!(response["code"], "CANCELLED");
        assert_eq!(cancel_calls.load(Ordering::SeqCst), 1);
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
            context_window: 32768,
            max_output_tokens: 4096,
            reasoning_supported: false,
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

        assert_eq!(object.len(), 7);
        assert_eq!(payload["provider"], "openai");
        assert_eq!(payload["protocol"], "openai_chat");
        assert_eq!(payload["model"], "fixture-model");
        assert_eq!(payload["contextWindow"], 32768);
        assert_eq!(payload["maxOutputTokens"], 4096);
        assert_eq!(payload["reasoning"], false);
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
    fn accepts_only_registered_closed_tool_declarations() {
        let openai = |name: &str, additional: &str| {
            format!(
                r#"{{"model":"fixture-model","messages":[{{"role":"user","content":"x"}}],"stream":true,"max_tokens":2048,"tools":[{{"type":"function","function":{{"name":"{name}","description":"search","parameters":{{"type":"object","properties":{{"text":{{"type":"string"}}}},"required":["text"],"additionalProperties":{additional}}}}}}}]}}"#
            )
            .into_bytes()
        };
        let anthropic = |name: &str, additional: &str| {
            format!(
                r#"{{"model":"fixture-model","messages":[{{"role":"user","content":"x"}}],"stream":true,"max_tokens":2048,"tools":[{{"name":"{name}","description":"search","eager_input_streaming":true,"input_schema":{{"type":"object","properties":{{"text":{{"type":"string"}}}},"additionalProperties":{additional}}}}}]}}"#
            )
            .into_bytes()
        };
        assert_eq!(
            validate_request(
                Route::OpenAiChat,
                "fixture-model",
                &openai("tea_assets_search", "false")
            ),
            Ok(())
        );
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                &anthropic("tea_assets_search", "false")
            ),
            Ok(())
        );
        for body in [
            openai("tea_plans_approve", "false"),
            openai("tea_operations_resume", "false"),
            openai("assets.search", "false"),
        ] {
            assert_eq!(
                validate_request(Route::OpenAiChat, "fixture-model", &body),
                Err("REQUEST_INVALID")
            );
        }
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                &anthropic("tea_plans_approve", "false")
            ),
            Err("REQUEST_INVALID")
        );
        // Argument schema is forwarded opaquely now, so a registered tool with a
        // non-closed `additionalProperties` is still accepted.
        assert_eq!(
            validate_request(
                Route::OpenAiChat,
                "fixture-model",
                &openai("tea_assets_search", "true")
            ),
            Ok(())
        );
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                &anthropic("tea_assets_search", "true")
            ),
            Ok(())
        );
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                br#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":2048,"tools":[{"name":"tea_assets_search","description":"search","input_schema":{"type":"object","properties":{"text":{"type":"string"}},"additionalProperties":false}},{"name":"tea_assets_search","description":"search","input_schema":{"type":"object","properties":{"text":{"type":"string"}},"additionalProperties":false}}]}"#
            ),
            Err("REQUEST_INVALID")
        );
    }

    #[test]
    fn accepts_registered_tool_follow_up_and_rejects_forged_shapes() {
        let openai_tools = r#""tools":[{"type":"function","function":{"name":"tea_assets_search","description":"search","parameters":{"type":"object","properties":{"text":{"type":"string"}},"additionalProperties":false}}}]"#;
        let anthropic_tools = r#""tools":[{"name":"tea_assets_search","description":"search","input_schema":{"type":"object","properties":{"text":{"type":"string"}},"additionalProperties":false}}]"#;
        let openai = |assistant: &str, tool: &str| {
            format!(
                r#"{{"model":"fixture-model","messages":[{{"role":"user","content":"搜索茶馆"}},{assistant},{tool}],"stream":true,"max_tokens":2048,{openai_tools}}}"#
            )
            .into_bytes()
        };
        let openai_assistant = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"call_abc","type":"function","function":{"name":"tea_assets_search","arguments":"{\"text\":\"茶馆\"}"}}]}"#;
        let openai_tool = r#"{"role":"tool","content":"{\"total\":0}","tool_call_id":"call_abc","name":"tea_assets_search"}"#;
        assert_eq!(
            validate_request(
                Route::OpenAiChat,
                "fixture-model",
                &openai(openai_assistant, openai_tool)
            ),
            Ok(())
        );

        let unknown_assistant = openai_assistant.replace("tea_assets_search", "tea_plans_approve");
        let missing_call_id = r#"{"role":"tool","content":"{}"}"#;
        let null_without_calls = r#"{"role":"assistant","content":null}"#;
        let custom_call = openai_assistant.replace("\"type\":\"function\"", "\"type\":\"custom\"");
        let malformed_arguments = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"call_abc","type":"function","function":{"name":"tea_assets_search","arguments":"not-json"}}]}"#;
        let unknown_tool_name = r#"{"role":"tool","content":"{}","tool_call_id":"call_abc","name":"tea_plans_approve"}"#;
        for (assistant, tool) in [
            (unknown_assistant.as_str(), openai_tool),
            (openai_assistant, missing_call_id),
            (null_without_calls, openai_tool),
            (custom_call.as_str(), openai_tool),
            (malformed_arguments, openai_tool),
            (unknown_assistant.as_str(), unknown_tool_name),
        ] {
            assert_eq!(
                validate_request(Route::OpenAiChat, "fixture-model", &openai(assistant, tool)),
                Err("REQUEST_INVALID"),
                "assistant={assistant} tool={tool}"
            );
        }

        let many_calls = (0..=MAX_TOOL_CALLS)
            .map(|index| {
                format!(
                    r#"{{"id":"call_{index}","type":"function","function":{{"name":"tea_assets_search","arguments":"{{}}"}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let many_assistant =
            format!(r#"{{"role":"assistant","content":null,"tool_calls":[{many_calls}]}}"#);
        assert_eq!(
            validate_request(
                Route::OpenAiChat,
                "fixture-model",
                &openai(&many_assistant, openai_tool)
            ),
            Err("REQUEST_LIMIT")
        );

        let anthropic = |assistant: &str, tool: &str| {
            format!(
                r#"{{"model":"fixture-model","messages":[{{"role":"user","content":"搜索茶馆"}},{assistant},{tool}],"stream":true,"max_tokens":2048,{anthropic_tools}}}"#
            )
            .into_bytes()
        };
        let assistant_tool_use = r#"{"role":"assistant","content":[{"type":"text","text":"我来搜索"},{"type":"tool_use","id":"call_abc","name":"tea_assets_search","input":{"text":"茶馆"}}]}"#;
        let user_tool_result = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_abc","content":[{"type":"text","text":"{\"total\":0}"}],"is_error":false}]}"#;
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                &anthropic(assistant_tool_use, user_tool_result)
            ),
            Ok(())
        );

        let forged_name = assistant_tool_use.replace("tea_assets_search", "plans.approve");
        let string_input =
            assistant_tool_use.replace(r#""input":{"text":"茶馆"}"#, r#""input":"茶馆""#);
        let result_in_assistant = r#"{"role":"assistant","content":[{"type":"tool_result","tool_use_id":"call_abc","content":"{}"}]}"#;
        let bad_error_flag = user_tool_result.replace("\"is_error\":false", "\"is_error\":\"no\"");
        for (assistant, tool) in [
            (forged_name.as_str(), user_tool_result),
            (string_input.as_str(), user_tool_result),
            (result_in_assistant, user_tool_result),
            (assistant_tool_use, bad_error_flag.as_str()),
        ] {
            assert_eq!(
                validate_request(
                    Route::AnthropicMessages,
                    "fixture-model",
                    &anthropic(assistant, tool)
                ),
                Err("REQUEST_INVALID"),
                "assistant={assistant} tool={tool}"
            );
        }
    }

    #[test]
    fn rejects_non_text_wrong_model_and_unbounded_tokens() {
        let cases = [
            r#"{"model":"fixture-model","messages":[{"role":"user","content":[{"type":"image","source":{"data":"x"}}]}],"stream":true,"max_tokens":10}"#,
            r#"{"model":"other","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":8193}"#,
        ];
        for body in cases {
            assert_eq!(
                validate_request(Route::AnthropicMessages, "fixture-model", body.as_bytes()),
                Err("REQUEST_INVALID")
            );
        }
        // Ceiling boundary: 8192 (adaptive budget ceiling) is accepted.
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                br#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":8192}"#
            ),
            Ok(())
        );
        // An empty tools array is allowed: pi emits `tools: []` when a turn keeps
        // tool history but has no request tools.
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                br#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"tools":[]}"#
            ),
            Ok(())
        );

        let openai_image = br#"{"model":"fixture-model","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]}],"stream":true,"max_tokens":10}"#;
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", openai_image),
            Err("REQUEST_INVALID")
        );
    }

    #[test]
    fn forwards_reasoning_metadata_and_generation_params_but_rejects_dangerous_keys() {
        // Multi-turn Pi history: the assistant turn replays `reasoning_details`
        // (the original bug), unknown generation params (`top_p`) pass through,
        // and `tools: []` is accepted.
        let ok_body = r#"{"model":"fixture-model","messages":[{"role":"user","content":"你好"},{"role":"assistant","content":"你好！","reasoning_details":[{"type":"reasoning.text","text":"internal"}],"reasoning_content":"internal"}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":2048,"top_p":0.9,"tools":[]}"#;
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", ok_body.as_bytes()),
            Ok(())
        );

        // Top-level credential / endpoint / header keys are rejected.
        for bad in [
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"api_key":"sk"}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"base_url":"http://evil"}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"headers":{"x":"y"}"}"#,
        ] {
            assert_eq!(
                validate_request(Route::OpenAiChat, "fixture-model", bad.as_bytes()),
                Err("REQUEST_INVALID"),
                "body={bad}"
            );
        }
        // Message-level credential key rejected (normalized casing).
        let bad_msg = r#"{"model":"fixture-model","messages":[{"role":"user","content":"x","apiKey":"sk"}],"stream":true,"max_tokens":10}"#;
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", bad_msg.as_bytes()),
            Err("REQUEST_INVALID")
        );
        // Cost amplifiers and legacy function-calling shapes rejected.
        for bad in [
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"n":2}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"best_of":2}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"functions":[]}"#,
            r#"{"model":"fixture-model","messages":[{"role":"user","content":"x"}],"stream":true,"max_tokens":10,"function_call":"auto"}"#,
        ] {
            assert_eq!(
                validate_request(Route::OpenAiChat, "fixture-model", bad.as_bytes()),
                Err("REQUEST_INVALID"),
                "body={bad}"
            );
        }
        // Message content stays text-only even though message keys are open.
        let image_msg = r#"{"model":"fixture-model","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:x"}}]}],"stream":true,"max_tokens":10}"#;
        assert_eq!(
            validate_request(Route::OpenAiChat, "fixture-model", image_msg.as_bytes()),
            Err("REQUEST_INVALID")
        );

        // Anthropic thinking / redacted_thinking blocks are accepted on assistant
        // turns (they are replayed reasoning metadata).
        let anthropic_ok = r#"{"model":"fixture-model","messages":[{"role":"user","content":"你好"},{"role":"assistant","content":[{"type":"thinking","thinking":"hmm","signature":"sig"},{"type":"redacted_thinking","data":"abc"},{"type":"text","text":"你好"}]}],"stream":true,"max_tokens":2048}"#;
        assert_eq!(
            validate_request(
                Route::AnthropicMessages,
                "fixture-model",
                anthropic_ok.as_bytes()
            ),
            Ok(())
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
    fn endpoint_appends_route_path_to_the_user_base_url_verbatim() {
        // 用户填写的就是连接测试验证过的地址：接口路径直接追加，不自行补 /v1。
        let cases = [
            // (base, openai 结果, anthropic 结果)
            (
                "https://api.example.test/v1",
                "https://api.example.test/v1/chat/completions",
                "https://api.example.test/v1/messages",
            ),
            (
                "https://api.example.test/v1/",
                "https://api.example.test/v1/chat/completions",
                "https://api.example.test/v1/messages",
            ),
            (
                "https://api.example.test",
                "https://api.example.test/chat/completions",
                "https://api.example.test/messages",
            ),
            (
                "https://open.bigmodel.cn/api/paas/v4",
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "https://open.bigmodel.cn/api/paas/v4/messages",
            ),
            (
                "https://ark.cn-beijing.volces.com/api/v3",
                "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
                "https://ark.cn-beijing.volces.com/api/v3/messages",
            ),
            (
                "http://127.0.0.1:11434/v1",
                "http://127.0.0.1:11434/v1/chat/completions",
                "http://127.0.0.1:11434/v1/messages",
            ),
        ];
        for (base, openai, anthropic) in cases {
            assert_eq!(
                Route::OpenAiChat.endpoint_url(base),
                Ok(openai.to_string()),
                "openai base={base}"
            );
            assert_eq!(
                Route::AnthropicMessages.endpoint_url(base),
                Ok(anthropic.to_string()),
                "anthropic base={base}"
            );
        }
        for rejected in [
            "ftp://api.example.test/v1",
            "https://user:pw@api.example.test/v1",
            "https://api.example.test/v1?x=1",
            "https://api.example.test/v1#frag",
            "not-a-url",
        ] {
            assert_eq!(
                Route::OpenAiChat.endpoint_url(rejected),
                Err("ROUTE_DENIED"),
                "base={rejected}"
            );
        }
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

    // ---- A3-02 真实外发端到端验收（显式 opt-in，默认不跑） --------------------
    //
    // 该测试默认被 `#[ignore]`，不会进入任何默认门禁；仅当负责人显式提供真实
    // provider 网关参数并手动运行 `cargo test -- --ignored real_provider` 时才执行。
    // 参数全部来自环境变量，仓库内不含任何密钥：
    //   TEACELL_A302_E2E_BASE_URL  例如 http://127.0.0.1:3425
    //   TEACELL_A302_E2E_KEY       真实 API Key（仅存内存，绝不写入文件/stdio）
    //   TEACELL_A302_E2E_MODEL     例如 opencode-zen-free/mimo-v2.6-flash-free
    // 断言：
    //   1) 真实 HttpGatewayTransport 拿到 200 且下发非空 SSE 流（含 data: 帧）；
    //   2) 密钥与端点从不出现在任何回给 Host 的帧里（隐私边界）；
    //   3) 关断传输对同一连接必须以 UPSTREAM_UNAVAILABLE 拒绝（默认安全）。
    #[test]
    #[ignore = "A3-02 real egress: requires TEACELL_A302_E2E_BASE_URL/KEY/MODEL and network access"]
    fn real_provider_egress_streams_and_keeps_endpoint_and_key_off_stdio() {
        use crate::services::agent_runtime::gateway::DisabledGatewayTransport;
        use crate::services::agent_runtime::gateway_http::HttpGatewayTransport;

        let (Ok(base_url), Ok(key), Ok(model)) = (
            std::env::var("TEACELL_A302_E2E_BASE_URL"),
            std::env::var("TEACELL_A302_E2E_KEY"),
            std::env::var("TEACELL_A302_E2E_MODEL"),
        ) else {
            panic!(
                "缺少 A3-02 真实外发参数：请设置 TEACELL_A302_E2E_BASE_URL / TEACELL_A302_E2E_KEY / TEACELL_A302_E2E_MODEL"
            );
        };

        let connection_id = "connection_fixture_gateway_0001";
        let conn = crate::db::init_memory().unwrap();
        crate::db::ai_connections::upsert_with_limits(
            &conn,
            connection_id,
            "A3-02 real provider",
            "cloud",
            "openai_chat",
            &base_url,
            &model,
            Some(connection_id),
            1,
            60,
            600,
        )
        .unwrap();
        crate::db::ai_connections::bind_usage(&conn, "agent", connection_id).unwrap();
        let db = Arc::new(Database::new(conn));
        let credentials = Arc::new(FixtureCredentials::default());
        credentials.save(connection_id, &key).unwrap();

        let transport: Arc<dyn GatewayTransport> =
            Arc::new(HttpGatewayTransport::new(Duration::from_secs(120), None).unwrap());
        assert!(transport.enabled(), "真实传输应处于启用状态");
        let broker = Arc::new(GatewayBroker::with_real_network(
            "instance_0123456789abcdef".to_string(),
            db,
            credentials.clone(),
            transport,
            true,
        ));
        assert!(
            broker.uses_real_network_transport(),
            "真实 HTTP 传输必须上报为会走网络"
        );

        let run_id = "run_0123456789abcdef";
        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();

        let mut open = broker_frame(1, "gateway.request.open", run_id, lease_id);
        open["route"] = json!("openai_chat");
        open["method"] = json!("POST");
        let mut replies = vec![broker_exchange(&broker, open)];
        assert_eq!(replies[0]["type"], "gateway.request.credit");

        let body = format!(
            "{{\"model\":\"{}\",\"messages\":[{{\"role\":\"user\",\"content\":\"用一句话回答：你好\"}}],\"stream\":true,\"stream_options\":{{\"include_usage\":true}},\"max_tokens\":64}}",
            model
        );
        let body = body.into_bytes();
        let mut chunk = broker_frame(2, "gateway.request.chunk", run_id, lease_id);
        chunk["chunkSequence"] = json!(0);
        chunk["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&body));
        replies.push(broker_exchange(&broker, chunk));
        assert_eq!(replies[1]["type"], "gateway.request.credit");

        let mut commit = broker_frame(3, "gateway.request.commit", run_id, lease_id);
        commit["chunkCount"] = json!(1);
        commit["byteLength"] = json!(body.len());
        // 真实外发走网络：3 秒离线预算不够，commit 要等真实上游首包。
        replies.push(broker_exchange_within(&broker, commit, REAL_EGRESS_BUDGET));
        assert_eq!(
            replies[2]["type"], "gateway.response.head",
            "真实 provider 未返回 head：{:#}",
            replies[2]
        );
        let status = replies[2]["status"].as_u64().unwrap_or(0);
        assert_eq!(status, 200, "真实 provider 返回非 200：{:#}", replies[2]);

        // 拉取 SSE 流，直到 response.end 或耗尽；累计 payload 用于判定与隐私检查。
        let mut streamed = Vec::<u8>::new();
        let mut seq: u64 = 0;
        let mut saw_sse = false;
        loop {
            let mut pull = broker_frame(4 + seq, "gateway.response.pull", run_id, lease_id);
            pull["pullSequence"] = json!(seq);
            // 真实上游按 token 回流，相邻帧的间隔可能远超 3 秒离线预算，
            // 因此拉流同样必须使用真实外发预算而不是夹具预算。
            let reply = broker_exchange_within(&broker, pull, REAL_EGRESS_BUDGET);
            match reply["type"].as_str().unwrap_or("") {
                "gateway.response.chunk" => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(reply["base64"].as_str().unwrap())
                        .unwrap();
                    if bytes.windows(5).any(|w| w == b"data:") {
                        saw_sse = true;
                    }
                    streamed.extend_from_slice(&bytes);
                    replies.push(reply);
                }
                "gateway.response.end" => {
                    replies.push(reply);
                    break;
                }
                "gateway.response.error" => {
                    panic!("真实 provider 流式期间报错：{:#}", reply);
                }
                other => panic!("意外的拉取回应类型：{other}：{reply:#}"),
            }
            seq += 1;
            assert!(seq < 10_000, "流式拉取次数异常");
        }
        assert!(saw_sse, "真实 provider 未下发 SSE 数据帧");
        assert!(!streamed.is_empty(), "真实 provider 回流为空");

        // 隐私边界：任何回给 Host 的帧都不得泄露 endpoint 或 key。
        let wire = replies
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!wire.contains(&key), "密钥泄漏到 Host 帧！");
        assert!(!wire.contains(&base_url), "端点泄漏到 Host 帧！");
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 1);

        broker.revoke_run(conversation_id, run_id);

        // 默认安全：关断传输对同一连接必须拒绝外发。
        let (disabled, _) = fixture_broker(Arc::new(DisabledGatewayTransport));
        let lease = disabled.issue_run_lease(conversation_id, run_id).unwrap();
        let rejected =
            commit_fixture_request(&disabled, run_id, lease["leaseId"].as_str().unwrap());
        assert_eq!(rejected["type"], "gateway.response.error");
        assert_eq!(rejected["code"], "UPSTREAM_UNAVAILABLE");
        assert!(
            !disabled.uses_real_network_transport(),
            "关断传输不得上报为会走网络"
        );
    }

    // ---- A3-02 真实外发：错误 / 取消 / 限流 三个场景 -------------------------
    //
    // 与上一个测试同一套 opt-in 约定（`--ignored real_provider`）。它们补齐
    // A3-02 卡面要求的「错误、取消、限流」实测，而不是只靠离线 fixture。
    // 参数仍是 TEACELL_A302_E2E_BASE_URL / _KEY / _MODEL。

    /// 读取真实外发参数；缺失时给出明确提示（仅 opt-in 测试调用）。
    fn real_egress_params() -> (String, String, String) {
        let (Ok(base_url), Ok(key), Ok(model)) = (
            std::env::var("TEACELL_A302_E2E_BASE_URL"),
            std::env::var("TEACELL_A302_E2E_KEY"),
            std::env::var("TEACELL_A302_E2E_MODEL"),
        ) else {
            panic!(
                "缺少 A3-02 真实外发参数：请设置 TEACELL_A302_E2E_BASE_URL / TEACELL_A302_E2E_KEY / TEACELL_A302_E2E_MODEL"
            );
        };
        (base_url, key, model)
    }

    /// 用真实端点搭一个 broker；`requests_per_minute` 可用来压我们自己的限流门。
    fn real_egress_broker(
        base_url: &str,
        key: &str,
        model: &str,
        requests_per_minute: i64,
    ) -> (Arc<GatewayBroker>, Arc<FixtureCredentials>) {
        use crate::services::agent_runtime::gateway_http::HttpGatewayTransport;

        let connection_id = "connection_fixture_gateway_0001";
        let conn = crate::db::init_memory().unwrap();
        crate::db::ai_connections::upsert_with_limits(
            &conn,
            connection_id,
            "A3-02 real provider",
            "cloud",
            "openai_chat",
            base_url,
            model,
            Some(connection_id),
            1,
            requests_per_minute,
            600,
        )
        .unwrap();
        crate::db::ai_connections::bind_usage(&conn, "agent", connection_id).unwrap();
        let db = Arc::new(Database::new(conn));
        let credentials = Arc::new(FixtureCredentials::default());
        credentials.save(connection_id, key).unwrap();
        let transport: Arc<dyn GatewayTransport> =
            Arc::new(HttpGatewayTransport::new(Duration::from_secs(120), None).unwrap());
        let broker = Arc::new(GatewayBroker::with_real_network(
            "instance_0123456789abcdef".to_string(),
            db,
            credentials.clone(),
            transport,
            true,
        ));
        (broker, credentials)
    }

    /// 真实外发单步等待预算：真实上游可能在数秒后才回头，3 秒离线预算不够。
    const REAL_EGRESS_BUDGET: Duration = Duration::from_secs(180);

    fn real_open_body(model: &str, prompt: &str) -> Vec<u8> {
        format!(
            "{{\"model\":\"{}\",\"messages\":[{{\"role\":\"user\",\"content\":\"{}\"}}],\"stream\":true,\"stream_options\":{{\"include_usage\":true}},\"max_tokens\":32}}",
            model, prompt
        )
        .into_bytes()
    }

    /// 走到 commit 并返回 commit 的回应（head 或 error）。
    fn real_commit(
        broker: &Arc<GatewayBroker>,
        run_id: &str,
        lease_id: &str,
        body: &[u8],
    ) -> serde_json::Value {
        let mut open = broker_frame(1, "gateway.request.open", run_id, lease_id);
        open["route"] = json!("openai_chat");
        open["method"] = json!("POST");
        assert_eq!(
            broker_exchange(broker, open)["type"],
            "gateway.request.credit"
        );

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
        broker_exchange_within(broker, commit, REAL_EGRESS_BUDGET)
    }

    /// 真实 provider 返回错误时，必须是结构化失败而不是静默换供应商或挂住。
    ///
    /// 用一个不存在的模型名制造上游错误：期望 commit 阶段就拿到
    /// `gateway.response.error`（`UPSTREAM_HTTP` 带状态码，或 `UPSTREAM_UNAVAILABLE`），
    /// 且错误帧里不得出现端点或密钥。
    #[test]
    #[ignore = "A3-02 real egress: requires TEACELL_A302_E2E_BASE_URL/KEY/MODEL and network access"]
    fn real_provider_error_is_structured_and_keeps_secrets_off_stdio() {
        let (base_url, key, model) = real_egress_params();
        // 故意篡改模型名，确保上游不会 200。
        let bogus_model = format!("{model}-definitely-not-a-model");
        let (broker, _credentials) = real_egress_broker(&base_url, &key, &bogus_model, 60);

        let conversation_id = "conversation_0123456789abcdef";
        let run_id = "run_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();

        let body = real_open_body(&bogus_model, "hi");
        let reply = real_commit(&broker, run_id, lease_id, &body);
        assert_eq!(
            reply["type"], "gateway.response.error",
            "上游错误必须以结构化 error 返回，实际：{reply:#}"
        );
        let code = reply["code"].as_str().unwrap_or("");
        assert!(
            matches!(code, "UPSTREAM_HTTP" | "UPSTREAM_UNAVAILABLE"),
            "上游错误码应为 UPSTREAM_HTTP 或 UPSTREAM_UNAVAILABLE，实际：{code}"
        );
        // 隐私边界同样适用于错误路径。
        let wire = reply.to_string();
        assert!(!wire.contains(&key), "密钥泄漏到错误帧！");
        assert!(!wire.contains(&base_url), "端点泄漏到错误帧！");
        broker.revoke_run(conversation_id, run_id);
    }

    /// 真实流式传输进行中取消：必须回 `gateway.request.cancelled`，
    /// 且取消之后不再有正常分块。
    #[test]
    #[ignore = "A3-02 real egress: requires TEACELL_A302_E2E_BASE_URL/KEY/MODEL and network access"]
    fn real_provider_stream_can_be_cancelled_mid_flight() {
        let (base_url, key, model) = real_egress_params();
        let (broker, _credentials) = real_egress_broker(&base_url, &key, &model, 60);

        let conversation_id = "conversation_0123456789abcdef";
        let run_id = "run_0123456789abcdef";
        let lease = broker.issue_run_lease(conversation_id, run_id).unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();

        let body = real_open_body(&model, "请写一段较长的文字，便于中途取消");
        let head = real_commit(&broker, run_id, lease_id, &body);
        assert_eq!(
            head["type"], "gateway.response.head",
            "取消场景前置要求能拿到 head，实际：{head:#}"
        );
        assert_eq!(head["status"], 200);

        // 拉一到两块，然后立刻取消。
        let mut pulled = 0;
        loop {
            let mut pull = broker_frame(4 + pulled, "gateway.response.pull", run_id, lease_id);
            pull["pullSequence"] = json!(pulled);
            let reply = broker_exchange_within(&broker, pull, REAL_EGRESS_BUDGET);
            match reply["type"].as_str().unwrap_or("") {
                "gateway.response.chunk" => {
                    pulled += 1;
                    if pulled >= 1 {
                        break;
                    }
                }
                "gateway.response.end" => break,
                other => panic!("取消前置：意外的回应 {other}: {reply:#}"),
            }
        }

        let cancel = broker_frame(90, "gateway.request.cancel", run_id, lease_id);
        let cancelled = broker_exchange_within(&broker, cancel, REAL_EGRESS_BUDGET);
        assert_eq!(
            cancelled["type"], "gateway.request.cancelled",
            "取消必须回 gateway.request.cancelled，实际：{cancelled:#}"
        );

        // 取消后继续拉，不应再出现正常 chunk（只能是 error / end 或直接失败）。
        let mut pull = broker_frame(91, "gateway.response.pull", run_id, lease_id);
        pull["pullSequence"] = json!(pulled);
        let after = broker_exchange_within(&broker, pull, REAL_EGRESS_BUDGET);
        let after_type = after["type"].as_str().unwrap_or("");
        assert!(
            after_type != "gateway.response.chunk",
            "取消之后不应再下发正常数据块，实际：{after:#}"
        );
        broker.revoke_run(conversation_id, run_id);
    }

    /// 我们自己的请求额度门（每分 1 次）：第二个请求必须被本地限流拒绝，
    /// 而不是被静默放行到上游。
    #[test]
    #[ignore = "A3-02 real egress: requires TEACELL_A302_E2E_BASE_URL/KEY/MODEL and network access"]
    fn real_provider_requests_are_gated_by_our_own_rate_limit() {
        let (base_url, key, model) = real_egress_params();
        // 每分钟只允许 1 次，第二个请求必然等待超时或被拒。
        let (broker, _credentials) = real_egress_broker(&base_url, &key, &model, 1);

        let conversation_id = "conversation_0123456789abcdef";
        let lease = broker
            .issue_run_lease(conversation_id, "run_0123456789abcdef")
            .unwrap();
        let lease_id = lease["leaseId"].as_str().unwrap();

        // 第一请求占满额度。
        // 第一请求占满额度。额度门在 open 阶段就会判定，因此这里只断言
        // 「第一个请求被放行到上游」（credit 继续 + commit 给出 head），
        // 而把「第二个请求被拒」留给下面的断言。
        let body = real_open_body(&model, "第一个请求");
        let first = real_commit(&broker, "run_0123456789abcdef", lease_id, &body);
        assert_eq!(
            first["type"], "gateway.response.head",
            "第一请求应被完整放行（额度未满时不得被限流），实际：{first:#}"
        );
        assert_eq!(
            first["status"], 200,
            "第一请求 head 状态码应为 200：{first:#}"
        );

        // 第二请求：额度已耗尽，必须在本地就被拒绝，且错误码明确是额度类问题。
        let body2 = real_open_body(&model, "第二个请求");
        let lease2 = broker
            .issue_run_lease(conversation_id, "run_0123456789abcdea")
            .unwrap();
        let lease2_id = lease2["leaseId"].as_str().unwrap();

        let mut open2 = broker_frame(
            11,
            "gateway.request.open",
            "run_0123456789abcdea",
            lease2_id,
        );
        // 每个 attempt 的 gatewayRequestId 必须严格递增（base36 序号），
        // 否则 broker 以 REQUEST_INVALID 拒绝，永远走不到额度门。
        open2["gatewayRequestId"] = json!(format!(
            "gateway_attempt_2_{}",
            &"instance_0123456789abcdef"[..12]
        ));
        open2["route"] = json!("openai_chat");
        open2["method"] = json!("POST");
        let second_open = broker_exchange_within(&broker, open2, REAL_EGRESS_BUDGET);
        assert_eq!(
            second_open["type"], "gateway.request.credit",
            "第二请求的 open 阶段不设额度门（额度在 commit 前统一获取），应拿到 credit，实际：{second_open:#}"
        );
        let second_id = "gateway_attempt_2_instance_012";
        let mut chunk2 = broker_frame(
            12,
            "gateway.request.chunk",
            "run_0123456789abcdea",
            lease2_id,
        );
        chunk2["gatewayRequestId"] = json!(second_id);
        chunk2["chunkSequence"] = json!(0);
        chunk2["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&body2));
        assert_eq!(
            broker_exchange_within(&broker, chunk2, REAL_EGRESS_BUDGET)["type"],
            "gateway.request.credit"
        );
        let mut commit2 = broker_frame(
            13,
            "gateway.request.commit",
            "run_0123456789abcdea",
            lease2_id,
        );
        commit2["gatewayRequestId"] = json!(second_id);
        commit2["chunkCount"] = json!(1);
        commit2["byteLength"] = json!(body2.len());

        // 额度门是「等待型」：额度/速率耗尽时，第二请求不会静默放行到上游，
        // 而是阻塞在 acquire_until_owned 直到 run deadline 才以 TIMEOUT 结束。
        // run deadline 远长于本地预算，因此这里用较短预算去证明「这段窗口内
        // 它确实没有被放行」——被放行（拿到 head）才是不变量被破坏。
        //
        // 注意超时是通过 broker_exchange_within 内部的 recv_timeout(...).unwrap()
        // panic 暴露的，所以下面 catch_unwind 接住的 Err 正是「窗口内无回应」。
        let short_budget = Duration::from_secs(15);
        let blocked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            broker_exchange_within(&broker, commit2, short_budget)
        }));
        match blocked {
            Ok(reply) => {
                // 若额度门即时拒绝，也只会是结构化错误；出现 head 才是缺陷。
                assert_ne!(
                    reply["type"], "gateway.response.head",
                    "额度/速率耗尽时第二请求竟然被放行到上游拿到 head：{reply:#}"
                );
                let code = reply["code"].as_str().unwrap_or("");
                assert!(
                    matches!(
                        code,
                        "TIMEOUT" | "CANCELLED" | "REQUEST_LIMIT" | "BUDGET_EXCEEDED"
                    ),
                    "第二请求被拒时的错误码应为额度类，实际：{code}"
                );
            }
            Err(_) => {
                // 15 秒内没有任何回应 = 正在额度门上等待，正是期望行为。
                eprintln!("A3-02 限流：第二请求在 15 秒窗口内被阻塞在额度门（未放行），符合预期");
            }
        }
        broker.revoke_run(conversation_id, "run_0123456789abcdef");
        broker.revoke_run(conversation_id, "run_0123456789abcdea");
    }
}
