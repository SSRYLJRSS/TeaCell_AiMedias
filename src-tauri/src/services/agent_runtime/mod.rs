//! Owns the bundled Pi process and its bounded newline-JSON control protocol.
//! The first production target is Windows x64. A3-02's Rust gateway bridge is
//! installed, while its production transport remains disabled pending external
//! routing and cost-policy validation.

pub mod gateway;
mod protocol;

#[cfg(windows)]
mod windows;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{encode_frame, valid_token, FrameDecoder, MAX_FRAME_BYTES, MAX_PROMPT_BYTES};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::Manager;

#[cfg(windows)]
use std::fs::File;

use crate::error::{AppError, AppResult};
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
use crate::services::agent_runtime::gateway::GatewayReplySink;
use crate::services::agent_runtime::gateway::{
    DisabledGatewayTransport, GatewayBroker, GatewayTransport,
};
#[cfg(all(target_os = "windows", target_arch = "x86_64", test))]
use crate::services::agent_runtime::gateway::{GatewayTransportResponse, GatewayUpstreamRequest};
use crate::services::credentials::{CredentialBackend, SystemCredentialBackend};
use crate::state::Database;

const STDERR_BUDGET: usize = 64 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_ACK_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

pub type EventSink = Arc<dyn Fn(Value) + Send + Sync + 'static>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHostStatus {
    pub host_instance_id: String,
    pub protocol_version: u32,
    pub sdk_version: String,
    pub recovered_stranded_host: bool,
    pub model_network_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConversationStatus {
    pub host_instance_id: String,
    pub conversation_ref: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRunDispatchState {
    Accepted,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunDispatch {
    pub run_id: String,
    pub state: AgentRunDispatchState,
}

#[derive(Debug, Clone)]
struct AgentHostPaths {
    node: PathBuf,
    bootstrap: PathBuf,
    package_root: PathBuf,
    agent_dir: PathBuf,
}

impl AgentHostPaths {
    fn resolve(app: &tauri::AppHandle, data_dir: &Path) -> AppResult<Self> {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            let (node, bootstrap, package_root) = if cfg!(debug_assertions) {
                let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .ok_or_else(|| AppError::internal("Agent Host开发资源目录无效"))?
                    .to_path_buf();
                let runtime = repository
                    .join("artifacts")
                    .join("agent-runtime")
                    .join("node-v24.19.0-win-x64")
                    .join("node.exe");
                let package = repository.join("packages").join("agent-host");
                (runtime, package.join("src").join("bootstrap.mjs"), package)
            } else {
                let resource = app
                    .path()
                    .resource_dir()
                    .map_err(|_| AppError::internal("Agent Host打包资源目录不可用"))?;
                let root = resource.join("agent-host");
                let runtime = root
                    .join("runtime")
                    .join("node-v24.19.0-win-x64")
                    .join("node.exe");
                let package = root.join("host");
                (runtime, package.join("src").join("bootstrap.mjs"), package)
            };
            let agent_dir = data_dir.join("agent-runtime");
            for path in [&node, &bootstrap] {
                if !path.is_file() {
                    return Err(AppError::internal(
                        "Agent Host运行资源缺失；请检查安装包或开发资源准备",
                    ));
                }
            }
            if !package_root.join("node_modules").is_dir() {
                return Err(AppError::internal(
                    "Agent Host依赖资源缺失；请重新准备开发或打包资源",
                ));
            }
            Ok(Self {
                node: std::fs::canonicalize(&node)
                    .map_err(|_| AppError::internal("Agent Host Node运行时路径不可用"))?,
                bootstrap: std::fs::canonicalize(&bootstrap)
                    .map_err(|_| AppError::internal("Agent Host启动脚本路径不可用"))?,
                package_root: std::fs::canonicalize(&package_root)
                    .map_err(|_| AppError::internal("Agent Host包路径不可用"))?,
                agent_dir,
            })
        }
        #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
        {
            let _ = (app, data_dir);
            Err(AppError::unsupported(
                "Pi Agent Host当前只支持Windows x64；此平台入口保持禁用",
            ))
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    paths: Option<AgentHostPaths>,
    event_sink: Option<EventSink>,
    gateway_config: Option<GatewayRuntimeConfig>,
    host: Option<Arc<ManagedHost>>,
    starting: bool,
}

#[derive(Clone)]
struct GatewayRuntimeConfig {
    db: Arc<Database>,
    credential_backend: Arc<dyn CredentialBackend>,
    transport: Arc<dyn GatewayTransport>,
}

pub struct AgentRuntime {
    state: Mutex<RuntimeState>,
    exiting: AtomicBool,
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self {
            state: Mutex::new(RuntimeState::default()),
            exiting: AtomicBool::new(false),
        }
    }
}

impl AgentRuntime {
    pub fn configure_for_app(
        &self,
        app: &tauri::AppHandle,
        data_dir: &Path,
        db: Arc<Database>,
        sink: EventSink,
    ) {
        if self.exiting.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.gateway_config = Some(GatewayRuntimeConfig {
                db,
                credential_backend: Arc::new(SystemCredentialBackend),
                transport: Arc::new(DisabledGatewayTransport),
            });
        }
        match AgentHostPaths::resolve(app, data_dir) {
            Ok(paths) => match self.state.lock() {
                Ok(mut state) => {
                    state.paths = Some(paths);
                    state.event_sink = Some(sink);
                }
                Err(_) => tracing::error!(
                    "operation = agent_host_configure error_code = state_lock_poisoned"
                ),
            },
            Err(error) => tracing::info!(
                operation = "agent_host_configure",
                error_code = "unsupported_or_unavailable",
                message = %error,
                "Agent Host未在此平台启用"
            ),
        }
    }

    pub fn start(&self, recover_stranded: bool) -> AppResult<AgentHostStatus> {
        if self.exiting.load(Ordering::Acquire) {
            return Err(AppError::conflict("应用正在退出，不能启动Agent Host"));
        }
        let (paths, sink, gateway_config) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AppError::internal("Agent Host状态锁不可用"))?;
            if self.exiting.load(Ordering::Acquire) {
                return Err(AppError::conflict("应用正在退出，不能启动Agent Host"));
            }
            if let Some(host) = state.host.as_ref() {
                if host.is_healthy() {
                    return Ok(host.status(false));
                }
                host.kill_and_reap();
                state.host = None;
                if !recover_stranded {
                    return Err(AppError::conflict(
                        "Agent Host已中断；核对会话与任务后，由用户明确选择恢复再启动",
                    ));
                }
            }
            if state.starting {
                return Err(AppError::conflict("Agent Host正在启动"));
            }
            let paths = state
                .paths
                .clone()
                .ok_or_else(|| AppError::unsupported("Agent Host运行资源未准备或当前平台不支持"))?;
            let sink = state.event_sink.clone().unwrap_or_else(|| Arc::new(|_| {}));
            let gateway_config = state.gateway_config.clone();
            state.starting = true;
            (paths, sink, gateway_config)
        };

        let launched =
            ManagedHost::launch(paths, sink, recover_stranded, gateway_config).map(Arc::new);
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::internal("Agent Host状态锁不可用"))?;
        state.starting = false;
        let host = launched?;
        if self.exiting.load(Ordering::Acquire) {
            host.kill_and_reap();
            return Err(AppError::conflict("应用正在退出，Agent Host已停止"));
        }
        let status = host.status(recover_stranded);
        state.host = Some(host);
        Ok(status)
    }

    pub fn open_conversation(
        &self,
        conversation_ref: &str,
        mode: &str,
    ) -> AppResult<AgentConversationStatus> {
        if !valid_token(conversation_ref) || !matches!(mode, "new" | "open" | "resume") {
            return Err(AppError::invalid_arg("会话引用或打开方式无效"));
        }
        let host = self.lock_running()?;
        let response = host.request(
            "conversation.open",
            json!({ "conversationId": conversation_ref, "mode": mode }),
            "ack",
            REQUEST_TIMEOUT,
        )?;
        let session_id = response["sessionId"]
            .as_str()
            .filter(|value| valid_token(value))
            .ok_or_else(|| AppError::internal("Agent Host握手响应格式无效"))?;
        Ok(AgentConversationStatus {
            host_instance_id: host.instance_id.clone(),
            conversation_ref: conversation_ref.to_string(),
            session_id: session_id.to_string(),
        })
    }

    pub fn send(&self, conversation_ref: &str, text: &str) -> AppResult<AgentRunDispatch> {
        if !valid_token(conversation_ref) || text.len() > MAX_PROMPT_BYTES || text.contains('\0') {
            return Err(AppError::invalid_arg("消息格式或长度无效"));
        }
        let host = self.lock_running()?;
        dispatch_run(&host, conversation_ref, text, SEND_ACK_TIMEOUT)
    }

    pub fn cancel_run(&self, conversation_ref: &str, run_id: &str) -> AppResult<()> {
        if !valid_token(conversation_ref) || !valid_token(run_id) {
            return Err(AppError::invalid_arg("会话或运行引用无效"));
        }
        let host = self.lock_running()?;
        host.ensure_active_run(conversation_ref, run_id)?;
        if let Some(gateway) = host.gateway.as_ref() {
            gateway.revoke_run(conversation_ref, run_id);
        }
        host.request(
            "conversation.cancel_run",
            json!({ "conversationId": conversation_ref, "runId": run_id }),
            "ack",
            REQUEST_TIMEOUT,
        )?;
        Ok(())
    }

    pub fn close_conversation(&self, conversation_ref: &str) -> AppResult<()> {
        if !valid_token(conversation_ref) {
            return Err(AppError::invalid_arg("会话引用无效"));
        }
        let host = self.lock_running()?;
        if let Some(gateway) = host.gateway.as_ref() {
            gateway.revoke_conversation(conversation_ref);
        }
        host.request(
            "conversation.close",
            json!({ "conversationId": conversation_ref }),
            "ack",
            REQUEST_TIMEOUT,
        )?;
        host.clear_conversation_run(conversation_ref);
        Ok(())
    }

    pub fn shutdown(&self) {
        self.exiting.store(true, Ordering::Release);
        let host = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.host.take());
        if let Some(host) = host {
            host.shutdown();
        }
    }

    fn lock_running(&self) -> AppResult<Arc<ManagedHost>> {
        if self.exiting.load(Ordering::Acquire) {
            return Err(AppError::conflict("应用正在退出，不能继续使用Agent Host"));
        }
        let state = self
            .state
            .lock()
            .map_err(|_| AppError::internal("Agent Host状态锁不可用"))?;
        if self.exiting.load(Ordering::Acquire) {
            return Err(AppError::conflict("应用正在退出，不能继续使用Agent Host"));
        }
        let host = state.host.as_ref().cloned().ok_or_else(|| {
            AppError::conflict("Agent Host未运行或已中断；由用户明确启动或恢复后重试")
        })?;
        if !host.is_healthy() {
            return Err(AppError::conflict(
                "Agent Host未运行或已中断；由用户明确启动或恢复后重试",
            ));
        }
        Ok(host)
    }
}

struct ActiveRun {
    conversation_id: String,
    run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestDelivery {
    NotSent,
    Rejected,
    Unknown,
}

struct RequestFailure {
    delivery: RequestDelivery,
    error: AppError,
}

impl RequestFailure {
    fn not_sent(error: AppError) -> Self {
        Self {
            delivery: RequestDelivery::NotSent,
            error,
        }
    }

    fn rejected(error: AppError) -> Self {
        Self {
            delivery: RequestDelivery::Rejected,
            error,
        }
    }

    fn unknown(error: AppError) -> Self {
        Self {
            delivery: RequestDelivery::Unknown,
            error,
        }
    }
}

fn dispatch_run(
    host: &ManagedHost,
    conversation_ref: &str,
    text: &str,
    timeout: Duration,
) -> AppResult<AgentRunDispatch> {
    let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
    host.start_run(conversation_ref, &run_id)?;
    let gateway_lease = match host.gateway.as_ref() {
        Some(gateway) => match gateway.issue_run_lease(conversation_ref, &run_id) {
            Ok(lease) => Some(lease),
            Err(error) => {
                host.clear_run(&run_id);
                return Err(error);
            }
        },
        None => None,
    };
    let mut payload = json!({
        "conversationId": conversation_ref,
        "runId": run_id,
        "text": text,
    });
    if let (Some(lease), Some(object)) = (gateway_lease, payload.as_object_mut()) {
        object.insert("gatewayLease".to_string(), lease);
    }
    match host.request_with_delivery("conversation.send", payload, "ack", timeout) {
        Ok(_) => Ok(AgentRunDispatch {
            run_id,
            state: AgentRunDispatchState::Accepted,
        }),
        Err(failure) if failure.delivery == RequestDelivery::Unknown => {
            // Keep the run identity so the GUI can attempt an exact cancellation. Never replay.
            Ok(AgentRunDispatch {
                run_id,
                state: AgentRunDispatchState::OutcomeUnknown,
            })
        }
        Err(failure) => {
            if let Some(gateway) = host.gateway.as_ref() {
                gateway.revoke_run(conversation_ref, &run_id);
            }
            host.clear_run(&run_id);
            Err(failure.error)
        }
    }
}

struct ManagedHost {
    instance_id: String,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<String, SyncSender<PendingResponse>>>>,
    active_run: Arc<Mutex<Option<ActiveRun>>>,
    gateway: Option<Arc<GatewayBroker>>,
    failed: Arc<AtomicBool>,
    shutting_down: AtomicBool,
    request_lock: Mutex<()>,
    #[cfg(windows)]
    job: Mutex<Option<windows::OwnedJob>>,
    #[cfg(windows)]
    directory_guards: Mutex<Option<Vec<File>>>,
    #[cfg(windows)]
    session_file_pins: Arc<Mutex<HashMap<(String, String), File>>>,
}

enum PendingResponse {
    Frame(Value),
    Rejected(HostErrorCode),
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostErrorCode {
    InvalidConversationId,
    InvalidOpenMode,
    RunBusy,
    ConversationExists,
    SessionLimit,
    SessionOpenFailed,
    InvalidPrompt,
    InvalidRunId,
    ConversationNotOpen,
    RunMismatch,
    VersionMismatch,
    InvalidCommand,
    InvalidModelLease,
    HostStartFailed,
    RunFailed,
    SessionFileInvalid,
}

impl HostErrorCode {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "INVALID_CONVERSATION_ID" => Self::InvalidConversationId,
            "INVALID_OPEN_MODE" => Self::InvalidOpenMode,
            "RUN_BUSY" => Self::RunBusy,
            "CONVERSATION_EXISTS" => Self::ConversationExists,
            "SESSION_LIMIT" => Self::SessionLimit,
            "SESSION_OPEN_FAILED" => Self::SessionOpenFailed,
            "INVALID_PROMPT" => Self::InvalidPrompt,
            "INVALID_RUN_ID" => Self::InvalidRunId,
            "CONVERSATION_NOT_OPEN" => Self::ConversationNotOpen,
            "RUN_MISMATCH" => Self::RunMismatch,
            "VERSION_MISMATCH" => Self::VersionMismatch,
            "INVALID_COMMAND" => Self::InvalidCommand,
            "INVALID_MODEL_LEASE" => Self::InvalidModelLease,
            "HOST_START_FAILED" => Self::HostStartFailed,
            "RUN_FAILED" => Self::RunFailed,
            "SESSION_FILE_INVALID" => Self::SessionFileInvalid,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConversationId => "INVALID_CONVERSATION_ID",
            Self::InvalidOpenMode => "INVALID_OPEN_MODE",
            Self::RunBusy => "RUN_BUSY",
            Self::ConversationExists => "CONVERSATION_EXISTS",
            Self::SessionLimit => "SESSION_LIMIT",
            Self::SessionOpenFailed => "SESSION_OPEN_FAILED",
            Self::InvalidPrompt => "INVALID_PROMPT",
            Self::InvalidRunId => "INVALID_RUN_ID",
            Self::ConversationNotOpen => "CONVERSATION_NOT_OPEN",
            Self::RunMismatch => "RUN_MISMATCH",
            Self::VersionMismatch => "VERSION_MISMATCH",
            Self::InvalidCommand => "INVALID_COMMAND",
            Self::InvalidModelLease => "INVALID_MODEL_LEASE",
            Self::HostStartFailed => "HOST_START_FAILED",
            Self::RunFailed => "RUN_FAILED",
            Self::SessionFileInvalid => "SESSION_FILE_INVALID",
        }
    }

    fn is_command_rejection(self) -> bool {
        !matches!(self, Self::HostStartFailed | Self::RunFailed)
    }

    fn app_error(self) -> AppError {
        match self {
            Self::InvalidConversationId
            | Self::InvalidOpenMode
            | Self::InvalidPrompt
            | Self::InvalidRunId => AppError::invalid_arg("Agent Host拒绝了无效命令参数"),
            Self::ConversationExists => AppError::conflict("Agent Host会话已存在"),
            Self::SessionLimit => AppError::conflict("Agent Host已达到同时打开会话上限"),
            Self::RunBusy => AppError::conflict("Agent Host当前已有运行中的回答"),
            Self::ConversationNotOpen => AppError::not_found("Agent Host会话当前未打开"),
            Self::RunMismatch => AppError::conflict("Agent Host运行引用已变化，请检查当前状态"),
            Self::SessionOpenFailed => AppError::internal("Agent Host无法打开会话；请检查脱敏日志"),
            Self::VersionMismatch => AppError::unsupported("Agent Host版本或协议不兼容"),
            Self::InvalidCommand => AppError::unsupported("Agent Host拒绝了不支持的命令"),
            Self::InvalidModelLease => AppError::conflict("Agent Host拒绝了无效模型租约"),
            Self::HostStartFailed => AppError::internal("Agent Host启动失败；请检查脱敏日志"),
            Self::RunFailed => AppError::internal("Agent Host回答失败；请检查脱敏日志"),
            Self::SessionFileInvalid => AppError::conflict("Agent Host会话文件无法安全打开"),
        }
    }
}

impl ManagedHost {
    fn status(&self, recovered_stranded_host: bool) -> AgentHostStatus {
        AgentHostStatus {
            host_instance_id: self.instance_id.clone(),
            protocol_version: protocol::PROTOCOL_VERSION as u32,
            sdk_version: protocol::SDK_VERSION.to_string(),
            recovered_stranded_host,
            model_network_enabled: false,
        }
    }

    fn is_healthy(&self) -> bool {
        if self.failed.load(Ordering::Acquire) || self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        match self.child.lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(None)),
            Err(_) => false,
        }
    }

    fn request(
        &self,
        command: &str,
        payload: Value,
        expected_type: &str,
        timeout: Duration,
    ) -> AppResult<Value> {
        self.request_with_delivery(command, payload, expected_type, timeout)
            .map_err(|failure| failure.error)
    }

    fn request_with_delivery(
        &self,
        command: &str,
        payload: Value,
        expected_type: &str,
        timeout: Duration,
    ) -> Result<Value, RequestFailure> {
        if !self.is_healthy() {
            return Err(RequestFailure::not_sent(AppError::conflict(
                "Agent Host当前不可用",
            )));
        }
        self.request_with_delivery_inner(command, payload, expected_type, timeout, false)
    }

    fn request_with_delivery_inner(
        &self,
        command: &str,
        payload: Value,
        expected_type: &str,
        timeout: Duration,
        allow_shutdown: bool,
    ) -> Result<Value, RequestFailure> {
        let _request_guard = if allow_shutdown {
            None
        } else {
            Some(self.request_lock.lock().map_err(|_| {
                RequestFailure::not_sent(AppError::internal("Agent Host请求锁不可用"))
            })?)
        };
        if !allow_shutdown && self.shutting_down.load(Ordering::Acquire) {
            return Err(RequestFailure::not_sent(AppError::conflict(
                "Agent Host正在退出",
            )));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let request_id = format!("request_{}", uuid::Uuid::new_v4().simple());
        self.pending
            .lock()
            .map_err(|_| RequestFailure::not_sent(AppError::internal("Agent Host请求表不可用")))?
            .insert(request_id.clone(), sender);
        let mut frame = json!({
            "protocol": protocol::PROTOCOL_VERSION,
            "instance": self.instance_id,
            "requestId": request_id,
            "type": command,
        });
        if let (Some(target), Some(additions)) = (frame.as_object_mut(), payload.as_object()) {
            for (key, value) in additions {
                target.insert(key.clone(), value.clone());
            }
        }
        let result = self.write_frame(&frame, allow_shutdown).and_then(|()| {
            let response = receiver
                .recv_timeout(timeout)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => RequestFailure::unknown(AppError::timeout(
                        "Agent Host响应超时；命令结果未知，请勿自动重试",
                    )),
                    mpsc::RecvTimeoutError::Disconnected => RequestFailure::unknown(
                        AppError::conflict("Agent Host已退出；请由用户检查后恢复"),
                    ),
                })?;
            match response {
                PendingResponse::Frame(response) => {
                    if response["type"].as_str() != Some(expected_type) {
                        Err(RequestFailure::unknown(AppError::internal(
                            "Agent Host响应类型不匹配",
                        )))
                    } else if expected_type == "ack"
                        && !ack_matches_request(&response, command, &payload)
                    {
                        Err(RequestFailure::unknown(AppError::conflict(
                            "Agent Host响应身份不匹配；命令结果未知，请检查当前状态",
                        )))
                    } else {
                        Ok(response)
                    }
                }
                PendingResponse::Rejected(code) => {
                    tracing::warn!(
                        operation = "agent_host",
                        error_code = "command_rejected",
                        host_error_code = code.as_str()
                    );
                    Err(RequestFailure::rejected(code.app_error()))
                }
                PendingResponse::Interrupted => Err(RequestFailure::unknown(AppError::conflict(
                    "Agent Host请求因进程关闭而中断",
                ))),
            }
        });
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&request_id);
        }
        result
    }

    fn write_frame(&self, frame: &Value, allow_shutdown: bool) -> Result<(), RequestFailure> {
        let bytes = encode_frame(frame).map_err(|_| {
            RequestFailure::not_sent(AppError::invalid_arg("Agent Host请求超过协议限制"))
        })?;
        let mut stdin = self.stdin.lock().map_err(|_| {
            RequestFailure::not_sent(AppError::conflict("Agent Host输入管道不可用"))
        })?;
        if !allow_shutdown && self.shutting_down.load(Ordering::Acquire) {
            return Err(RequestFailure::not_sent(AppError::conflict(
                "Agent Host正在退出",
            )));
        }
        stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| RequestFailure::unknown(AppError::conflict("Agent Host输入管道已关闭")))
    }

    fn start_run(&self, conversation_id: &str, run_id: &str) -> AppResult<()> {
        let mut active = self
            .active_run
            .lock()
            .map_err(|_| AppError::internal("Agent Host运行状态不可用"))?;
        if active.is_some() {
            return Err(AppError::conflict("Agent Host已有活动回答"));
        }
        *active = Some(ActiveRun {
            conversation_id: conversation_id.to_string(),
            run_id: run_id.to_string(),
        });
        Ok(())
    }

    fn ensure_active_run(&self, conversation_id: &str, run_id: &str) -> AppResult<()> {
        let active = self
            .active_run
            .lock()
            .map_err(|_| AppError::internal("Agent Host运行状态不可用"))?;
        if active.as_ref().is_some_and(|active| {
            active.conversation_id == conversation_id && active.run_id == run_id
        }) {
            Ok(())
        } else {
            Err(AppError::conflict("运行已结束或引用不匹配"))
        }
    }

    fn clear_run(&self, run_id: &str) {
        if let Ok(mut active) = self.active_run.lock() {
            if active
                .as_ref()
                .is_some_and(|active| active.run_id == run_id)
            {
                *active = None;
            }
        }
    }

    fn clear_conversation_run(&self, conversation_id: &str) {
        if let Ok(mut active) = self.active_run.lock() {
            if active
                .as_ref()
                .is_some_and(|active| active.conversation_id == conversation_id)
            {
                *active = None;
            }
        }
    }

    fn interrupt_pending(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(PendingResponse::Interrupted);
            }
        }
    }

    fn kill_and_reap(&self) {
        self.failed.store(true, Ordering::Release);
        self.shutting_down.store(true, Ordering::Release);
        if let Some(gateway) = self.gateway.as_ref() {
            gateway.revoke_all();
        }
        self.interrupt_pending();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        if let Ok(mut job) = self.job.lock() {
            job.take();
        }
        #[cfg(windows)]
        if let Ok(mut pins) = self.session_file_pins.lock() {
            pins.clear();
        }
        #[cfg(windows)]
        if let Ok(mut guards) = self.directory_guards.lock() {
            guards.take();
        }
    }

    fn shutdown(&self) {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        self.shutting_down.store(true, Ordering::Release);
        if let Some(gateway) = self.gateway.as_ref() {
            gateway.revoke_all();
        }
        self.interrupt_pending();
        if !self.failed.load(Ordering::Acquire) {
            let _ = self.request_with_delivery_inner(
                "host.shutdown",
                json!({}),
                "ack",
                SHUTDOWN_TIMEOUT.min(Duration::from_millis(250)),
                true,
            );
            loop {
                if let Ok(mut child) = self.child.lock() {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        self.kill_and_reap();
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn launch(
        paths: AgentHostPaths,
        sink: EventSink,
        recover_stranded: bool,
        gateway_config: Option<GatewayRuntimeConfig>,
    ) -> AppResult<Self> {
        use std::os::windows::process::CommandExt;
        use windows::OwnedJob;

        let agent_dir = resolve_agent_directory(&paths.agent_dir, &paths.package_root)?;
        let agent_dir_guard = pin_agent_directory(&agent_dir, &paths.package_root)?;
        let sessions_dir_guard = pin_sessions_directory(&agent_dir, &paths.package_root)?;

        let instance_id = uuid::Uuid::new_v4().to_string();
        let gateway = gateway_config.map(|config| {
            Arc::new(GatewayBroker::new(
                instance_id.clone(),
                config.db,
                config.credential_backend,
                config.transport,
            ))
        });
        let job = OwnedJob::create()
            .map_err(|_| AppError::internal("Windows无法创建Agent Host进程所有权组"))?;
        let mut command = Command::new(&paths.node);
        command
            .arg(&paths.bootstrap)
            .arg(&agent_dir)
            .arg(&instance_id)
            .arg(env!("CARGO_PKG_VERSION"))
            .arg(if recover_stranded {
                "recover-stranded"
            } else {
                "fresh"
            })
            .current_dir(&agent_dir)
            .env_clear()
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "LANG", "LC_ALL"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|_| AppError::internal("无法启动应用自带的Agent Host运行时"))?;
        if job.attach(&child).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::internal("无法取得Agent Host进程所有权"));
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::internal("Agent Host输入管道创建失败"))?;
        let stdin = Arc::new(Mutex::new(stdin));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::internal("Agent Host输出管道创建失败"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::internal("Agent Host诊断管道创建失败"))?;
        let child = Arc::new(Mutex::new(child));
        let failed = Arc::new(AtomicBool::new(false));
        let startup_diagnostic = Arc::new(Mutex::new(None));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let active_run = Arc::new(Mutex::new(None));
        let session_file_pins = Arc::new(Mutex::new(HashMap::new()));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        spawn_stderr_drain(
            stderr,
            Arc::clone(&child),
            Arc::clone(&failed),
            Arc::clone(&startup_diagnostic),
        );
        spawn_stdout_reader(
            stdout,
            instance_id.clone(),
            Arc::clone(&child),
            Arc::clone(&failed),
            Arc::clone(&pending),
            Arc::clone(&active_run),
            Arc::clone(&stdin),
            gateway.clone(),
            agent_dir.join("sessions"),
            Arc::clone(&session_file_pins),
            sink,
            ready_sender,
        );

        let start_written = stdin
            .lock()
            .map(|mut writer| writer.write_all(b"start\n").and_then(|()| writer.flush()))
            .is_ok_and(|result| result.is_ok());
        if !start_written {
            if let Ok(mut process) = child.lock() {
                let _ = process.kill();
                let _ = process.wait();
            }
            return Err(AppError::internal("Agent Host启动屏障未能解除"));
        }
        let host = Self {
            instance_id,
            child,
            stdin,
            pending,
            active_run,
            gateway,
            failed,
            shutting_down: AtomicBool::new(false),
            request_lock: Mutex::new(()),
            job: Mutex::new(Some(job)),
            directory_guards: Mutex::new(Some(vec![agent_dir_guard, sessions_dir_guard])),
            session_file_pins,
        };
        let handshake_deadline = Instant::now() + READY_TIMEOUT;
        let handshake = host.request(
            "host.hello",
            json!({
                "appBuild": env!("CARGO_PKG_VERSION"),
                "schemaVersion": protocol::SCHEMA_VERSION,
                "sdkVersion": protocol::SDK_VERSION,
            }),
            "ready",
            READY_TIMEOUT,
        );
        let response = match ready_receiver
            .recv_timeout(handshake_deadline.saturating_duration_since(Instant::now()))
        {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(AppError::internal("Agent Host握手失败，请检查脱敏诊断日志")),
            Err(_) => Err(AppError::timeout("Agent Host启动握手超时")),
        };
        let checked = match (handshake, response) {
            (Ok(handshake), Ok(response)) => {
                validate_ready(&handshake)?;
                validate_ready(&response)?;
                if handshake != response {
                    Err(AppError::internal("Agent Host握手响应不一致"))
                } else {
                    Ok(())
                }
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        };
        if let Err(error) = checked {
            let startup_code = startup_diagnostic
                .lock()
                .ok()
                .and_then(|diagnostic| diagnostic.clone());
            host.kill_and_reap();
            return Err(startup_code.map_or(error, |code| {
                AppError::internal(format!("Agent Host启动失败 error_code={code}"))
            }));
        }
        Ok(host)
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    fn launch(
        _paths: AgentHostPaths,
        _sink: EventSink,
        _recover_stranded: bool,
        _gateway_config: Option<GatewayRuntimeConfig>,
    ) -> AppResult<Self> {
        Err(AppError::unsupported("Pi Agent Host当前只支持Windows x64"))
    }
}

impl Drop for ManagedHost {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

fn is_active_run(
    active_run: &Mutex<Option<ActiveRun>>,
    conversation_id: &str,
    run_id: &str,
) -> bool {
    active_run.lock().ok().is_some_and(|active| {
        active.as_ref().is_some_and(|active| {
            active.conversation_id == conversation_id && active.run_id == run_id
        })
    })
}

fn resolve_agent_directory(agent_dir: &Path, package_root: &Path) -> AppResult<PathBuf> {
    let parent = agent_dir
        .parent()
        .ok_or_else(|| AppError::internal("Agent Host数据目录不可用"))?;
    let name = agent_dir
        .file_name()
        .ok_or_else(|| AppError::internal("Agent Host数据目录不可用"))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| AppError::internal("Agent Host数据目录不可用"))?;
    let candidate = parent.join(name);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) => {
            if is_reparse_point(&metadata) {
                return Err(AppError::unsupported("Agent Host数据目录不能是目录链接"));
            }
            if !metadata.is_dir() {
                return Err(AppError::unsupported("Agent Host数据路径必须是目录"));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AppError::internal("Agent Host数据目录不可用")),
            }
            let metadata = std::fs::symlink_metadata(&candidate)
                .map_err(|_| AppError::internal("Agent Host数据目录不可用"))?;
            if is_reparse_point(&metadata) {
                return Err(AppError::unsupported("Agent Host数据目录不能是目录链接"));
            }
            if !metadata.is_dir() {
                return Err(AppError::unsupported("Agent Host数据路径必须是目录"));
            }
        }
        Err(_) => return Err(AppError::internal("Agent Host数据目录不可用")),
    }

    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|_| AppError::internal("Agent Host数据目录不可用"))?;
    validate_agent_directory_child(&canonical, &parent, name)?;
    if canonical == package_root || canonical.starts_with(package_root) {
        return Err(AppError::internal("Agent Host会话目录与只读资源目录冲突"));
    }
    Ok(canonical)
}

fn validate_agent_directory_child(
    canonical: &Path,
    expected_parent: &Path,
    expected_name: &std::ffi::OsStr,
) -> AppResult<()> {
    if canonical.parent() != Some(expected_parent) || canonical.file_name() != Some(expected_name) {
        return Err(AppError::unsupported(
            "Agent Host数据目录必须是应用数据目录的直接子目录",
        ));
    }
    Ok(())
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    let symbolic_link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        symbolic_link || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        symbolic_link
    }
}

#[cfg(windows)]
fn open_pinned_directory(path: &Path) -> AppResult<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| AppError::internal("Agent Host数据目录无法固定"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::internal("Agent Host数据目录不可用"))?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(AppError::unsupported(
            "Agent Host数据目录不能是链接或重解析点",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn pin_agent_directory(agent_dir: &Path, package_root: &Path) -> AppResult<File> {
    let guard = open_pinned_directory(agent_dir)?;
    let canonical = std::fs::canonicalize(agent_dir)
        .map_err(|_| AppError::internal("Agent Host数据目录不可用"))?;
    if canonical != agent_dir {
        return Err(AppError::unsupported(
            "Agent Host数据目录必须是应用数据目录的直接子目录",
        ));
    }
    if canonical == package_root || canonical.starts_with(package_root) {
        return Err(AppError::internal("Agent Host会话目录与只读资源目录冲突"));
    }
    Ok(guard)
}

#[cfg(windows)]
fn pin_sessions_directory(agent_dir: &Path, package_root: &Path) -> AppResult<File> {
    let sessions_dir = agent_dir.join("sessions");
    match std::fs::symlink_metadata(&sessions_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(&sessions_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AppError::internal("Agent Host会话目录不可用")),
            }
        }
        Err(_) => return Err(AppError::internal("Agent Host会话目录不可用")),
    }

    let guard = open_pinned_directory(&sessions_dir)?;
    let canonical = std::fs::canonicalize(&sessions_dir)
        .map_err(|_| AppError::internal("Agent Host会话目录不可用"))?;
    if canonical.parent() != Some(agent_dir)
        || canonical == package_root
        || canonical.starts_with(package_root)
    {
        return Err(AppError::unsupported("Agent Host会话目录必须位于数据根内"));
    }
    Ok(guard)
}

fn valid_session_file_name(value: &str) -> bool {
    value.len() <= 240
        && value.ends_with(".jsonl")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_session_file_for_conversation(file_name: &str, conversation_id: &str) -> bool {
    valid_session_file_name(file_name)
        && valid_token(conversation_id)
        && file_name.ends_with(&format!("_{conversation_id}.jsonl"))
}

#[cfg(windows)]
fn open_pinned_session_file(sessions_dir: &Path, file_name: &str) -> AppResult<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    if !valid_session_file_name(file_name) {
        return Err(AppError::invalid_arg("Agent Host会话文件名无效"));
    }
    let path = sessions_dir.join(file_name);
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)
        .map_err(|_| AppError::conflict("Agent Host会话文件无法安全打开"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AppError::conflict("Agent Host会话文件无法安全打开"))?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let has_one_link = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0
        && info.nNumberOfLinks == 1;
    if !metadata.is_file() || is_reparse_point(&metadata) || !has_one_link {
        return Err(AppError::conflict("Agent Host会话文件必须是单链接普通文件"));
    }
    Ok(file)
}

#[cfg(windows)]
fn write_session_pin_result(
    stdin: &Mutex<ChildStdin>,
    instance_id: &str,
    pin_id: &str,
    approved: bool,
) -> Result<(), &'static str> {
    let response = if approved {
        json!({
            "protocol": protocol::PROTOCOL_VERSION,
            "instance": instance_id,
            "type": "session.pin_result",
            "pinId": pin_id,
            "approved": true,
        })
    } else {
        json!({
            "protocol": protocol::PROTOCOL_VERSION,
            "instance": instance_id,
            "type": "session.pin_result",
            "pinId": pin_id,
            "approved": false,
            "code": HostErrorCode::SessionFileInvalid.as_str(),
        })
    };
    let bytes = encode_frame(&response)?;
    let mut writer = stdin.lock().map_err(|_| "stdin_lock_poisoned")?;
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| "stdin_write_failed")
}

#[cfg(windows)]
fn handle_session_pin_request(
    frame: &Value,
    instance_id: &str,
    sessions_dir: &Path,
    stdin: &Mutex<ChildStdin>,
    pins: &Mutex<HashMap<(String, String), File>>,
) -> Result<(), &'static str> {
    let pin_id = frame["pinId"].as_str().ok_or("missing_pin_id")?;
    let conversation_id = frame["conversationId"]
        .as_str()
        .ok_or("missing_conversation_id")?;
    let file_name = frame["fileName"].as_str().ok_or("missing_session_file")?;
    if !valid_session_file_for_conversation(file_name, conversation_id) {
        return write_session_pin_result(stdin, instance_id, pin_id, false);
    }
    let key = (conversation_id.to_string(), file_name.to_string());
    let already_pinned = pins
        .lock()
        .map_err(|_| "session_pin_lock_poisoned")?
        .contains_key(&key);
    let approved = if already_pinned {
        true
    } else if let Ok(file) = open_pinned_session_file(sessions_dir, file_name) {
        pins.lock()
            .map_err(|_| "session_pin_lock_poisoned")?
            .insert(key, file);
        true
    } else {
        false
    };
    write_session_pin_result(stdin, instance_id, pin_id, approved)
}

#[cfg(windows)]
fn handle_session_unpin_request(
    frame: &Value,
    pins: &Mutex<HashMap<(String, String), File>>,
) -> Result<(), &'static str> {
    let conversation_id = frame["conversationId"]
        .as_str()
        .ok_or("missing_conversation_id")?;
    let file_name = frame["fileName"].as_str().ok_or("missing_session_file")?;
    if !valid_session_file_for_conversation(file_name, conversation_id) {
        return Err("invalid_session_file_identity");
    }
    pins.lock()
        .map_err(|_| "session_pin_lock_poisoned")?
        .remove(&(conversation_id.to_string(), file_name.to_string()));
    Ok(())
}

fn fields_match(frame: &Value, allowed: &[&str], required: &[&str]) -> bool {
    let Some(object) = frame.as_object() else {
        return false;
    };
    object.keys().all(|key| allowed.contains(&key.as_str()))
        && required.iter().all(|key| object.contains_key(*key))
}

fn valid_request_id(frame: &Value) -> bool {
    frame["requestId"].as_str().is_some_and(valid_token)
}

fn valid_gateway_error_details(value: &Value) -> bool {
    if !fields_match(value, &["code", "status"], &["code"])
        || !value["code"]
            .as_str()
            .is_some_and(gateway::is_allowed_gateway_error)
    {
        return false;
    }
    value.get("status").is_none_or(|status| {
        status
            .as_u64()
            .is_some_and(|status| (100..=599).contains(&status))
    })
}

fn ack_matches_request(frame: &Value, command: &str, payload: &Value) -> bool {
    if frame["command"].as_str() != Some(command) {
        return false;
    }
    match command {
        "conversation.open" | "conversation.close" => {
            frame["conversationId"] == payload["conversationId"]
        }
        "conversation.send" | "conversation.cancel_run" => {
            frame["conversationId"] == payload["conversationId"]
                && frame["runId"] == payload["runId"]
        }
        "host.shutdown" => true,
        _ => false,
    }
}

fn validate_host_frame(frame: &Value, ready_seen: bool) -> Result<(), &'static str> {
    let base = ["protocol", "instance", "sequence", "type"];
    let Some(kind) = frame["type"].as_str() else {
        return Err("missing_event_type");
    };
    let has_base = base.iter().all(|key| frame.get(*key).is_some());
    if !has_base {
        return Err("missing_frame_base_field");
    }
    match kind {
        "session.pin_request" => {
            let valid_session_identity = frame["conversationId"]
                .as_str()
                .zip(frame["fileName"].as_str())
                .is_some_and(|(conversation_id, file_name)| {
                    valid_session_file_for_conversation(file_name, conversation_id)
                });
            if !ready_seen
                || !fields_match(
                    frame,
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "pinId",
                        "conversationId",
                        "fileName",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "pinId",
                        "conversationId",
                        "fileName",
                    ],
                )
                || !frame["pinId"].as_str().is_some_and(valid_token)
                || !valid_session_identity
            {
                return Err("invalid_session_pin_request");
            }
        }
        "session.unpin_request" => {
            let valid_session_identity = frame["conversationId"]
                .as_str()
                .zip(frame["fileName"].as_str())
                .is_some_and(|(conversation_id, file_name)| {
                    valid_session_file_for_conversation(file_name, conversation_id)
                });
            if !ready_seen
                || !fields_match(
                    frame,
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "conversationId",
                        "fileName",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "conversationId",
                        "fileName",
                    ],
                )
                || !valid_session_identity
            {
                return Err("invalid_session_unpin_request");
            }
        }
        "ready" => {
            if !fields_match(
                frame,
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "requestId",
                    "schemaVersion",
                    "sdk",
                    "appBuild",
                    "tools",
                    "modelNetwork",
                    "gatewayProtocol",
                ],
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "requestId",
                    "schemaVersion",
                    "sdk",
                    "appBuild",
                    "tools",
                    "modelNetwork",
                    "gatewayProtocol",
                ],
            ) || !valid_request_id(frame)
                || validate_ready(frame).is_err()
            {
                return Err("invalid_ready_frame");
            }
        }
        "gateway.request.open"
        | "gateway.request.chunk"
        | "gateway.request.commit"
        | "gateway.response.pull"
        | "gateway.request.cancel" => {
            if !ready_seen || gateway::parse_host_gateway_frame(frame).is_err() {
                return Err("invalid_gateway_frame");
            }
        }
        "ack" => {
            if !valid_request_id(frame) || frame["command"].as_str().is_none() {
                return Err("invalid_ack_frame");
            }
            let command = frame["command"].as_str().unwrap_or_default();
            let common = [
                "protocol",
                "instance",
                "sequence",
                "type",
                "requestId",
                "command",
            ];
            let (allowed, required): (&[&str], &[&str]) = match command {
                "conversation.open" => (
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                        "sessionId",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                        "sessionId",
                    ],
                ),
                "conversation.send" | "conversation.cancel_run" => (
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                        "runId",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                        "runId",
                    ],
                ),
                "conversation.close" => (
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "requestId",
                        "command",
                        "conversationId",
                    ],
                ),
                "host.shutdown" => (&common, &common),
                _ => return Err("unknown_ack_command"),
            };
            if !fields_match(frame, allowed, required)
                || frame
                    .get("conversationId")
                    .is_some_and(|value| value.as_str().is_none_or(|id| !valid_token(id)))
                || frame
                    .get("runId")
                    .is_some_and(|value| value.as_str().is_none_or(|id| !valid_token(id)))
                || frame
                    .get("sessionId")
                    .is_some_and(|value| value.as_str().is_none_or(|id| !valid_token(id)))
            {
                return Err("invalid_ack_frame");
            }
        }
        "error" => {
            let code = frame["code"]
                .as_str()
                .and_then(HostErrorCode::parse)
                .ok_or("invalid_error_code")?;
            if frame.get("requestId").is_some() {
                if !code.is_command_rejection()
                    || !valid_request_id(frame)
                    || !fields_match(
                        frame,
                        &[
                            "protocol",
                            "instance",
                            "sequence",
                            "type",
                            "requestId",
                            "code",
                        ],
                        &[
                            "protocol",
                            "instance",
                            "sequence",
                            "type",
                            "requestId",
                            "code",
                        ],
                    )
                {
                    return Err("invalid_rejection_frame");
                }
            } else if code == HostErrorCode::HostStartFailed && !ready_seen {
                if !fields_match(
                    frame,
                    &["protocol", "instance", "sequence", "type", "code"],
                    &["protocol", "instance", "sequence", "type", "code"],
                ) {
                    return Err("invalid_start_error_frame");
                }
            } else if code == HostErrorCode::RunFailed
                && fields_match(
                    frame,
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "code",
                        "conversationId",
                        "runId",
                        "gatewayError",
                    ],
                    &[
                        "protocol",
                        "instance",
                        "sequence",
                        "type",
                        "code",
                        "conversationId",
                        "runId",
                    ],
                )
                && frame["conversationId"].as_str().is_some_and(valid_token)
                && frame["runId"].as_str().is_some_and(valid_token)
                && frame
                    .get("gatewayError")
                    .is_none_or(valid_gateway_error_details)
            {
            } else {
                return Err("invalid_error_event");
            }
        }
        "delta" => {
            if !fields_match(
                frame,
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "conversationId",
                    "runId",
                    "text",
                ],
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "conversationId",
                    "runId",
                    "text",
                ],
            ) || !frame["conversationId"].as_str().is_some_and(valid_token)
                || !frame["runId"].as_str().is_some_and(valid_token)
                || frame["text"]
                    .as_str()
                    .is_none_or(|text| text.len() > MAX_FRAME_BYTES)
            {
                return Err("invalid_delta");
            }
        }
        "settled" => {
            if !fields_match(
                frame,
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "conversationId",
                    "runId",
                    "aborted",
                ],
                &[
                    "protocol",
                    "instance",
                    "sequence",
                    "type",
                    "conversationId",
                    "runId",
                    "aborted",
                ],
            ) || !frame["conversationId"].as_str().is_some_and(valid_token)
                || !frame["runId"].as_str().is_some_and(valid_token)
                || !frame["aborted"].is_boolean()
            {
                return Err("invalid_settled_event");
            }
        }
        _ => return Err("unknown_event_type"),
    }
    Ok(())
}

fn validate_ready(frame: &Value) -> AppResult<()> {
    if frame["type"] != "ready"
        || frame["protocol"].as_u64() != Some(protocol::PROTOCOL_VERSION)
        || frame["schemaVersion"].as_u64() != Some(protocol::SCHEMA_VERSION)
        || frame["sdk"].as_str() != Some(protocol::SDK_VERSION)
        || frame["appBuild"].as_str() != Some(env!("CARGO_PKG_VERSION"))
        || frame["modelNetwork"] != false
        || frame["gatewayProtocol"].as_u64() != Some(1)
        || frame["tools"]
            .as_array()
            .is_none_or(|tools| !tools.is_empty())
    {
        return Err(AppError::unsupported("Agent Host协议或版本不兼容"));
    }
    Ok(())
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn spawn_stderr_drain(
    mut stderr: impl Read + Send + 'static,
    child: Arc<Mutex<Child>>,
    failed: Arc<AtomicBool>,
    startup_diagnostic: Arc<Mutex<Option<String>>>,
) {
    thread::Builder::new()
        .name("agent-host-stderr".to_string())
        .spawn(move || {
            let mut total = 0usize;
            let mut buffer = [0u8; 4096];
            let mut line = Vec::new();
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        total = total.saturating_add(read);
                        if total > STDERR_BUDGET {
                            tracing::warn!(operation = "agent_host", error_code = "stderr_limit");
                            failed.store(true, Ordering::Release);
                            if let Ok(mut process) = child.lock() {
                                let _ = process.kill();
                            }
                            break;
                        }
                        for byte in &buffer[..read] {
                            if *byte == b'\n' {
                                record_start_diagnostic(&line, &startup_diagnostic);
                                line.clear();
                            } else if line.len() < 128 {
                                line.push(*byte);
                            }
                        }
                    }
                }
            }
            record_start_diagnostic(&line, &startup_diagnostic);
        })
        .ok();
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn record_start_diagnostic(line: &[u8], destination: &Mutex<Option<String>>) {
    const PREFIX: &str = "agent_host_start_failed error_code=";
    let Ok(line) = std::str::from_utf8(line) else {
        return;
    };
    let Some(code) = line.strip_prefix(PREFIX) else {
        return;
    };
    if code.is_empty()
        || code.len() > 48
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return;
    }
    if let Ok(mut current) = destination.lock() {
        *current = Some(code.to_string());
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[allow(clippy::too_many_arguments)]
fn spawn_stdout_reader(
    mut stdout: impl Read + Send + 'static,
    instance_id: String,
    child: Arc<Mutex<Child>>,
    failed: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<String, SyncSender<PendingResponse>>>>,
    active_run: Arc<Mutex<Option<ActiveRun>>>,
    stdin: Arc<Mutex<ChildStdin>>,
    gateway: Option<Arc<GatewayBroker>>,
    sessions_dir: PathBuf,
    session_file_pins: Arc<Mutex<HashMap<(String, String), File>>>,
    sink: EventSink,
    ready_sender: SyncSender<Result<Value, String>>,
) {
    let reply_stdin = Arc::clone(&stdin);
    let gateway_instance = instance_id.clone();
    let gateway_reply: GatewayReplySink = Arc::new(move |frame| {
        if frame["instance"].as_str() != Some(gateway_instance.as_str()) {
            return;
        }
        let Ok(bytes) = encode_frame(&frame) else {
            return;
        };
        if let Ok(mut writer) = reply_stdin.lock() {
            let _ = writer.write_all(&bytes).and_then(|()| writer.flush());
        }
    });
    thread::Builder::new()
        .name("agent-host-protocol".to_string())
        .spawn(move || {
            let mut decoder = FrameDecoder::default();
            let mut last_sequence = 0u64;
            let mut ready_seen = false;
            let mut ready_sender = Some(ready_sender);
            let mut buffer = [0u8; 8192];
            let result = 'reader: loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break 'reader decoder.finish().map_err(str::to_string),
                    Err(_) => break 'reader Err("stdout_read_failed".to_string()),
                    Ok(read) => {
                        let frames = match decoder.push(&buffer[..read]) {
                            Ok(frames) => frames,
                            Err(code) => break 'reader Err(code.to_string()),
                        };
                        for frame in frames {
                            if frame["protocol"].as_u64() != Some(protocol::PROTOCOL_VERSION)
                                || frame["instance"].as_str() != Some(instance_id.as_str())
                            {
                                break 'reader Err("identity_or_version_mismatch".to_string());
                            }
                            let Some(sequence) = frame["sequence"].as_u64() else {
                                break 'reader Err("invalid_sequence".to_string());
                            };
                            if sequence <= last_sequence {
                                break 'reader Err("non_monotonic_sequence".to_string());
                            }
                            last_sequence = sequence;
                            let Some(kind) = frame["type"].as_str() else {
                                break 'reader Err("missing_event_type".to_string());
                            };
                            if let Err(code) = validate_host_frame(&frame, ready_seen) {
                                break 'reader Err(code.to_string());
                            }
                            if kind == "session.pin_request" {
                                if let Err(code) = handle_session_pin_request(
                                    &frame,
                                    &instance_id,
                                    &sessions_dir,
                                    &stdin,
                                    &session_file_pins,
                                ) {
                                    break 'reader Err(code.to_string());
                                }
                                continue;
                            }
                            if kind == "session.unpin_request" {
                                if let Err(code) =
                                    handle_session_unpin_request(&frame, &session_file_pins)
                                {
                                    break 'reader Err(code.to_string());
                                }
                                continue;
                            }
                            if kind == "ready" {
                                if ready_seen || validate_ready(&frame).is_err() {
                                    break 'reader Err("invalid_ready_frame".to_string());
                                }
                                ready_seen = true;
                                if let Some(sender) = ready_sender.take() {
                                    let _ = sender.send(Ok(frame.clone()));
                                }
                            }
                            if kind.starts_with("gateway.") {
                                let Some(gateway) = gateway.as_ref() else {
                                    break 'reader Err("gateway_unavailable".to_string());
                                };
                                if gateway
                                    .handle_host_frame(frame, Arc::clone(&gateway_reply))
                                    .is_err()
                                {
                                    break 'reader Err("invalid_gateway_frame".to_string());
                                }
                                continue;
                            }
                            let request_id = frame["requestId"].as_str();
                            let response = matches!(kind, "ready" | "ack")
                                || (kind == "error" && request_id.is_some());
                            if response {
                                if let Some(request_id) = request_id {
                                    if let Ok(mut requests) = pending.lock() {
                                        if let Some(sender) = requests.remove(request_id) {
                                            let value = if kind == "error" {
                                                let Some(code) = frame["code"]
                                                    .as_str()
                                                    .and_then(HostErrorCode::parse)
                                                else {
                                                    break 'reader Err(
                                                        "invalid_error_code".to_string()
                                                    );
                                                };
                                                PendingResponse::Rejected(code)
                                            } else {
                                                PendingResponse::Frame(frame)
                                            };
                                            let _ = sender.send(value);
                                        }
                                    }
                                }
                                continue;
                            }
                            if kind == "error"
                                && frame["code"].as_str()
                                    == Some(HostErrorCode::HostStartFailed.as_str())
                            {
                                break 'reader Err("host_start_failed".to_string());
                            }
                            if !matches!(kind, "delta" | "settled" | "error") {
                                break 'reader Err("unknown_event_type".to_string());
                            }
                            let (Some(conversation_id), Some(run_id)) =
                                (frame["conversationId"].as_str(), frame["runId"].as_str())
                            else {
                                break 'reader Err("missing_event_identity".to_string());
                            };
                            if !is_active_run(&active_run, conversation_id, run_id) {
                                // Sequence is still consumed; stale output never reaches a new run.
                                continue;
                            }
                            if kind == "delta"
                                && frame["text"]
                                    .as_str()
                                    .is_none_or(|text| text.len() > MAX_FRAME_BYTES)
                            {
                                break 'reader Err("invalid_delta".to_string());
                            }
                            sink(frame.clone());
                            if matches!(kind, "settled" | "error") {
                                if let Some(gateway) = gateway.as_ref() {
                                    gateway.revoke_run(conversation_id, run_id);
                                }
                            }
                            if kind == "settled" {
                                if let Ok(mut active) = active_run.lock() {
                                    if active
                                        .as_ref()
                                        .is_some_and(|active| active.run_id == run_id)
                                    {
                                        *active = None;
                                    }
                                }
                            }
                        }
                    }
                }
            };
            if let Err(code) = result {
                tracing::warn!(operation = "agent_host", error_code = %code);
                failed.store(true, Ordering::Release);
                if let Some(gateway) = gateway.as_ref() {
                    gateway.revoke_all();
                }
                if let Some(sender) = ready_sender.take() {
                    let _ = sender.send(Err(code));
                }
                if let Ok(mut process) = child.lock() {
                    let _ = process.kill();
                }
                if let Ok(mut requests) = pending.lock() {
                    for (_, sender) in requests.drain() {
                        let _ = sender.send(PendingResponse::Interrupted);
                    }
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::{
        is_active_run, resolve_agent_directory, valid_session_file_for_conversation,
        valid_session_file_name, validate_agent_directory_child, validate_host_frame,
        validate_ready, ActiveRun, HostErrorCode,
    };
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn handshake_requires_exact_build_sdk_gateway_schema_tools_and_offline_mode() {
        let good = json!({
            "type":"ready", "protocol":1, "schemaVersion":3, "sdk":"1.0.4",
            "appBuild": env!("CARGO_PKG_VERSION"), "modelNetwork":false,
            "gatewayProtocol":1, "tools":[]
        });
        assert!(validate_ready(&good).is_ok());
        for (key, value) in [
            ("protocol", json!(2)),
            ("schemaVersion", json!(1)),
            ("sdk", json!("1.1.0")),
            ("modelNetwork", json!(true)),
            ("gatewayProtocol", json!(2)),
            ("tools", json!(["read"])),
        ] {
            let mut changed = good.clone();
            changed[key] = value;
            assert!(validate_ready(&changed).is_err(), "{key}");
        }
    }

    #[test]
    fn host_event_frames_require_fixed_fields_and_typed_values() {
        let valid_delta = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 1,
            "type": "delta",
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "text": "安全文本",
        });
        assert!(validate_host_frame(&valid_delta, true).is_ok());

        let mut unexpected_path = valid_delta.clone();
        unexpected_path["path"] = json!("C:\\Users\\private");
        assert_eq!(
            validate_host_frame(&unexpected_path, true),
            Err("invalid_delta")
        );

        let invalid_settled = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 2,
            "type": "settled",
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "aborted": "false",
        });
        assert_eq!(
            validate_host_frame(&invalid_settled, true),
            Err("invalid_settled_event")
        );

        let invalid_error = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 3,
            "type": "error",
            "code": "RUN_FAILED",
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "message": "private path C:\\Users\\private",
        });
        assert_eq!(
            validate_host_frame(&invalid_error, true),
            Err("invalid_error_event")
        );

        let gateway_error = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 4,
            "type": "error",
            "code": "RUN_FAILED",
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "gatewayError": { "code": "UPSTREAM_HTTP", "status": 429 },
        });
        assert!(validate_host_frame(&gateway_error, true).is_ok());

        let mut unsafe_gateway_error = gateway_error.clone();
        unsafe_gateway_error["gatewayError"]["endpoint"] = json!("https://private.invalid");
        assert_eq!(
            validate_host_frame(&unsafe_gateway_error, true),
            Err("invalid_error_event")
        );

        let mut invalid_gateway_status = gateway_error;
        invalid_gateway_status["gatewayError"]["status"] = json!(700);
        assert_eq!(
            validate_host_frame(&invalid_gateway_status, true),
            Err("invalid_error_event")
        );

        let unknown_code = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 5,
            "type": "error",
            "requestId": "request_0123456789abcdef",
            "code": "C:\\Users\\private",
        });
        assert_eq!(
            validate_host_frame(&unknown_code, true),
            Err("invalid_error_code")
        );

        let pin_request = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 5,
            "type": "session.pin_request",
            "pinId": "pin_0123456789abcdef",
            "conversationId": "conversation_0123456789abcdef",
            "fileName": "2026-10-11T12-00-00-000Z_conversation_0123456789abcdef.jsonl",
        });
        assert!(validate_host_frame(&pin_request, true).is_ok());
        assert!(valid_session_file_name(
            "2026-10-11T12-00-00-000Z_conversation_0123456789abcdef.jsonl"
        ));
        assert!(valid_session_file_for_conversation(
            "2026-10-11T12-00-00-000Z_conversation_0123456789abcdef.jsonl",
            "conversation_0123456789abcdef"
        ));
        assert!(!valid_session_file_name(r"..\\outside.jsonl"));

        let mut invalid_pin = pin_request;
        invalid_pin["fileName"] = json!(r"..\\outside.jsonl");
        assert_eq!(
            validate_host_frame(&invalid_pin, true),
            Err("invalid_session_pin_request")
        );

        let unpin_request = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 6,
            "type": "session.unpin_request",
            "conversationId": "conversation_0123456789abcdef",
            "fileName": "2026-10-11T12-00-00-000Z_conversation_0123456789abcdef.jsonl",
        });
        assert!(validate_host_frame(&unpin_request, true).is_ok());
        let mut wrong_owner = unpin_request;
        wrong_owner["conversationId"] = json!("conversation_other_123456789");
        assert_eq!(
            validate_host_frame(&wrong_owner, true),
            Err("invalid_session_unpin_request")
        );

        let gateway_open = json!({
            "protocol": 1,
            "instance": "instance_0123456789abcdef",
            "sequence": 7,
            "type": "gateway.request.open",
            "requestId": "request_0123456789abcdef",
            "conversationId": "conversation_0123456789abcdef",
            "runId": "run_0123456789abcdef",
            "leaseId": "lease_0123456789abcdef",
            "gatewayRequestId": "gateway_0123456789abcdef",
            "route": "openai_chat",
            "method": "POST"
        });
        assert!(validate_host_frame(&gateway_open, true).is_ok());
        assert_eq!(
            validate_host_frame(&gateway_open, false),
            Err("invalid_gateway_frame")
        );

        let mut arbitrary_url = gateway_open;
        arbitrary_url["url"] = json!("https://example.invalid/collect");
        assert_eq!(
            validate_host_frame(&arbitrary_url, true),
            Err("invalid_gateway_frame")
        );
    }

    #[test]
    fn host_rejections_map_to_safe_classified_errors() {
        let code = HostErrorCode::parse("CONVERSATION_EXISTS").unwrap();
        let error = code.app_error();
        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(error.to_string(), "Agent Host会话已存在");

        let untrusted = r"C:\Users\private\prompt";
        assert!(HostErrorCode::parse(untrusted).is_none());
        assert!(!error.to_string().contains(untrusted));

        let invalid_lease = HostErrorCode::parse("INVALID_MODEL_LEASE").unwrap();
        assert_eq!(invalid_lease.as_str(), "INVALID_MODEL_LEASE");
        assert_eq!(invalid_lease.app_error().code(), "CONFLICT");
    }

    #[test]
    fn agent_directory_canonicalized_outside_expected_parent_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let expected_parent = directory.path().join("app-data");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&expected_parent).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let expected_parent = std::fs::canonicalize(expected_parent).unwrap();
        let outside = std::fs::canonicalize(outside).unwrap();

        let result = validate_agent_directory_child(
            &outside,
            &expected_parent,
            std::ffi::OsStr::new("agent-runtime"),
        );
        assert_eq!(result.unwrap_err().code(), "UNSUPPORTED");
    }

    #[test]
    fn agent_directory_symlink_is_rejected_before_following_or_writing() {
        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        let agent_dir = directory.path().join("agent-runtime");
        let package_root = directory.path().join("package");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&package_root).unwrap();

        #[cfg(windows)]
        {
            let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("repository root")
                .to_path_buf();
            let node = repository
                .join("artifacts")
                .join("agent-runtime")
                .join("node-v24.19.0-win-x64")
                .join("node.exe");
            let output = std::process::Command::new(node)
                .arg("-e")
                .arg("const fs=require('node:fs');const [target,link]=process.argv.slice(1);fs.symlinkSync(target,link,'junction')")
                .arg(&outside)
                .arg(&agent_dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "the bundled Node runtime should create a temp junction: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &agent_dir).unwrap();

        let error =
            resolve_agent_directory(&agent_dir, &std::fs::canonicalize(package_root).unwrap())
                .unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED");
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn late_events_are_accepted_only_for_the_current_conversation_and_run() {
        let active = Mutex::new(Some(ActiveRun {
            conversation_id: "conversation_current_1234".to_string(),
            run_id: "run_current_1234567890".to_string(),
        }));
        assert!(is_active_run(
            &active,
            "conversation_current_1234",
            "run_current_1234567890"
        ));
        assert!(!is_active_run(
            &active,
            "conversation_previous_1234",
            "run_current_1234567890"
        ));
        assert!(!is_active_run(
            &active,
            "conversation_current_1234",
            "run_previous_1234567890"
        ));
        *active.lock().unwrap() = None;
        assert!(!is_active_run(
            &active,
            "conversation_current_1234",
            "run_current_1234567890"
        ));
    }
}

#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
mod windows_integration_tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::time::Instant;

    fn host_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn paths(agent_dir: PathBuf) -> AgentHostPaths {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repository root")
            .to_path_buf();
        let package_root = repository.join("packages").join("agent-host");
        AgentHostPaths {
            node: repository
                .join("artifacts")
                .join("agent-runtime")
                .join("node-v24.19.0-win-x64")
                .join("node.exe"),
            bootstrap: package_root.join("src").join("bootstrap.mjs"),
            package_root,
            agent_dir,
        }
    }

    fn paths_with_fake_host(agent_dir: PathBuf, bootstrap: PathBuf) -> AgentHostPaths {
        let mut host_paths = paths(agent_dir);
        host_paths.bootstrap = bootstrap;
        host_paths
    }

    fn write_fake_host_bootstrap(path: &Path) {
        std::fs::write(
            path,
            r#"
import { createInterface } from 'node:readline';
import { appendFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const [, , agentDir, instance, appBuild] = process.argv;
let sequence = 0;
let started = false;
let activeRun;
let pendingPinRequest;
function frame(type, extra = {}) {
  process.stdout.write(`${JSON.stringify({ protocol: 1, instance, sequence: ++sequence, type, ...extra })}\n`);
}
const input = createInterface({ input: process.stdin });
input.on('line', line => {
  if (!started) { if (line === 'start') started = true; return; }
  const request = JSON.parse(line);
  if (request.type === 'host.hello') {
      frame('ready', { requestId: request.requestId, schemaVersion: 3, sdk: '1.0.4', appBuild, tools: [], modelNetwork: false, gatewayProtocol: 1 });
  } else if (request.type === 'conversation.open') {
    if (request.conversationId === 'conversation_pin_reparse_test_1234') {
      pendingPinRequest = request;
      writeFileSync(join(agentDir, 'pin-ready'), 'ready');
      return;
    }
    if (request.conversationId === 'conversation_hang') {
      writeFileSync(join(agentDir, 'open-seen'), 'seen');
      return;
    }
    if (request.conversationId === 'conversation_reject') {
      frame('error', { requestId: request.requestId, code: 'CONVERSATION_EXISTS' });
      return;
    }
    if (request.conversationId === 'conversation_ack_mismatch') {
      frame('ack', { requestId: request.requestId, command: request.type, conversationId: 'conversation_wrong_123456', sessionId: 'session_fake_0123456789ab' });
      return;
    }
    frame('ack', { requestId: request.requestId, command: request.type, conversationId: request.conversationId, sessionId: 'session_fake_0123456789ab' });
  } else if (request.type === 'conversation.send') {
    activeRun = { conversationId: request.conversationId, runId: request.runId };
    appendFileSync(join(agentDir, 'send-log'), `${request.runId}\n`);
    if (request.text === 'mismatched acknowledgement') {
      frame('ack', { requestId: request.requestId, command: request.type, conversationId: request.conversationId, runId: 'run_0123456789abcdef' });
      return;
    }
    // Deliberately withhold the acknowledgement to exercise unknown delivery.
  } else if (request.type === 'conversation.cancel_run') {
    if (activeRun?.conversationId === request.conversationId && activeRun.runId === request.runId) {
      frame('settled', { conversationId: request.conversationId, runId: request.runId, aborted: true });
      frame('ack', { requestId: request.requestId, command: request.type, conversationId: request.conversationId, runId: request.runId });
      activeRun = undefined;
    }
  } else if (request.type === 'test.release_pin' && pendingPinRequest) {
    frame('session.pin_request', {
      pinId: 'pin_0123456789abcdef',
      conversationId: pendingPinRequest.conversationId,
      fileName: 'session_conversation_pin_reparse_test_1234.jsonl',
    });
  } else if (request.type === 'host.shutdown') {
    process.stdout.write(`${JSON.stringify({ protocol: 1, instance, sequence: ++sequence, type: 'ack', requestId: request.requestId, command: request.type })}\n`, () => process.exit(0));
  } else if (request.type === 'session.pin_result' && pendingPinRequest) {
    if (request.approved !== true) {
      frame('error', { requestId: pendingPinRequest.requestId, code: 'SESSION_FILE_INVALID' });
    }
    pendingPinRequest = undefined;
  }
});
"#,
        )
        .expect("fake Node Host bootstrap should be writable");
    }

    fn next_event(receiver: &mpsc::Receiver<Value>, kind: &str, run_id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = receiver
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for {kind}"));
            if event["type"] == kind && event["runId"] == run_id {
                return event;
            }
        }
    }

    #[derive(Default)]
    struct MockCredentialBackend {
        entries: Mutex<HashMap<String, String>>,
    }

    impl CredentialBackend for MockCredentialBackend {
        fn get(&self, id: &str) -> AppResult<Option<String>> {
            Ok(self.entries.lock().unwrap().get(id).cloned())
        }

        fn save(&self, id: &str, secret: &str) -> AppResult<()> {
            self.entries
                .lock()
                .unwrap()
                .insert(id.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, id: &str) -> AppResult<()> {
            self.entries.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockGatewayTransport {
        observed: Mutex<Option<(String, String, Vec<u8>)>>,
    }

    impl GatewayTransport for MockGatewayTransport {
        fn open(
            &self,
            request: GatewayUpstreamRequest,
        ) -> Result<GatewayTransportResponse, &'static str> {
            *self.observed.lock().unwrap() = Some((
                request.authorization.unwrap_or_default(),
                request.endpoint,
                request.body,
            ));
            let model = request.model;
            let events = [
                json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":0,"model":model,"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}),
                json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":0,"model":model,"choices":[{"index":0,"delta":{"content":"Rust mock connected"},"finish_reason":null}]}),
                json!({"id":"chatcmpl-fixture","object":"chat.completion.chunk","created":0,"model":model,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}),
            ];
            let mut body = events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            body.push_str("data: [DONE]\n\n");
            Ok(GatewayTransportResponse {
                status: 200,
                content_type: "text/event-stream; charset=utf-8".to_string(),
                body: Box::new(Cursor::new(body.into_bytes())),
                cancel: Arc::new(|| {}),
            })
        }
    }

    #[test]
    fn bundled_node_host_uses_only_the_rust_mock_gateway_for_a_leased_model() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let connection_id = "connection_fixture_e2e_00000001";
        let conn = crate::db::init_memory().unwrap();
        crate::db::ai_connections::upsert_with_limits(
            &conn,
            connection_id,
            "Local gateway fixture",
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
        let credentials = Arc::new(MockCredentialBackend::default());
        credentials
            .save(connection_id, "fixture-secret-never-on-stdio")
            .unwrap();
        let transport = Arc::new(MockGatewayTransport::default());

        let (sender, receiver) = mpsc::channel();
        let sink: EventSink = Arc::new(move |event| {
            let _ = sender.send(event);
        });
        let host = ManagedHost::launch(
            paths(directory.path().join("agent-runtime")),
            sink,
            false,
            Some(GatewayRuntimeConfig {
                db,
                credential_backend: credentials,
                transport: transport.clone(),
            }),
        )
        .expect("the pinned Node Host should complete the offline handshake");
        assert!(!host.status(false).model_network_enabled);

        let conversation_id = format!("conversation_{}", uuid::Uuid::new_v4().simple());
        host.request(
            "conversation.open",
            json!({ "conversationId": conversation_id, "mode": "new" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        let dispatch = dispatch_run(
            &host,
            &conversation_id,
            "local end-to-end fixture",
            SEND_ACK_TIMEOUT,
        )
        .unwrap();
        assert!(matches!(
            dispatch.state,
            AgentRunDispatchState::Accepted | AgentRunDispatchState::OutcomeUnknown
        ));

        let mut output = String::new();
        let settled = loop {
            let event = receiver.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|error| {
                let transport_opened = transport.observed.lock().unwrap().is_some();
                let child_status = host.child.lock().unwrap().try_wait().ok().flatten();
                panic!(
                    "mock host event stream ended: {error}; failed={}, transport_opened={transport_opened}, child_status={child_status:?}",
                    host.failed.load(Ordering::Acquire)
                );
            });
            if event["runId"] != dispatch.run_id {
                continue;
            }
            match event["type"].as_str() {
                Some("delta") => output.push_str(event["text"].as_str().unwrap_or_default()),
                Some("settled") => break event,
                Some("error") => panic!("mock run failed: {event}"),
                _ => {}
            }
        };
        let observed = transport.observed.lock().unwrap().take();
        assert!(
            observed.is_some(),
            "Rust fixture transport was never called; settled={settled}"
        );
        assert_eq!(output, "Rust mock connected", "settled event: {settled}");
        assert_eq!(settled["aborted"], false);

        let observed = observed.unwrap();
        assert_eq!(observed.0, "Bearer fixture-secret-never-on-stdio");
        assert_eq!(observed.1, "https://api.example.test/v1/chat/completions");
        let request = serde_json::from_slice::<Value>(&observed.2).unwrap();
        assert_eq!(request["model"], "fixture-model");
        assert_eq!(request["stream"], true);
        assert!(!request.to_string().contains("fixture-secret"));

        let session_name = host
            .session_file_pins
            .lock()
            .unwrap()
            .keys()
            .find(|(conversation, _)| conversation == &conversation_id)
            .map(|(_, file_name)| file_name.clone())
            .unwrap();
        let transcript = std::fs::read_to_string(
            directory
                .path()
                .join("agent-runtime")
                .join("sessions")
                .join(session_name),
        )
        .unwrap();
        assert!(!transcript.contains("fixture-secret-never-on-stdio"));
        assert_eq!(host.gateway.as_ref().unwrap().lease_count(), 0);
        host.shutdown();
    }

    #[test]
    fn bundled_node_host_streams_cancels_and_exits_within_the_approved_budget() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::channel();
        let sink: EventSink = Arc::new(move |event| {
            let _ = sender.send(event);
        });
        let host = ManagedHost::launch(
            paths(directory.path().join("agent-runtime")),
            sink,
            false,
            None,
        )
        .expect("the pinned bundled Node Host should start without a global Node install");
        let status = host.status(false);
        assert!(!status.model_network_enabled);

        let conversation_id = format!("conversation_{}", uuid::Uuid::new_v4().simple());
        let opened = host
            .request(
                "conversation.open",
                json!({ "conversationId": conversation_id, "mode": "new" }),
                "ack",
                REQUEST_TIMEOUT,
            )
            .unwrap();
        assert!(opened["sessionId"].as_str().is_some());
        assert_eq!(
            host.session_file_pins.lock().unwrap().len(),
            1,
            "the parent must pin the newly created session before returning it"
        );

        let second_conversation_id = format!("conversation_{}", uuid::Uuid::new_v4().simple());
        host.request(
            "conversation.open",
            json!({ "conversationId": second_conversation_id, "mode": "new" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .expect("a second session must open while the first session file is present");
        assert_eq!(host.session_file_pins.lock().unwrap().len(), 2);
        host.request(
            "conversation.close",
            json!({ "conversationId": conversation_id }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .expect("the first session must close while another session file is present");
        assert_eq!(host.session_file_pins.lock().unwrap().len(), 1);
        host.request(
            "conversation.open",
            json!({ "conversationId": conversation_id, "mode": "resume" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .expect("an existing session must be found among unrelated session files");
        assert_eq!(host.session_file_pins.lock().unwrap().len(), 2);

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        host.start_run(&conversation_id, &run_id).unwrap();
        host.request(
            "conversation.send",
            json!({ "conversationId": conversation_id, "runId": run_id, "text": "离线测试" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        assert_eq!(next_event(&receiver, "delta", &run_id)["text"], "离线");
        assert_eq!(next_event(&receiver, "settled", &run_id)["aborted"], false);

        let cancelled_run = format!("run_{}", uuid::Uuid::new_v4().simple());
        host.start_run(&conversation_id, &cancelled_run).unwrap();
        host.request(
            "conversation.send",
            json!({ "conversationId": conversation_id, "runId": cancelled_run, "text": "取消测试" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        host.ensure_active_run(&conversation_id, &cancelled_run)
            .unwrap();
        host.request(
            "conversation.cancel_run",
            json!({ "conversationId": conversation_id, "runId": cancelled_run }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            next_event(&receiver, "settled", &cancelled_run)["aborted"],
            true
        );

        host.request(
            "conversation.close",
            json!({ "conversationId": conversation_id }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        assert_eq!(host.session_file_pins.lock().unwrap().len(), 1);
        host.request(
            "conversation.close",
            json!({ "conversationId": second_conversation_id }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        assert!(
            host.session_file_pins.lock().unwrap().is_empty(),
            "closing each conversation must release its own pinned session file"
        );

        let started = Instant::now();
        host.shutdown();
        assert!(started.elapsed() <= SHUTDOWN_TIMEOUT);
        assert!(!host.is_healthy());
    }

    #[test]
    fn a_second_host_cannot_take_a_live_session_directory() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent-runtime");
        let sink: EventSink = Arc::new(|_| {});
        let first =
            ManagedHost::launch(paths(agent_dir.clone()), Arc::clone(&sink), false, None).unwrap();
        let second = ManagedHost::launch(paths(agent_dir), sink, true, None);
        assert!(second.is_err(), "live lease must not be recovered");
        assert!(
            first.is_healthy(),
            "failed recovery must leave the owner alive"
        );
        first.shutdown();
    }

    #[test]
    fn unknown_send_result_preserves_run_and_shutdown_interrupts_a_pending_request() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent-runtime");
        let bootstrap = directory.path().join("fake-host.mjs");
        write_fake_host_bootstrap(&bootstrap);
        let host = Arc::new(
            ManagedHost::launch(
                paths_with_fake_host(agent_dir.clone(), bootstrap),
                Arc::new(|_| {}),
                false,
                None,
            )
            .expect("the fake Host should complete the production handshake"),
        );

        let rejected = host
            .request(
                "conversation.open",
                json!({ "conversationId": "conversation_reject", "mode": "new" }),
                "ack",
                REQUEST_TIMEOUT,
            )
            .unwrap_err();
        assert_eq!(rejected.code(), "CONFLICT");
        assert_eq!(rejected.to_string(), "Agent Host会话已存在");

        let mismatched_open = host
            .request_with_delivery(
                "conversation.open",
                json!({ "conversationId": "conversation_ack_mismatch", "mode": "new" }),
                "ack",
                REQUEST_TIMEOUT,
            )
            .unwrap_err();
        assert_eq!(mismatched_open.delivery, RequestDelivery::Unknown);
        assert_eq!(mismatched_open.error.code(), "CONFLICT");

        host.request(
            "conversation.open",
            json!({ "conversationId": "conversation_test", "mode": "new" }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();

        let dispatch = dispatch_run(
            &host,
            "conversation_test",
            "uncertain delivery",
            Duration::from_millis(50),
        )
        .unwrap();
        assert_eq!(dispatch.state, AgentRunDispatchState::OutcomeUnknown);
        host.ensure_active_run("conversation_test", &dispatch.run_id)
            .expect("unknown delivery must retain the exact run for cancellation");
        host.request(
            "conversation.cancel_run",
            json!({ "conversationId": "conversation_test", "runId": dispatch.run_id }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(agent_dir.join("send-log"))
                .unwrap()
                .lines()
                .count(),
            1,
            "an uncertain send must never be replayed",
        );

        let mismatched_ack = dispatch_run(
            &host,
            "conversation_test",
            "mismatched acknowledgement",
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(mismatched_ack.state, AgentRunDispatchState::OutcomeUnknown);
        host.ensure_active_run("conversation_test", &mismatched_ack.run_id)
            .expect("an identity-mismatched acknowledgement must preserve the exact run");
        host.request(
            "conversation.cancel_run",
            json!({ "conversationId": "conversation_test", "runId": mismatched_ack.run_id }),
            "ack",
            REQUEST_TIMEOUT,
        )
        .unwrap();

        let worker_host = Arc::clone(&host);
        let pending_open = thread::spawn(move || {
            worker_host
                .request(
                    "conversation.open",
                    json!({ "conversationId": "conversation_hang", "mode": "new" }),
                    "ack",
                    REQUEST_TIMEOUT,
                )
                .is_err()
        });
        let marker = agent_dir.join("open-seen");
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < marker_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            marker.exists(),
            "fake Host must receive the deliberately hanging request"
        );

        let shutdown_started = Instant::now();
        host.shutdown();
        assert!(
            shutdown_started.elapsed() <= SHUTDOWN_TIMEOUT,
            "shutdown must stay inside its one-second budget with a pending request",
        );
        assert!(
            pending_open.join().unwrap(),
            "shutdown must interrupt the waiting command"
        );
        assert!(!host.is_healthy());
    }

    #[test]
    fn session_file_replaced_after_discovery_is_rejected_before_sdk_open() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent-runtime");
        let sessions_dir = agent_dir.join("sessions");
        let outside = directory.path().join("outside");
        let session_file = sessions_dir.join("session_conversation_pin_reparse_test_1234.jsonl");
        let bootstrap = directory.path().join("fake-host.mjs");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(
            &session_file,
            format!(
                "{}\\n",
                json!({
                    "type": "session",
                    "version": 3,
                    "id": "conversation_pin_reparse_test_1234",
                    "timestamp": "2026-10-11T12:00:00.000Z",
                    "cwd": agent_dir,
                })
            ),
        )
        .unwrap();

        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repository root")
            .to_path_buf();
        let node = repository
            .join("artifacts")
            .join("agent-runtime")
            .join("node-v24.19.0-win-x64")
            .join("node.exe");
        write_fake_host_bootstrap(&bootstrap);

        let host = Arc::new(
            ManagedHost::launch(
                paths_with_fake_host(agent_dir.clone(), bootstrap),
                Arc::new(|_| {}),
                false,
                None,
            )
            .expect("the fake Host should complete the production handshake"),
        );
        let worker_host = Arc::clone(&host);
        let opening = thread::spawn(move || {
            worker_host.request(
                "conversation.open",
                json!({
                    "conversationId": "conversation_pin_reparse_test_1234",
                    "mode": "resume",
                }),
                "ack",
                REQUEST_TIMEOUT,
            )
        });
        let marker = agent_dir.join("pin-ready");
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < marker_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            marker.exists(),
            "the Host must reach the session-pin boundary"
        );

        std::fs::remove_file(&session_file).unwrap();
        let output = std::process::Command::new(node)
            .arg("-e")
            .arg("const fs=require('node:fs');const [target,link]=process.argv.slice(1);fs.symlinkSync(target,link,'junction')")
            .arg(&outside)
            .arg(&session_file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "the bundled Node runtime should create a temp junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let release_pin = encode_frame(&json!({ "type": "test.release_pin" })).unwrap();
        {
            let mut stdin = host.stdin.lock().unwrap();
            stdin.write_all(&release_pin).unwrap();
            stdin.flush().unwrap();
        }

        let error = opening.join().unwrap().unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(error.to_string(), "Agent Host会话文件无法安全打开");
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
        host.shutdown();
    }

    #[test]
    fn session_file_pin_rejects_external_hard_links() {
        let _host_test_guard = host_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let sessions_dir = directory.path().join("sessions");
        let outside = directory.path().join("outside.jsonl");
        let file_name = "session_conversation_hardlink_test_1234.jsonl";
        std::fs::create_dir(&sessions_dir).unwrap();
        std::fs::write(&outside, "preserve me").unwrap();
        std::fs::hard_link(&outside, sessions_dir.join(file_name)).unwrap();

        let error = open_pinned_session_file(&sessions_dir, file_name).unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve me");
    }
}
