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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    config: RateLimitConfig,
    active: u32,
    requests: VecDeque<Instant>,
}

pub struct AiRateLimiter {
    state: Mutex<LimiterState>,
    wake: Condvar,
}

impl AiRateLimiter {
    fn new(config: RateLimitConfig) -> Self {
        Self {
            state: Mutex::new(LimiterState {
                config,
                ..LimiterState::default()
            }),
            wake: Condvar::new(),
        }
    }

    fn update_config(&self, config: RateLimitConfig) -> AppResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::internal("在线服务限流状态锁中毒"))?;
        state.config = config;
        self.wake.notify_all();
        Ok(())
    }

    /// 等待一个实际 HTTP 请求的额度。等待期间每 250ms 检查一次取消，
    /// 同时由已有请求结束时的 Condvar 唤醒，避免无意义忙等。
    pub fn acquire(&self, cancel: &AtomicBool) -> AppResult<RequestPermit<'_>> {
        self.acquire_until(cancel, None)
    }

    /// 有截止时间的额度等待：搜索等有整体预算的调用必须同时在等待队列中超时。
    pub fn acquire_until(
        &self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> AppResult<RequestPermit<'_>> {
        self.acquire_slot(cancel, deadline)?;
        Ok(RequestPermit { limiter: self })
    }

    /// Acquire a permit that can remain attached to a streamed request after
    /// the current worker returns. The connection registry keeps the limiter
    /// alive while the response is being pulled by the Agent Host.
    pub fn acquire_until_owned(
        self: &Arc<Self>,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> AppResult<OwnedRequestPermit> {
        self.acquire_slot(cancel, deadline)?;
        Ok(OwnedRequestPermit {
            limiter: Arc::clone(self),
        })
    }

    fn acquire_slot(&self, cancel: &AtomicBool, deadline: Option<Instant>) -> AppResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::msg("在线服务限流状态锁中毒"))?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::cancelled("已取消等待在线服务限流"));
            }
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return Err(AppError::timeout("等待在线服务请求额度超过任务时限"));
            }
            prune_requests(&mut state.requests, now);
            if Self::can_acquire(&state, now) {
                state.active = state.active.saturating_add(1);
                if !state.config.is_unlimited() {
                    state.requests.push_back(now);
                }
                return Ok(());
            }

            let mut wait_for = Self::wait_duration(&state, now).min(POLL_INTERVAL);
            if let Some(deadline) = deadline {
                wait_for = wait_for.min(deadline.saturating_duration_since(now));
            }
            state = self
                .wake
                .wait_timeout(state, wait_for)
                .map_err(|_| AppError::msg("在线服务限流等待失败"))?
                .0;
        }
    }

    fn can_acquire(state: &LimiterState, now: Instant) -> bool {
        if state.config.max_concurrency > 0 && state.active >= state.config.max_concurrency {
            return false;
        }
        if state.config.requests_per_minute > 0
            && count_since(&state.requests, now, MINUTE) >= state.config.requests_per_minute
        {
            return false;
        }
        if state.config.requests_per_hour > 0
            && state.requests.len() as u32 >= state.config.requests_per_hour
        {
            return false;
        }
        true
    }

    fn wait_duration(state: &LimiterState, now: Instant) -> Duration {
        let mut wait = POLL_INTERVAL;
        if state.config.requests_per_minute > 0
            && count_since(&state.requests, now, MINUTE) >= state.config.requests_per_minute
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
        if state.config.requests_per_hour > 0
            && state.requests.len() as u32 >= state.config.requests_per_hour
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
        self.limiter.release();
    }
}

pub struct OwnedRequestPermit {
    limiter: Arc<AiRateLimiter>,
}

impl Drop for OwnedRequestPermit {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

impl AiRateLimiter {
    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
            self.wake.notify_all();
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

type Registry = HashMap<String, Arc<AiRateLimiter>>;

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// 取得某连接唯一的共享限流器。配置修改时保留正在运行的 permit 和已有窗口计数，
/// 避免保存连接时通过替换 limiter 重置并发/RPM/RPH 配额。
pub fn for_connection(
    connection_id: &str,
    max_concurrency: i64,
    requests_per_minute: i64,
    requests_per_hour: i64,
) -> AppResult<Arc<AiRateLimiter>> {
    let config =
        RateLimitConfig::from_values(max_concurrency, requests_per_minute, requests_per_hour);
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut entries = registry
        .lock()
        .map_err(|_| AppError::internal("在线服务限流注册表锁中毒"))?;
    if let Some(limiter) = entries.get(connection_id) {
        limiter.update_config(config)?;
        return Ok(Arc::clone(limiter));
    }
    let limiter = Arc::new(AiRateLimiter::new(config));
    entries.insert(connection_id.to_string(), Arc::clone(&limiter));
    Ok(limiter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_limits_are_unlimited() {
        let config = RateLimitConfig::from_values(0, 0, 0);
        assert!(config.is_unlimited());
        assert!(for_connection("unlimited-test", 0, 0, 0).is_ok());
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

    #[test]
    fn deadline_expires_while_waiting_for_concurrency_permit() {
        let limiter = AiRateLimiter::new(RateLimitConfig::from_values(1, 0, 0));
        let cancel = AtomicBool::new(false);
        let permit = limiter.acquire(&cancel).unwrap();
        let deadline = Instant::now() + Duration::from_millis(25);
        let error = match limiter.acquire_until(&cancel, Some(deadline)) {
            Ok(_) => panic!("超过总时限后不能继续等待额度"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "TIMEOUT");
        drop(permit);
    }

    #[test]
    fn connection_config_updates_preserve_active_requests_and_window_usage() {
        let connection_id = format!("limiter-config-update-{}", uuid::Uuid::new_v4());
        let cancel = AtomicBool::new(false);
        let limiter = for_connection(&connection_id, 1, 0, 0).unwrap();
        let permit = limiter.acquire(&cancel).unwrap();

        let updated = for_connection(&connection_id, 1, 1, 0).unwrap();
        assert!(Arc::ptr_eq(&limiter, &updated));
        let deadline = Instant::now() + Duration::from_millis(25);
        let error = match updated.acquire_until(&cancel, Some(deadline)) {
            Ok(_) => panic!("保存新限额不能丢失仍活动的共享请求"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "TIMEOUT");
        drop(permit);

        let error = match updated
            .acquire_until(&cancel, Some(Instant::now() + Duration::from_millis(25)))
        {
            Ok(_) => panic!("更新配置不能清除已有 RPM 窗口记录"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "TIMEOUT");
    }

    #[test]
    fn owned_permit_keeps_the_shared_connection_slot_until_stream_drop() {
        let limiter = Arc::new(AiRateLimiter::new(RateLimitConfig::from_values(1, 0, 0)));
        let cancel = AtomicBool::new(false);
        let permit = limiter.acquire_until_owned(&cancel, None).unwrap();
        let deadline = Instant::now() + Duration::from_millis(25);
        let error = match limiter.acquire_until(&cancel, Some(deadline)) {
            Ok(_) => panic!("streaming response must retain its concurrency permit"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "TIMEOUT");
        drop(permit);
        assert!(limiter
            .acquire_until(&cancel, Some(Instant::now() + Duration::from_millis(25)))
            .is_ok());
    }
}
