//! Ollama 本地服务运行态（指导书 §8.2 L2 ownership）：
//! - 启动前 ping：已有服务标记 External（应用永不停止 External 服务，§2 非目标第 9 条）；
//! - 应用自启成功：保存 Child/pid 为 AppOwned，防重复启动；
//! - 停止：仅 AppOwned 走 Child kill/wait；Windows 必要时经校验 taskkill /PID <pid> /T；
//! - 应用退出钩子也只停 AppOwned（§8.2 应用退出、明确关闭、安装重启只停止 AppOwned）。
//!
//! 注意：Child 由本模块独占持有（不 Send 复制），Mutex 包裹后由命令层/退出钩子访问。

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::services::ollama_installer;
use crate::services::ollama_setup;

/// 应用自启服务的空闲关闭阈值。
pub const AUTO_STOP_IDLE: Duration = Duration::from_secs(120);
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// L2 ownership（§8.2）
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum ServiceOwnership {
    /// 应用启动前已存在的外部服务（用户自启/安装包自启）：永不停止
    External,
    /// 应用自己拉起的服务：可停止
    #[serde(rename_all = "camelCase")]
    AppOwned { pid: u32, started_at: i64 },
}

impl ServiceOwnership {
    /// 停止条件：仅 AppOwned（§8.2）
    pub fn is_app_owned(&self) -> bool {
        matches!(self, ServiceOwnership::AppOwned { .. })
    }
}

/// 给外部调用方看的快照（不含 Child 句柄，可跨线程序列化）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaRuntimeSnapshot {
    pub ownership: Option<ServiceOwnership>,
    pub last_activity_at: i64,
}

/// 运行态：ownership + AppOwned 的 Child 句柄 + 最近活动时间
pub struct OllamaRuntimeState {
    ownership: Option<ServiceOwnership>,
    child: Option<Child>,
    last_activity_at: i64,
    active_requests: usize,
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl OllamaRuntimeState {
    pub fn new() -> Self {
        Self {
            ownership: None,
            child: None,
            last_activity_at: 0,
            active_requests: 0,
        }
    }

    /// 启动前 ping 到已有服务：标记 External（此处仅记录，绝不执行停止）
    pub fn mark_external(&mut self) {
        self.ownership = Some(ServiceOwnership::External);
        self.child = None;
        self.last_activity_at = now_ts();
        self.active_requests = 0;
    }

    /// 应用自启成功：保存 pid/Child（防重复启动；替换旧句柄时先清理旧 Child）
    pub fn register_app_owned(&mut self, child: Child) {
        let pid = child.id();
        // 防重复启动：若已有 AppOwned 句柄，先停旧的（理论由 ping 短路避免，这里双保险）
        self.stop_app_owned();
        let started_at = now_ts();
        tracing::info!("Ollama 服务由应用持有（AppOwned pid={pid}）");
        self.child = Some(child);
        self.ownership = Some(ServiceOwnership::AppOwned { pid, started_at });
        self.last_activity_at = started_at;
        self.active_requests = 0;
    }

    /// 活动更新（模型请求发生/结束时由调用方调用，L3 runtime 使用）
    pub fn touch_activity(&mut self) {
        self.last_activity_at = now_ts();
    }

    /// 标记一个本地 AI 请求开始。
    pub fn begin_request(&mut self) {
        self.active_requests = self.active_requests.saturating_add(1);
        self.touch_activity();
    }

    /// 标记一个本地 AI 请求结束；计数归零后开始计算空闲时间。
    pub fn end_request(&mut self) {
        self.active_requests = self.active_requests.saturating_sub(1);
        self.touch_activity();
    }

    pub fn active_requests(&self) -> usize {
        self.active_requests
    }

    /// 检查应用持有的 serve 子进程是否仍然活着；已退出时清理过期 ownership。
    pub fn app_owned_alive(&mut self) -> bool {
        if !self
            .ownership
            .as_ref()
            .is_some_and(ServiceOwnership::is_app_owned)
        {
            return false;
        }
        let Some(child) = self.child.as_mut() else {
            self.ownership = None;
            self.active_requests = 0;
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                self.child = None;
                self.ownership = None;
                self.active_requests = 0;
                false
            }
        }
    }

    /// 只对应用自己启动且没有活动请求的服务判定自动停服。
    pub fn should_auto_stop_at(&self, now: i64, idle: Duration) -> bool {
        self.ownership
            .as_ref()
            .is_some_and(ServiceOwnership::is_app_owned)
            && self.active_requests == 0
            && self.last_activity_at > 0
            && now.saturating_sub(self.last_activity_at) >= idle.as_secs() as i64
    }

    pub fn should_auto_stop(&self, idle: Duration) -> bool {
        self.should_auto_stop_at(now_ts(), idle)
    }

    pub fn ownership(&self) -> Option<&ServiceOwnership> {
        self.ownership.as_ref()
    }

    pub fn last_activity_at(&self) -> i64 {
        self.last_activity_at
    }

    pub fn snapshot(&self) -> OllamaRuntimeSnapshot {
        OllamaRuntimeSnapshot {
            ownership: self.ownership.clone(),
            last_activity_at: self.last_activity_at,
        }
    }

    /// 仅停止 AppOwned 服务（§8.2）：Child kill + wait；Windows 兜底 taskkill /T（先校验 pid）。
    /// 返回是否真的有 AppOwned 停止动作；External/None 恒为 false（不误杀）。
    pub fn stop_app_owned(&mut self) -> bool {
        let Some(ownership) = self.ownership.as_ref() else {
            return false;
        };
        if !ownership.is_app_owned() {
            tracing::info!("Ollama 服务为外部服务（External），应用不停止");
            return false;
        }
        let pid = match ownership {
            ServiceOwnership::AppOwned { pid, .. } => *pid,
            ServiceOwnership::External => return false,
        };
        // 优先 Child kill/wait（带短超时的轮询，参考 services/video.rs §4.3 模式）
        let mut confirmed_exit = false;
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(8);
            let _ = child.kill();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        confirmed_exit = true;
                        break;
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            tracing::warn!("Ollama 子进程 kill 后 8s 未退出，转 taskkill 兜底");
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
        }
        // Windows：子进程未确认退出时，经 pid 校验后 taskkill /T 兜底；
        // 已确认退出（或从未持有 Child）时不再 taskkill，避免对已退出进程的噪音报错。
        #[cfg(windows)]
        if !confirmed_exit {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
        self.ownership = None;
        self.child = None;
        self.active_requests = 0;
        tracing::info!("Ollama AppOwned 服务已停止（pid={pid}）");
        true
    }
}

/// 只管理默认 Ollama 地址；LM Studio 等其它本机兼容服务不应被应用擅自启动或停止。
pub fn is_managed_ollama_base(base_url: &str) -> bool {
    let root = ollama_setup::api_root(base_url).to_ascii_lowercase();
    matches!(
        root.as_str(),
        "http://localhost:11434" | "http://127.0.0.1:11434"
    )
}

/// 发起默认 Ollama 启动（若已有服务则标记为 External）。不等待端点就绪，
/// 慢启动由页面复检，AI 请求则调用 `ensure_ready` 等待。
pub fn start_if_needed(runtime: &Arc<Mutex<OllamaRuntimeState>>, proxy: &str) -> AppResult<()> {
    let mut rt = runtime
        .lock()
        .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?;
    if rt.app_owned_alive() {
        return Ok(());
    }
    if ollama_setup::ping(ollama_installer::LOCAL_BASE_URL).running {
        rt.mark_external();
        return Ok(());
    }
    let exe = ollama_installer::detect_installed()
        .exe_path
        .ok_or_else(|| AppError::msg("未检测到已安装的 Ollama，请先一键安装"))?;
    let child = if proxy.trim().is_empty() {
        ollama_installer::start_service(&exe)?
    } else {
        ollama_installer::start_service_with_proxy(&exe, proxy)?
    };
    rt.register_app_owned(child);
    Ok(())
}

/// AI 请求使用的就绪保证：发起启动后最多等待 90 秒，服务晚启动也不会被 20 秒硬切断。
pub fn ensure_ready(runtime: &Arc<Mutex<OllamaRuntimeState>>, proxy: &str) -> AppResult<()> {
    start_if_needed(runtime, proxy)?;
    if ollama_installer::wait_ready(ollama_installer::LOCAL_BASE_URL, READY_TIMEOUT).is_some()
        || ollama_setup::ping(ollama_installer::LOCAL_BASE_URL).running
    {
        return Ok(());
    }
    Err(AppError::timeout(
        "Ollama 服务启动超时，请检查安装状态和 11434 端口后重试",
    ))
}

impl Default for OllamaRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// 造一个真实可杀的子进程（跨平台：Windows 用 ping 长休眠；其他平台用 sleep）
    fn spawn_dummy(secs: u64) -> Child {
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args([
                    "/C",
                    "ping",
                    "127.0.0.1",
                    "-n",
                    &(secs + 1).to_string(),
                    ">",
                    "nul",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn dummy ping")
        }
        #[cfg(not(windows))]
        {
            Command::new("sleep")
                .arg(secs.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn dummy sleep")
        }
    }

    #[test]
    fn empty_state_has_no_ownership() {
        let mut s = OllamaRuntimeState::new();
        assert!(s.ownership().is_none());
        assert!(!s.stop_app_owned()); // 空态不产生停止动作
    }

    #[test]
    fn external_is_never_stopped() {
        let mut s = OllamaRuntimeState::new();
        s.mark_external();
        assert!(matches!(s.ownership(), Some(ServiceOwnership::External)));
        assert!(!s.ownership().unwrap().is_app_owned());
        // 关键断言：External 存在一个真实子进程也不停止
        let mut dummy = spawn_dummy(30);
        assert!(!s.stop_app_owned());
        assert!(matches!(s.ownership(), Some(ServiceOwnership::External)));
        let _ = dummy.kill();
        let _ = dummy.wait();
    }

    #[test]
    fn app_owned_stops_and_clears_ownership() {
        let mut s = OllamaRuntimeState::new();
        let child = spawn_dummy(30);
        let pid = child.id();
        s.register_app_owned(child);
        assert!(matches!(
            s.ownership(),
            Some(ServiceOwnership::AppOwned { pid: p, .. }) if *p == pid
        ));
        assert!(s.ownership().unwrap().is_app_owned());
        // 停止：Child kill/wait → 进程应退出 → ownership 清空
        assert!(s.stop_app_owned());
        assert!(s.ownership().is_none());
    }

    #[test]
    fn auto_stop_requires_idle_and_no_active_requests() {
        let mut s = OllamaRuntimeState::new();
        let child = spawn_dummy(30);
        s.register_app_owned(child);
        s.last_activity_at = 100;
        assert!(!s.should_auto_stop_at(219, Duration::from_secs(120)));
        assert!(s.should_auto_stop_at(220, Duration::from_secs(120)));

        s.begin_request();
        s.last_activity_at = 100;
        assert!(!s.should_auto_stop_at(1_000, Duration::from_secs(120)));
        s.end_request();
        s.last_activity_at = 100;
        assert!(s.should_auto_stop_at(220, Duration::from_secs(120)));
        assert!(s.stop_app_owned());
    }

    #[test]
    fn managed_base_only_covers_default_ollama_ports() {
        assert!(is_managed_ollama_base("http://localhost:11434/v1"));
        assert!(is_managed_ollama_base("http://127.0.0.1:11434"));
        assert!(!is_managed_ollama_base("http://localhost:1234/v1"));
        assert!(!is_managed_ollama_base("https://api.example.com/v1"));
    }

    #[test]
    fn register_app_owned_replaces_previous_child() {
        let mut s = OllamaRuntimeState::new();
        let first = spawn_dummy(30);
        let _first_pid = first.id();
        s.register_app_owned(first);
        let second = spawn_dummy(30);
        let second_pid = second.id();
        s.register_app_owned(second);
        // 旧 child 已被停掉清理；ownership 只保留新 pid
        assert_eq!(
            s.ownership().map(|o| match o {
                ServiceOwnership::AppOwned { pid, .. } => *pid,
                ServiceOwnership::External => 0,
            }),
            Some(second_pid)
        );
    }

    #[test]
    fn snapshot_serializes_camel_case() {
        let mut s = OllamaRuntimeState::new();
        s.mark_external();
        let snap = s.snapshot();
        assert!(serde_json::to_string(&snap)
            .unwrap()
            .contains("\"kind\":\"external\""));
        assert!(serde_json::to_string(&snap)
            .unwrap()
            .contains("\"lastActivityAt\""));
    }

    #[test]
    fn app_owned_serializes_with_pid() {
        let a = ServiceOwnership::AppOwned {
            pid: 123,
            started_at: 456,
        };
        let out = serde_json::to_string(&a).unwrap();
        assert!(out.contains("\"kind\":\"appOwned\""));
        assert!(out.contains("\"pid\":123"));
        assert!(out.contains("\"startedAt\":456"));
    }
}
