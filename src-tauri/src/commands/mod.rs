//! commands 层：只做参数校验与转发，业务在 services / db（共享知识 #2）

pub mod ai_cmd;
pub mod ai_connections_cmd;
pub mod assets_cmd;
pub mod export_cmd;
pub mod import_cmd;
pub mod media_cmd;
pub mod observability_cmd;
pub mod ollama_cmd;
pub mod platform_cmd;
pub mod settings_cmd;
pub mod super_search_cmd;
pub mod tags_cmd;
pub mod thumbnail_cmd;
pub mod video_cmd;

pub use ai_cmd::*;
pub use ai_connections_cmd::*;
pub use assets_cmd::*;
pub use export_cmd::*;
pub use import_cmd::*;
pub use media_cmd::*;
pub use observability_cmd::*;
pub use ollama_cmd::*;
pub use platform_cmd::*;
pub use settings_cmd::*;
pub use super_search_cmd::*;
pub use tags_cmd::*;
pub use thumbnail_cmd::*;
pub use video_cmd::*;
