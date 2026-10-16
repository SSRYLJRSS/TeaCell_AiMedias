// Reuse the application's limiter rather than testing an unrelated second implementation.
#[path = "../../../src-tauri/src/services/ai_rate_limit.rs"]
pub mod ai_rate_limit;
pub mod governance;

pub mod error {
    #[derive(Debug)]
    pub struct AppError(&'static str);
    pub type AppResult<T> = Result<T, AppError>;
    impl AppError {
        pub fn msg(_: &str) -> Self {
            Self("INTERNAL")
        }
        pub fn cancelled(_: &str) -> Self {
            Self("CANCELLED")
        }
        pub fn timeout(_: &str) -> Self {
            Self("TIMEOUT")
        }
        pub fn code(&self) -> &'static str {
            self.0
        }
    }
}
