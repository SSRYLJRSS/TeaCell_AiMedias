/** Ollama 一键配置（方案 A2）+ 一键安装（方案 A3）命令封装，对应 commands/ollama_cmd.rs */
import { invoke, on } from "./client";
import type { UnlistenFn } from "@tauri-apps/api/event";

export interface GpuInfo {
    name: string | null;
    vramGb: number | null;
    /** nvidia-smi | unknown */
    source: string;
}

export interface ModelRec {
    name: string;
    recommended: boolean;
    note: string;
}

export interface HardwareReport {
    gpu: GpuInfo;
    recommendations: ModelRec[];
}

export interface OllamaPullProgress {
    model: string;
    /** Ollama 阶段文案（pulling manifest / downloading … / success） */
    status: string;
    total: number;
    completed: number;
    done: boolean;
    error: string | null;
}

export function probeOllamaHardware(): Promise<HardwareReport> {
    return invoke<HardwareReport>("ollama_probe_hardware");
}

export function pullOllamaModel(baseUrl: string, model: string): Promise<void> {
    return invoke<void>("ollama_pull", { baseUrl, model });
}

export function onOllamaPullProgress(handler: (p: OllamaPullProgress) => void): Promise<UnlistenFn> {
    return on<OllamaPullProgress>("ollama://pull-progress", handler);
}

// ---- 方案 A3：应用内一键安装 ----

export interface InstallStatus {
    installed: boolean;
    exePath: string | null;
    version: string | null;
    running: boolean;
    models: string[];
    /** 已缓存的安装包路径（存在才返回） */
    installerPath: string | null;
    /** 已缓存的安装包大小（字节） */
    installerSize: number;
}

/** 下载源（内置 auto 特项 + 内置 3 源 + 自定义源） */
export interface DownloadSource {
    id: string;
    label: string;
    url: string;
}

/** 单源测速结果 */
export interface SourceProbe {
    id: string;
    label: string;
    ok: boolean;
    /** 首字节延迟（毫秒）；失败为 null */
    ttfbMs: number | null;
    /** 1MB 样本实测速度（字节/秒）；失败为 null */
    speedBps: number | null;
    /** 失败原因（"timeout" | "HTTP 403" | ...） */
    error: string | null;
}

export interface InstallProgress {
    /** download | install | verify */
    phase: string;
    downloaded: number;
    total: number;
    speedBps: number;
    /** 当前下载源在"本次 ordered 源列表"中的序号（保留，供统计/调试） */
    sourceIdx: number;
    /** 当前下载源的 id（"official"/"ghproxy"/"github"/"custom-*"；前端据此取 label） */
    sourceId: string;
}

export function ollamaInstallStatus(): Promise<InstallStatus> {
    return invoke<InstallStatus>("ollama_install_status");
}

export interface InstallerCacheInfo {
    path: string | null;
    sizeBytes: number;
}

/** File metadata only; no executable, network or GPU probes. */
export function ollamaInstallerCacheInfo(): Promise<InstallerCacheInfo> {
    return invoke<InstallerCacheInfo>("ollama_installer_cache_info");
}

/** 一键安装：preferredSourceId = "auto" | 源 id（"auto" 时后端按内置顺序直接开下） */
export function ollamaDownloadInstall(preferredSourceId: string = "auto"): Promise<void> {
    return invoke<void>("ollama_download_install", { preferredSourceId });
}

/** 列出全部下载源：内置 + 自定义 */
export function ollamaListSources(): Promise<DownloadSource[]> {
    return invoke<DownloadSource[]>("ollama_list_sources");
}

/** 并发测速：ids 缺省 = 全部源；结果不持久化（前端内存缓存 5 分钟） */
export function ollamaProbeSources(ids?: string[]): Promise<SourceProbe[]> {
    return invoke<SourceProbe[]>("ollama_probe_sources", { ids: ids ?? [] });
}

/** 新增自定义下载源（即时落库；返回新源） */
export function ollamaAddCustomSource(label: string, url: string): Promise<DownloadSource> {
    return invoke<DownloadSource>("ollama_add_custom_source", { label, url });
}

/** 删除自定义下载源（即时落库；id 无效则忽略） */
export function ollamaRemoveCustomSource(id: string): Promise<void> {
    return invoke<void>("ollama_remove_custom_source", { id });
}

export function ollamaStartService(): Promise<void> {
    return invoke<void>("ollama_start_service");
}

// ---- L2 运行态（§8.2/§8.5）：ownership + 停止服务 ----

/** 服务归属：应用自启（AppOwned，可停）或外部已有（External，应用永不停止） */
export type OllamaOwnership =
    | { kind: "external" }
    | { kind: "appOwned"; detail: { pid: number; startedAt: number } };

export interface OllamaRuntimeSnapshot {
    ownership: OllamaOwnership | null;
    lastActivityAt: number;
}

export interface OllamaStopResult {
    before: OllamaOwnership | null;
    stopped: boolean;
    after: OllamaRuntimeSnapshot;
}

/** 启动服务（L2：已有服务标记 External；自启保存 AppOwned），返回运行态快照 */
export function ollamaStartServiceWithStatus(): Promise<OllamaRuntimeSnapshot> {
    return invoke<OllamaRuntimeSnapshot>("ollama_start_service");
}

/** 查询本地服务运行态（ownership / 最近活动时间） */
export function ollamaRuntimeStatus(): Promise<OllamaRuntimeSnapshot> {
    return invoke<OllamaRuntimeSnapshot>("ollama_runtime_status");
}

/** 停止服务：仅停止 AppOwned；External 服务应用永不停止（返回 stopped=false 且 before=external） */
export function ollamaStopService(): Promise<OllamaStopResult> {
    return invoke<OllamaStopResult>("ollama_stop_service");
}

/** 删除已缓存的 Ollama 安装包（释放空间；返回是否删除了文件） */
export function ollamaRemoveInstaller(): Promise<boolean> {
    return invoke<boolean>("ollama_remove_installer");
}

export function onOllamaInstallProgress(handler: (p: InstallProgress) => void): Promise<UnlistenFn> {
    return on<InstallProgress>("ollama://install-progress", handler);
}

/** 安装过程日志行（监控输出框） */
export interface InstallLogLine {
    /** 毫秒时间戳（unix） */
    t: number;
    /** info | warn | error */
    level: string;
    msg: string;
}

export function onOllamaInstallLog(handler: (line: InstallLogLine) => void): Promise<UnlistenFn> {
    return on<InstallLogLine>("ollama://install-log", handler);
}

// ---- 本地打标：模型管理（列表/删除/目录） ----

/** 本地已装模型元信息（/api/tags 条目；size 单位字节） */
export interface LocalModelInfo {
    name: string;
    size: number;
}

/** 列出 Ollama 已安装模型（含占用大小） */
export function ollamaListLocalModels(baseUrl: string): Promise<LocalModelInfo[]> {
    return invoke<LocalModelInfo[]>("ollama_list_local_models", { baseUrl });
}

/** 删除 Ollama 已下载的模型（释放磁盘空间；服务端错误原样透传） */
export function ollamaDeleteModel(baseUrl: string, model: string): Promise<void> {
    return invoke<void>("ollama_delete_model", { baseUrl, model });
}

/** 探测本地模型存储目录（OLLAMA_MODELS / 默认 ~/.ollama/models；目录不存在时后端报错） */
export function ollamaModelDir(): Promise<string> {
    return invoke<string>("ollama_model_dir");
}

/** 在系统文件管理器中打开本地模型存储目录 */
export function ollamaOpenModelDir(): Promise<void> {
    return invoke<void>("ollama_open_model_dir");
}
