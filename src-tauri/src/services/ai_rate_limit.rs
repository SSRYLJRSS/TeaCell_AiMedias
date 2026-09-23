//! 在线 AI 连接的进程内限流。
//!
//! 限制按连接 id 共享到同时运行的多个批次，计数发生在每次实际 HTTP 请求发送前。
//! 窗口使用单调时钟，避免系统时间调整导致配额提前释放；0 表示不限。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{AppError, AppResult};

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(60 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub max_concurrency: u32,
    pub requests_per_minute: u32,
    pub requests_per_hour: u32,
}

impl RateLimitConfig {
    pub fn from_values(
        max_concurrency: i64,
        requests_per_minute: i64,
        requests_per_hour: i64,
    ) -> Self {
        // DB/IPC 层会拒绝负数和过大值；这里仍做防御性收敛，兼容旧 JSON 或外部测试构造。
        Self {
            max_concurrency: max_concurrency.clamp(0, 128) as u32,
            requests_per_minute: requests_per_minute.clamp(0, 1_000_000) as u32,
            requests_per_hour: requests_per_hour.clamp(0, 10_000_000) as u32,
        }
    }

    pub fn is_unlimited(self) -> bool {
        self.max_concurrency == 0 && self.requests_per_minute == 0 && self.requests_per_hour == 0
    }
}

#[derive(Debug, Default)]
struct LimiterState {
    active: u32,
    requests: VecDeque<Instant>,
}

pub struct AiRateLimiter {
    config: RateLimitConfig,
    state: Mutex<LimiterState>,
    wake: Condvar,
}

impl AiRateLimiter {
    fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            state: Mutex::new(LimiterState::default()),
            wake: Condvar::new(),
        }
    }

    /// 等待一个实际 HTTP 请求的额度。等待期间每 250ms 检查一次取消，
    /// 同时由已有请求结束时的 Condvar 唤醒，避免无意义忙等。
    pub fn acquire(&self, cancel: &AtomicBool) -> AppResult<RequestPermit<'_>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::msg("在线服务限流状态锁中毒"))?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::cancelled("已取消等待在线服务限流"));
            }
            let now = Instant::now();
            prune_requests(&mut state.requests, now);
            if self.can_acquire(&state, now) {
                state.active = state.active.saturating_add(1);
                state.requests.push_back(now);
                return Ok(RequestPermit { limiter: self });
            }

            let wait_for = self.wait_duration(&state, now).min(POLL_INTERVAL);
            state = self
                .wake
                .wait_timeout(state, wait_for)
                .map_err(|_| AppError::msg("在线服务限流等待失败"))?
                .0;
        }
    }

    fn can_acquire(&self, state: &LimiterState, now: Instant) -> bool {
        if self.config.max_concurrency > 0 && state.active >= self.config.max_concurrency {
            return false;
        }
        if self.config.requests_per_minute > 0
            && count_since(&state.requests, now, MINUTE) >= self.config.requests_per_minute
        {
            return false;
        }
        if self.config.requests_per_hour > 0
            && state.requests.len() as u32 >= self.config.requests_per_hour
        {
            return false;
        }
        true
    }

    fn wait_duration(&self, state: &LimiterState, now: Instant) -> Duration {
        let mut wait = POLL_INTERVAL;
        if self.config.requests_per_minute > 0
            && count_since(&state.requests, now, MINUTE) >= self.config.requests_per_minute
        {
            if let Some(oldest) = state
                .requests
                .iter()
                .copied()
                .find(|time| now.duration_since(*time) < MINUTE)
            {
                wait = wait.min(MINUTE.saturating_sub(now.duration_since(oldest)));
            }
        }
        if self.config.requests_per_hour > 0
            && state.requests.len() as u32 >= self.config.requests_per_hour
        {
            if let Some(oldest) = state.requests.front().copied() {
                wait = wait.min(HOUR.saturating_sub(now.duration_since(oldest)));
            }
        }
        wait
    }
}

pub struct RequestPermit<'a> {
    limiter: &'a AiRateLimiter,
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.limiter.state.lock() {
            state.active = state.active.saturating_sub(1);
            self.limiter.wake.notify_all();
        }
    }
}

fn count_since(requests: &VecDeque<Instant>, now: Instant, window: Duration) -> u32 {
    requests
        .iter()
        .filter(|time| now.duration_since(**time) < window)
        .count() as u32
}

fn prune_requests(requests: &mut VecDeque<Instant>, now: Instant) {
    while requests
        .front()
        .is_some_and(|time| now.duration_since(*time) >= HOUR)
    {
        requests.pop_front();
    }
}

type Registry = HashMap<String, (RateLimitConfig, Arc<AiRateLimiter>)>;

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// 取得某个连接的共享限流器。配置全为 0 时返回 None，保持旧行为的零开销路径。
pub fn for_connection(
    connection_id: &str,
    max_concurrency: i64,
    requests_per_minute: i64,
    requests_per_hour: i64,
) -> Option<Arc<AiRateLimiter>> {
    let config =
        RateLimitConfig::from_values(max_concurrency, requests_per_minute, requests_per_hour);
    if config.is_unlimited() {
        return None;
    }
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut entries = registry.lock().ok()?;
    if let Some((old_config, limiter)) = entries.get(connection_id) {
        if *old_config == config {
            return Some(Arc::clone(limiter));
        }
    }
    let limiter = Arc::new(AiRateLimiter::new(config));
    entries.insert(connection_id.to_string(), (config, Arc::clone(&limiter)));
    Some(limiter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_limits_are_unlimited() {
        let config = RateLimitConfig::from_values(0, 0, 0);
        assert!(config.is_unlimited());
        assert!(for_connection("unlimited-test", 0, 0, 0).is_none());
    }

    #[test]
    fn cancelled_wait_does_not_block_when_concurrency_is_full() {
        let limiter = AiRateLimiter::new(RateLimitConfig::from_values(1, 0, 0));
        let cancel = AtomicBool::new(false);
        let permit = limiter.acquire(&cancel).unwrap();
        cancel.store(true, Ordering::Relaxed);
        let error = match limiter.acquire(&cancel) {
            Ok(_) => panic!("已取消的请求不应获得限流许可"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "CANCELLED");
        drop(permit);
    }
}
