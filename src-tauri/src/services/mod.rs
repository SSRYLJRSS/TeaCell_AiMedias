pub mod agent_runtime;
pub mod agent_tools;
pub mod ai_cloud;
pub mod ai_rate_limit;
pub mod authorization;
pub mod backup_restore;
pub mod collection_targets;
pub mod conversations;
pub mod credentials;
pub mod dedup;
pub mod exif_meta;
pub mod export_local;
pub mod facets;
pub mod heic_decode;
pub mod imaging;
pub mod import_dispatch;
pub mod import_execute;
pub mod import_prepare;
pub mod import_submission;
pub mod importer;
pub mod installation;
pub mod kinship;
pub mod legacy_assets;
// A4-05：整库迁移/重新定位引擎与维护门（只由可信设置页入口驱动，不注册 Agent 工具）
pub mod library_migration;
pub mod library_move;
pub mod managed_library;
// A5-01：受控 stdio MCP 入口的宿主侧（命名管道 + 配对握手 + 能力分派）
pub mod mcp_bridge;
pub mod media_refill;
pub mod ollama_installer;
pub mod ollama_runtime;
pub mod ollama_setup;
pub mod operation_dispatch;
pub mod operations;
pub mod platform;
// FB2-08：算法提色（Lab k-means），见 §14.5
pub mod palette;
pub mod perceptual;
pub mod plans;
pub mod preview;
pub mod raw_decode;
pub mod selections;
pub mod super_search_ai;
pub mod tagging;
pub mod thumbnail;
// A4-03：回收站软删除计划、恢复核验与逐项收据
pub mod trash;
pub mod video;
pub mod video_proxy;
// A4-08：常用流程每次实例化为新计划并走统一审批
pub mod workflow_run;
// A4-07：个人常用流程（只保存步骤与参数定义，不保存授权或素材集合快照）
pub mod workflows;
