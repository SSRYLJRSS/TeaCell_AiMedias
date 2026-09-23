/** 设置相关命令封装（对应 commands/settings_cmd.rs） */
import { invoke } from "./client";
import type { Settings } from "@/types/settings";

export function getSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

export function saveSettings(s: Settings): Promise<void> {
  return invoke<void>("save_settings", { s });
}

/** 软件数据保存位置（R-33） */
export function getDataDir(): Promise<string> {
  return invoke<string>("get_data_dir");
}

/** 在系统文件管理器中打开数据目录 */
export function openDataDir(): Promise<void> {
  return invoke<void>("open_data_dir");
}

/** W0-9：打开日志目录（data_dir/logs，排障时打包发给支持） */
export function openLogsDir(): Promise<void> {
  return invoke<void>("open_logs_dir");
}

/** 打开使用帮助（系统默认浏览器）。 */
export function openHelpPage(): Promise<void> {
  return invoke<void>("open_help_page");
}

export interface DiagnosticsReport {
  path: string;
  logFiles: number;
  truncatedLogs: number;
  bytes: number;
}

/** 导出脱敏诊断包（日志 + 环境/数据库摘要，不包含 API Key 或素材内容）。 */
export function exportDiagnostics(target: string): Promise<DiagnosticsReport> {
  return invoke<DiagnosticsReport>("export_diagnostics", { target });
}

/** 手动清除缩略图缓存（R-33） */
export { clearThumbnailCache } from "./thumbnail";

/** 重置数据勾选项（对应后端 ResetSelection；false = 保留） */
export interface ResetDataSelection {
  /** 素材库记录（含搜索索引、导出任务；同时清缩略图/预览/代理缓存文件） */
  assets: boolean;
  /** 原始图片/视频文件（永久删除；成功项同时移除素材记录） */
  assetFiles: boolean;
  /** 导出任务历史（不影响素材记录） */
  exportTasks: boolean;
  /** 标签与分类 */
  tags: boolean;
  /** AI 打标任务 */
  aiTasks: boolean;
  /** AI 服务配置（含系统凭据中的密钥） */
  aiConnections: boolean;
  /** 偏好设置（恢复默认） */
  preferences: boolean;
  /** 超级搜索条件与最近使用字段（localStorage） */
  searchState: boolean;
  /** 缓存文件 */
  caches: boolean;
  /** 诊断日志文件 */
  logs: boolean;
}

/** 重置结果报告（对应后端 ResetReport） */
export interface ResetDataReport {
  /** 被删除的素材记录总数（原始文件删除成功的项目也计入） */
  assetsDeleted: number;
  /** 实际从磁盘删除的原始素材文件数 */
  assetFilesDeleted: number;
  /** 原始文件删除失败数；对应素材记录会保留 */
  assetFilesFailed: number;
  exportTasksDeleted: number;
  tagsDeleted: number;
  aiTasksDeleted: number;
  connectionsDeleted: number;
  preferencesReset: boolean;
  searchStateReset: boolean;
  cacheFilesDeleted: number;
  logFilesDeleted: number;
}

/** 分类重置应用数据（设置页「数据与缓存 → 重置数据」） */
export function resetAppData(selection: ResetDataSelection): Promise<ResetDataReport> {
  return invoke<ResetDataReport>("reset_app_data", { selection });
}

/** W5c：备份数据库到指定路径（VACUUM INTO 单文件快照） */
export function backupDb(target: string): Promise<void> {
  return invoke<void>("backup_db", { target });
}

/**
 * W5c：从备份恢复数据库。成功时应用会自动重启（本 Promise 不会 resolve）；
 * 失败时抛出带原因的错误（原库已还原）。
 */
export function restoreDb(source: string): Promise<void> {
  return invoke<void>("restore_db", { source });
}
