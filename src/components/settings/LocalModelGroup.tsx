/** 设置页「本地模型」向导卡片（方案 A3 + 改造方案：下载源自选 + 延迟检测）：
 * 小白动线全程两个按钮；配置写入设置草稿后由设置页统一自动保存。
 * 改造点：阶段1 增加「下载源下拉 + 测速 + 自定义源表单」，进度 label 取实际源；
 * 自定义源即时落库（后端直接写 settings），此处同步回填草稿防自动保存回滚。 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Button from "@/components/common/Button";
import ProgressBar from "@/components/common/ProgressBar";
import {
    ollamaAddCustomSource,
    ollamaDeleteModel,
    ollamaDownloadInstall,
    ollamaInstallStatus,
    ollamaListLocalModels,
    ollamaListSources,
    ollamaModelDir,
    ollamaOpenModelDir,
    ollamaProbeSources,
    ollamaRemoveCustomSource,
    ollamaRuntimeStatus,
    ollamaStartService,
    ollamaStopService,
    onOllamaInstallLog,
    onOllamaInstallProgress,
    probeOllamaHardware,
    type DownloadSource,
    type HardwareReport,
    type InstallLogLine,
    type InstallProgress,
    type InstallStatus,
    type LocalModelInfo,
    type OllamaRuntimeSnapshot,
    type SourceProbe,
} from "@/api/ollama";
import { useTauriEvent } from "@/hooks/hooks";
import { useOllamaPull } from "@/hooks/useOllama";
import { usePlatformStore, selectManagedOllama } from "@/stores/platformStore";
import type { ApiProfile, CustomSource, Settings } from "@/types/settings";

const DEFAULT_LOCAL_BASE = "http://localhost:11434/v1";
const PROBE_CACHE_MS = 5 * 60 * 1000; // 测速结果内存缓存 5 分钟

/** §8.5 空闲保留时长展示文案（与后端 KEEP_ALIVE_IDLE=2m 一致） */
const KEEP_ALIVE_LABEL = "2 分钟";

const fmtMb = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(0)} MB`;
const fmtSpeed = (bps: number) =>
    bps >= 1024 * 1024 ? `${(bps / 1024 / 1024).toFixed(1)} MB/s` : `${Math.max(1, Math.round(bps / 1024))} KB/s`;

/** 社区常用 GitHub Releases 反代（加速项：候选一键填入，避免手抄 URL；反代变动快，随时可删） */
const CANDIDATE_MIRRORS: { label: string; url: string }[] = [
    {
        label: "ghproxy.net",
        url: "https://ghproxy.net/https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe",
    },
    {
        label: "moeyy.xyz",
        url: "https://github.moeyy.xyz/https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe",
    },
    {
        label: "fastgit.cc",
        url: "https://fastgit.cc/https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe",
    },
];

interface Props {
    draft: Settings;
    onPatchAi: (patch: Partial<Settings["ai"]>) => void;
    /** 顶层设置补丁（自定义源即时落库后回填草稿，防止自动保存回滚） */
    onPatchSettings: (patch: Partial<Settings>) => void;
    /** 将向导选中的模型同步到实际 AI 连接；批次执行不读取 settings.ai.profiles。 */
    onModelSelected?: (model: string) => Promise<void>;
    notify: (msg: string) => void;
    fail: (msg: string) => void;
}

export default function LocalModelGroup({ draft, onPatchAi, onPatchSettings, onModelSelected, notify, fail }: Props) {
    // R1（三端复核）：组件层防御性 guard。仅 Windows 支持应用管理的 Ollama；
    // 非托管平台不触发任何安装态/模型目录/运行态探针、轮询与命令（不能只用 CSS 隐藏）。
    // ServiceManagement 已在非托管平台不挂载本组件，这里是第二重保护，防止未来直接引用绕过。
    const managedOllama = usePlatformStore(selectManagedOllama);
    const [status, setStatus] = useState<InstallStatus | null>(null);
    const [detectError, setDetectError] = useState(false);
    const [hw, setHw] = useState<HardwareReport | null>(null);
    const [installProg, setInstallProg] = useState<InstallProgress | null>(null);
    const [busy, setBusy] = useState(false);
    const { pullState, pullBusy, pull } = useOllamaPull();

    // ---- 下载引自选 / 测速（改造方案） ----
    const [sources, setSources] = useState<DownloadSource[]>([]);
    const [probes, setProbes] = useState<Record<string, SourceProbe>>({});
    const [probeAt, setProbeAt] = useState(0);
    const [probing, setProbing] = useState(false);
    const [sourceId, setSourceId] = useState<string>(draft.ai.ollamaSourceId ?? "auto");
    const [showCustomForm, setShowCustomForm] = useState(false);
    const [customLabel, setCustomLabel] = useState("");
    const [customUrl, setCustomUrl] = useState("");
    const [customBusy, setCustomBusy] = useState(false);
    const [showOffline, setShowOffline] = useState(false);
    const [manualModel, setManualModel] = useState("");
    // ---- 已装模型管理（v2.13：列表 + 大小 + 删除 + 打开存储目录） ----
    const [models, setModels] = useState<LocalModelInfo[]>([]);
    const [modelsLoading, setModelsLoading] = useState(false);
    /** 待二次确认删除的模型名（防误触；3s 未再点自动复位） */
    const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);
    const [deletingModel, setDeletingModel] = useState<string | null>(null);
    const [modelDir, setModelDir] = useState<string | null>(null);
    // ---- L2 运行态（§8.2/§8.5）：ownership + 停止服务 ----
    const [runtime, setRuntime] = useState<OllamaRuntimeSnapshot | null>(null);
    const [stopping, setStopping] = useState(false);
    const customFormRef = useRef<HTMLDivElement>(null);
    // ---- 安装监控（输出框 + 卡死检测） ----
    const [logs, setLogs] = useState<InstallLogLine[]>([]);
    const [installStartAt, setInstallStartAt] = useState(0);
    const [stalled, setStalled] = useState(false);
    const [lastProgAt, setLastProgAt] = useState(0);
    const logBoxRef = useRef<HTMLDivElement>(null);

    // 安装日志订阅（监控输出框）
    useTauriEvent(
        () =>
            onOllamaInstallLog((line) => {
                setLogs((prev) => [...prev.slice(-400), line]);
            }),
        [],
    );

    // 卡死检测：下载阶段 25s 无任何进度事件 → 提示可能卡住/无数据流入（网络中断、服务器挂起等）
    useEffect(() => {
        if (!busy) {
            setStalled(false);
            return;
        }
        const timer = setInterval(() => {
            if (installProg?.phase === "download" && lastProgAt > 0 && Date.now() - lastProgAt > 25_000) {
                setStalled(true);
            } else {
                setStalled(false);
            }
        }, 3000);
        return () => clearInterval(timer);
    }, [busy, installProg?.phase, lastProgAt]);

    // 自动滚动到监控输出框底部
    useEffect(() => {
        const el = logBoxRef.current;
        if (el) el.scrollTop = el.scrollHeight;
    }, [logs]);

    const refresh = useCallback(async () => {
        setDetectError(false);
        try {
            const st = await ollamaInstallStatus();
            setStatus(st);
            if (st.running) setHw(await probeOllamaHardware().catch(() => null));
            else setHw(null);
            // L2：同步运行态（ownership 只在服务运行时有意义；失败不阻塞向导）
            setRuntime(await ollamaRuntimeStatus().catch(() => null));
        } catch {
            // A-3：检测失败只在「本地打标」分组显示局部错误，不阻塞在线打标/视频开关
            setStatus(null);
            setDetectError(true);
        }
    }, []);

    const loadSources = useCallback(async () => {
        try {
            setSources(await ollamaListSources());
        } catch {
            setSources([]);
        }
    }, []);

    useEffect(() => {
        // 非托管平台：不发起任何 Ollama 管理探针。
        if (!managedOllama) return;
        void refresh();
        void loadSources();
    }, [managedOllama, refresh, loadSources]);

    // ---- 已装模型管理：服务就绪（且为 Ollama）时加载列表与存储目录 ----
    const loadModels = useCallback(async () => {
        setModelsLoading(true);
        try {
            setModels(await ollamaListLocalModels(DEFAULT_LOCAL_BASE));
        } catch {
            // 列表读取失败不打断页面：面板降级为空列表（有「刷新」可重试）
            setModels([]);
        } finally {
            setModelsLoading(false);
        }
    }, []);

    useEffect(() => {
        if (status?.running) {
            void loadModels();
            ollamaModelDir().then(setModelDir).catch(() => setModelDir(null));
        } else {
            setModels([]);
            setModelDir(null);
        }
    }, [status?.running, loadModels]);

    /** 删除模型：首次点击进入确认态（3s 自动复位），二次点击执行（防误触） */
    const onDeleteModel = async (name: string) => {
        if (confirmingDelete !== name) {
            setConfirmingDelete(name);
            window.setTimeout(() => {
                setConfirmingDelete((cur) => (cur === name ? null : cur));
            }, 3000);
            return;
        }
        setConfirmingDelete(null);
        setDeletingModel(name);
        try {
            await ollamaDeleteModel(DEFAULT_LOCAL_BASE, name);
            setModels((prev) => prev.filter((m) => m.name !== name));
            // 若删除的正是当前激活本地档案使用的模型，一并提醒
            const active = draft.ai.profiles.find(
                (p) => p.kind === "local" && p.id === draft.ai.activeProfile,
            );
            notify(
                active && active.model === name
                    ? `已删除模型 ${name}；当前打标档案使用的正是它，请重新拉取或更换模型`
                    : `已删除模型 ${name}`,
            );
            void refresh(); // 同步「推荐模型」的已安装标记与已装模型文本
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setDeletingModel(null);
        }
    };

    const onOpenModelDir = async () => {
        try {
            await ollamaOpenModelDir();
        } catch (e) {
            fail(errMsg(e));
        }
    };

    useTauriEvent(() => onOllamaInstallProgress((p) => {
        setInstallProg(p);
        setLastProgAt(Date.now());
        // 下载阶段有新数据流入即不算卡死
        if (p.phase === "download") setStalled(false);
    }), []);

    const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

    // ---- 源选择：选中即写 draft，设置页自动保存；下载中禁用 ----
    const pickSource = (id: string) => {
        setSourceId(id);
        onPatchAi({ ollamaSourceId: id });
    };

    const doProbe = async () => {
        setProbing(true);
        try {
            const list = await ollamaProbeSources();
            const map: Record<string, SourceProbe> = {};
            for (const p of list) map[p.id] = p;
            setProbes(map);
            setProbeAt(Date.now());
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setProbing(false);
        }
    };

    // 测速结果按速度降序展示
    const orderedProbes = useMemo(() => {
        const ids = sources.map((s) => s.id);
        return ids
            .map((id) => probes[id])
            .filter((p): p is SourceProbe => !!p)
            .sort((a, b) => ((b.speedBps ?? -1) - (a.speedBps ?? -1)));
    }, [sources, probes]);

    const probeFresh = probeAt > 0 && Date.now() - probeAt < PROBE_CACHE_MS;

    const addCustom = async () => {
        if (customBusy) return;
        const label = customLabel.trim();
        const url = customUrl.trim();
        if (!label) return fail("请填写源名称");
        if (!/^https?:\/\//.test(url)) return fail("源地址必须以 http(s):// 开头");
        setCustomBusy(true);
        try {
            const src = await ollamaAddCustomSource(label, url);
            setSources((prev) => [...prev, src]);
            // 回填 draft（后端已即时落库；不更新草稿则自动保存会用旧值覆盖新源）
            onPatchSettings({ customDownloadSources: [...(draft.customDownloadSources ?? []), toCustom(src)] });
            setSourceId(src.id);
            onPatchAi({ ollamaSourceId: src.id });
            setCustomLabel("");
            setCustomUrl("");
            setShowCustomForm(false);
            notify("已添加自定义下载源");
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setCustomBusy(false);
        }
    };

    const removeCustom = async (id: string) => {
        try {
            await ollamaRemoveCustomSource(id);
            setSources((prev) => prev.filter((s) => s.id !== id));
            onPatchSettings({
                customDownloadSources: (draft.customDownloadSources ?? []).filter((c) => c.id !== id),
            });
            if (sourceId === id) {
                setSourceId("auto");
                onPatchAi({ ollamaSourceId: "auto" });
            }
            notify("已删除自定义下载源");
        } catch (e) {
            fail(errMsg(e));
        }
    };

    const onInstall = async () => {
        setBusy(true);
        setInstallProg(null);
        setLogs([]);
        setStalled(false);
        setInstallStartAt(Date.now());
        try {
            await ollamaDownloadInstall(sourceId);
            notify("Ollama 安装完成，服务已就绪");
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setBusy(false);
            setInstallProg(null);
            void refresh();
        }
    };

    const onStartService = async () => {
        setBusy(true);
        try {
            await ollamaStartService();
            notify("Ollama 服务已就绪");
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setBusy(false);
            void refresh();
        }
    };

    /** L2（§8.2）：停止服务——仅 AppOwned 可停；External（用户自启）应用永不停止 */
    const onStopService = async () => {
        setStopping(true);
        try {
            const res = await ollamaStopService();
            if (!res.stopped) {
                fail("该服务不是应用启动的（外部服务），应用不会停止它");
            } else {
                notify("Ollama 服务已停止（模型已释放）");
            }
        } catch (e) {
            fail(errMsg(e));
        } finally {
            setStopping(false);
            void refresh();
        }
    };

    /** 确保存在本地档案并写入模型、置为激活（改草稿后由设置页自动保存） */
    const ensureLocalProfile = (model: string) => {
        const existing = draft.ai.profiles.find((p) => p.kind === "local");
        if (existing) {
            onPatchAi({
                profiles: draft.ai.profiles.map((p) =>
                    p.id === existing.id ? { ...p, model, baseUrl: p.baseUrl || DEFAULT_LOCAL_BASE } : p,
                ),
                activeProfile: existing.id,
            });
        } else {
            const p: ApiProfile = {
                id: crypto.randomUUID(),
                name: "本机 Ollama",
                apiMode: "openai",
                kind: "local",
                baseUrl: DEFAULT_LOCAL_BASE,
                apiKey: "",
                model,
            };
            onPatchAi({ profiles: [...draft.ai.profiles, p], activeProfile: p.id });
        }
    };

    const modelInstalled = (name: string) =>
        (status?.models ?? []).some((m) => m === name || m === `${name}:latest` || m.startsWith(`${name}:`));

    const onPick = async (name: string) => {
        if (modelInstalled(name)) {
            setBusy(true);
            try {
                await onModelSelected?.(name);
                ensureLocalProfile(name);
                notify("模型已就绪并保存到本机服务；如打标当前使用其他服务，请在打标页切换");
            } catch (e) {
                fail(errMsg(e));
            } finally {
                setBusy(false);
            }
            return;
        }
        try {
            await pull(DEFAULT_LOCAL_BASE, name);
            await onModelSelected?.(name);
            ensureLocalProfile(name);
            notify("模型已下载并保存到本机服务；如打标当前使用其他服务，请在打标页切换");
            void refresh();
        } catch (e) {
            fail(errMsg(e));
        }
    };

    if (!status) {
        if (detectError) {
            return (
                <div className="flex flex-col gap-2 p-3">
                    <p className="text-sm text-[var(--color-danger)]">本地环境检测失败</p>
                    <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
                        无法确认 Ollama 是否已安装。这不影响在线 AI 服务；可稍后重试，或在「服务管理」中选择在线服务。
                    </p>
                    <div>
                        <Button variant="ghost" onClick={() => void refresh()}>
                            重试检测
                        </Button>
                    </div>
                </div>
            );
        }
        return <p className="p-3 text-xs text-[var(--color-text-secondary)]">正在检测本地环境…</p>;
    }

    // ---- 阶段 1：未安装 → 一键安装（下载 + 静默安装 + 复检一条龙） ----
    if (!status.installed) {
        const p = installProg;
        const pct = p && p.phase === "download" && p.total > 0 ? p.downloaded / p.total : 0.03;
        // 当前下载源 label：按 sourceId 从 sources 解析（ordered 列表与展示顺序可能不同，用 id 才稳定）
        const activeSrcLabel = p?.sourceId
            ? (sources.find((s) => s.id === p.sourceId)?.label ?? "备用源")
            : "备用源";
        return (
            <div className="flex flex-col gap-2 p-3">
                <p className="text-sm text-[var(--color-text)]">未检测到 Ollama 本地引擎</p>
                <p className="text-xs leading-5 text-[var(--color-text-secondary)]">
                    点击下方按钮即可完成下载、安装和启动；之后可下载视觉模型，在本机离线打标。下载源支持测速，可选择响应较快的镜像。
                </p>

                {/* 下载源选择 + 测速 */}
                <div className="flex items-center gap-2">
                    <select
                        value={sourceId}
                        disabled={busy || probing}
                        onChange={(e) => pickSource(e.target.value)}
                        className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
                    >
                        <option value="auto">自动（测速选最快）</option>
                        {sources.map((s) => (
                            <option key={s.id} value={s.id}>
                                {s.label}
                            </option>
                        ))}
                    </select>
                    <Button variant="ghost" disabled={busy || probing} onClick={() => void doProbe()}>
                        {probing ? "测速中…" : probeFresh ? "重新测速" : "测速"}
                    </Button>
                </div>

                {/* 测速结果面板（有结果即展示） */}
                {(probeFresh || probing || orderedProbes.length > 0) && (
                    <div className="flex flex-col gap-1 rounded-md border border-[var(--color-border)] px-2.5 py-2">
                        {(probing || orderedProbes.length === 0) && (
                            <p className="text-[11px] text-[var(--color-text-secondary)]">正在测速…</p>
                        )}
                        {orderedProbes.map((p) => {
                            const fastest =
                                orderedProbes[0]?.speedBps != null &&
                                p.speedBps === orderedProbes[0].speedBps &&
                                p.speedBps != null;
                            return (
                                <div key={p.id} className="flex items-center gap-2 text-[11px]">
                                    <span className="min-w-0 flex-1 truncate text-[var(--color-text)]">
                                        {p.ok ? (
                                            <>
                                                {p.label}
                                                <span className="ml-1.5 text-[var(--color-text-secondary)]">
                                                    {p.ttfbMs != null ? `${p.ttfbMs}ms` : "—"} · {p.speedBps != null ? fmtSpeed(p.speedBps) : "—"}
                                                </span>
                                                {fastest && (
                                                    <span className="ml-1.5 rounded bg-[var(--color-accent)] px-1 text-[10px] text-white">最快</span>
                                                )}
                                            </>
                                        ) : (
                                            <>
                                                {p.label}
                                                <span className="ml-1.5 text-[var(--color-danger)]">{p.error ?? "失败"}</span>
                                            </>
                                        )}
                                    </span>
                                    {p.id.startsWith("custom-") && (
                                        <button
                                            onClick={() => void removeCustom(p.id)}
                                            className="shrink-0 rounded px-1 text-[var(--color-text-secondary)] hover:text-red-500"
                                        >
                                            删除
                                        </button>
                                    )}
                                </div>
                            );
                        })}
                    </div>
                )}

                {/* 添加自定义源（折叠） */}
                {!showCustomForm ? (
                    <button
                        onClick={() => setShowCustomForm(true)}
                        className="self-start text-xs text-[var(--color-accent)] hover:underline"
                    >
                        + 添加自定义源…
                    </button>
                ) : (
                    <div ref={customFormRef} className="flex flex-col gap-1.5 rounded-md border border-dashed border-[var(--color-border)] p-2">
                        <p className="text-[11px] text-[var(--color-text-secondary)]">
                            常用镜像源（选择后自动填入）：
                        </p>
                        <div className="flex flex-wrap gap-1.5">
                            {CANDIDATE_MIRRORS.map((c) => (
                                <button
                                    key={c.label}
                                    onClick={() => {
                                        setCustomLabel(c.label);
                                        setCustomUrl(c.url);
                                    }}
                                    className="rounded border border-[var(--color-border)] px-2 py-0.5 text-[11px] text-[var(--color-text)] hover:border-[var(--color-accent)] hover:text-[var(--color-accent)]"
                                >
                                    {c.label}
                                </button>
                            ))}
                        </div>
                        <input
                            value={customLabel}
                            onChange={(e) => setCustomLabel(e.target.value)}
                            placeholder="名称（如：我的 NAS 源）"
                            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm outline-none focus:border-[var(--color-accent)]"
                        />
                        <input
                            value={customUrl}
                            onChange={(e) => setCustomUrl(e.target.value)}
                            placeholder="https:// 镜像直链"
                            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm outline-none focus:border-[var(--color-accent)]"
                        />
                        <div className="flex items-center gap-2">
                            <Button variant="primary" disabled={customBusy} onClick={() => void addCustom()}>
                                {customBusy ? "添加中…" : "添加"}
                            </Button>
                            <Button variant="ghost" disabled={customBusy} onClick={() => setShowCustomForm(false)}>
                                取消
                            </Button>
                        </div>
                    </div>
                )}

                {busy && p?.phase === "download" && p.total === 0 && (
                <div className="flex flex-col gap-1">
                    <ProgressBar value={0.05} />
                    <p className="text-[11px] text-[var(--color-text-secondary)]">
                        正在连接（{activeSrcLabel}）… 网络较慢时可能需要较长时间
                    </p>
                </div>
            )}
            {busy && p?.phase === "download" && p.total > 0 && (
                <div className="flex flex-col gap-1">
                    <ProgressBar value={pct} />
                    <p className="text-[11px] text-[var(--color-text-secondary)]">
                        正在下载（{activeSrcLabel}）：{fmtMb(p.downloaded)}
                        {p.total > 0 ? ` / ${fmtMb(p.total)}` : ""} · {fmtSpeed(p.speedBps)} · 中断可续传
                    </p>
                </div>
            )}
                {busy && p?.phase === "install" && (
                    <p className="text-[11px] text-[var(--color-text-secondary)]">正在安装（约 1–2 分钟，安装期间进度条可能不会更新，请勿关闭应用）…</p>
                )}
                {busy && p?.phase === "verify" && (
                    <p className="text-[11px] text-[var(--color-text-secondary)]">安装完成，正在等待服务启动（最多 30 秒）…</p>
                )}

                {/* 安装监控输出框（实时展示每步与异常） */}
                {busy && (
                    <div className="flex flex-col gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
                        <p className="text-[10px] tracking-wide text-[var(--color-text-secondary)] uppercase">
                            安装监控 · 已运行 {fmtElapsed(installStartAt)}
                            {installProg?.phase && ` · 阶段：${PHASE_LABELS[installProg.phase] ?? installProg.phase}`}
                        </p>
                        {/* 卡死 / 异常警示 */}
                        {stalled && (
                            <p className="rounded border border-[var(--color-danger)] px-1.5 py-1 text-[11px] font-medium text-[var(--color-danger)]">
                                ⚠️ 已 25 秒未收到下载数据，可能因网络中断或服务器无响应。可继续等待，或检查网络后重试。
                            </p>
                        )}
                        <div
                            ref={logBoxRef}
                            className="max-h-40 overflow-y-auto rounded bg-black/30 px-2 py-1.5 font-mono text-[10px] leading-5"
                        >
                            {logs.length === 0 ? (
                                <span className="text-[var(--color-text-secondary)]">等待安装日志…</span>
                            ) : (
                                logs.map((l, i) => (
                                    <div
                                        key={i}
                                        className={
                                            l.level === "error"
                                                ? "text-[var(--color-danger)]"
                                                : l.level === "warn"
                                                  ? "text-yellow-400"
                                                  : "text-[var(--color-text-secondary)]"
                                        }
                                    >
                                        <span className="mr-1 opacity-70">[{fmtLogTime(l.t)}]</span>
                                        {l.msg}
                                    </div>
                                ))
                            )}
                        </div>
                    </div>
                )}

                <div>
                    <Button variant="primary" disabled={busy} onClick={() => void onInstall()}>
                        {busy ? "安装进行中…" : "一键安装 Ollama（约 900MB）"}
                    </Button>
                </div>
            </div>
        );
    }

    // ---- 阶段 2：已装但服务未跑 → 启动并复检 ----
    if (!status.running) {
        return (
            <div className="flex flex-col gap-2 p-3">
                <p className="text-sm text-[var(--color-text)]">已检测到 Ollama{status.version ? `（${status.version}）` : ""}，但服务未运行</p>
                <div>
                    <Button variant="primary" disabled={busy} onClick={() => void onStartService()}>
                        {busy ? "正在启动…" : "启动服务并复检"}
                    </Button>
                </div>
            </div>
        );
    }

    // ---- 阶段 3：就绪 → 推荐模型一键拉取并配置 ----
    return (
        <div className="flex flex-col gap-2.5 p-3">
            <p className="text-sm text-[var(--color-text)]">
                ✓ Ollama 已就绪{status.version ? `（${status.version}）` : ""}
                {hw?.gpu.name && (
                    <span className="ml-2 text-xs text-[var(--color-text-secondary)]">
                        显卡：{hw.gpu.name}
                        {hw.gpu.vramGb != null ? ` · ${hw.gpu.vramGb.toFixed(0)}GB 显存` : ""}
                    </span>
                )}
            </p>

            {/* L2 运行态（§8.5 服务管理）：ownership 只读展示 + 停止服务（仅 AppOwned） */}
            {runtime?.ownership && (
                <div className="flex items-center gap-2 rounded-md border border-[var(--color-border)] px-2.5 py-2">
                    <div className="min-w-0 flex-1">
                        <p className="text-xs text-[var(--color-text)]">
                            {runtime.ownership.kind === "appOwned" ? (
                                <>
                                    运行中（应用启动 · PID {runtime.ownership.detail.pid}）
                                    <span className="ml-1.5 text-[10px] text-[var(--color-text-secondary)]">
                                        空闲 {KEEP_ALIVE_LABEL} 后模型自动释放内存
                                    </span>
                                </>
                            ) : (
                                <>
                                    运行中（由其他程序启动）
                                    <span className="ml-1.5 text-[10px] text-[var(--color-text-secondary)]">
                                        应用不会停止此服务
                                    </span>
                                </>
                            )}
                        </p>
                    </div>
                    <Button
                        variant="ghost"
                        disabled={stopping || runtime.ownership.kind !== "appOwned"}
                        onClick={() => void onStopService()}
                    >
                        {stopping ? "停止中…" : runtime.ownership.kind === "appOwned" ? "停止服务" : "非应用启动"}
                    </Button>
                </div>
            )}

            {(hw?.recommendations ?? []).map((r) => (
                <div
                    key={r.name}
                    className="flex items-center gap-2 rounded-md border border-[var(--color-border)] px-2.5 py-2"
                >
                    <div className="min-w-0 flex-1">
                        <p className="text-sm text-[var(--color-text)]">
                            {r.name}
                            {r.recommended && <span className="ml-1.5 text-[10px] text-[var(--color-accent)]">推荐</span>}
                        </p>
                        <p className="truncate text-[11px] text-[var(--color-text-secondary)]">{r.note}</p>
                    </div>
                    <Button
                        variant={r.recommended ? "primary" : undefined}
                        disabled={pullBusy}
                        onClick={() => void onPick(r.name)}
                    >
                        {modelInstalled(r.name) ? "已安装 · 使用" : "下载并配置"}
                    </Button>
                </div>
            ))}

            {pullBusy && pullState && (
                <div className="flex flex-col gap-1">
                    <ProgressBar value={pullState.total > 0 ? pullState.completed / pullState.total : 0.05} />
                    <p className="text-[11px] text-[var(--color-text-secondary)]">
                        {pullState.model}：{pullState.status}
                        {pullState.total > 0 ? ` ${fmtMb(pullState.completed)} / ${fmtMb(pullState.total)}` : ""}
                    </p>
                </div>
            )}

            {/* 已装模型管理：大小 + 删除（两击确认防误触）+ 打开存储目录 */}
            {(models.length > 0 || modelsLoading || modelDir) && (
                <div className="flex flex-col gap-1.5 rounded-md border border-[var(--color-border)] px-2.5 py-2">
                    <div className="flex items-center justify-between gap-2">
                        <span className="text-[11px] text-[var(--color-text)]">
                            已安装模型（{models.length}）
                            {models.length > 0 && (
                                <span className="ml-1.5 text-[var(--color-text-secondary)]">
                                    共 {fmtMb(models.reduce((s, m) => s + m.size, 0))}
                                </span>
                            )}
                        </span>
                        <span className="flex shrink-0 items-center gap-1.5">
                            {modelDir && (
                                <Button variant="ghost" disabled={!!deletingModel} onClick={() => void onOpenModelDir()}>
                                    打开模型目录
                                </Button>
                            )}
                            <Button variant="ghost" disabled={modelsLoading || !!deletingModel} onClick={() => void loadModels()}>
                                {modelsLoading ? "刷新中…" : "刷新"}
                            </Button>
                        </span>
                    </div>
                    {models.map((m) => (
                        <div key={m.name} className="flex items-center gap-2">
                            <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--color-text)]" title={m.name}>
                                {m.name}
                            </span>
                            <span className="shrink-0 text-[10px] text-[var(--color-text-secondary)]">{fmtMb(m.size)}</span>
                            <Button
                                variant={confirmingDelete === m.name ? "danger" : "ghost"}
                                disabled={!!deletingModel}
                                onClick={() => void onDeleteModel(m.name)}
                            >
                                {deletingModel === m.name ? "删除中…" : confirmingDelete === m.name ? "确认删除？" : "删除"}
                            </Button>
                        </div>
                    ))}
                    {models.length === 0 && !modelsLoading && (
                        <p className="text-[11px] text-[var(--color-text-secondary)]">
                            暂无已安装模型，可使用上方向导下载视觉模型
                        </p>
                    )}
                </div>
            )}

            {/* 手动输入任意官方库模型名拉取（可选模型远不止上方推荐） */}
            <div className="flex items-center gap-2 rounded-md border border-dashed border-[var(--color-border)] px-2.5 py-2">
                <span className="shrink-0 text-[11px] text-[var(--color-text)]">自定义模型</span>
                <input
                    value={manualModel}
                    onChange={(e) => setManualModel(e.target.value)}
                    onKeyDown={(e) => {
                        if (e.key === "Enter" && manualModel.trim()) void onPick(manualModel.trim());
                    }}
                    placeholder="输入官方库模型名，如 qwen3.5:4b、qwen3.5:2b（按回车下载）"
                    className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs outline-none focus:border-[var(--color-accent)]"
                />
                <Button
                    variant="ghost"
                    disabled={pullBusy || !manualModel.trim()}
                    onClick={() => void onPick(manualModel.trim())}
                >
                    下载
                </Button>
            </div>

            <p className="text-[11px] leading-4 text-[var(--color-text-secondary)]">
                模型文件约 1.0–6.6GB（取决于模型规格），默认保存在用户目录的 .ollama 文件夹。可在上方「已安装模型」中删除不需要的模型以释放磁盘空间。下载较慢时，可配置下方代理，或使用离线导入。设置项修改后会自动保存。
            </p>

            {/* 模型下载代理（加速项 A：拉起 serve 时注入 HTTPS_PROXY） */}
            <div className="flex flex-col gap-1 rounded-md border border-[var(--color-border)] px-2.5 py-2">
                <div className="flex items-center gap-2">
                    <span className="shrink-0 text-[11px] text-[var(--color-text)]">模型下载代理</span>
                    <input
                        value={draft.modelDownloadProxy ?? ""}
                        onChange={(e) => onPatchSettings({ modelDownloadProxy: e.target.value.trim() })}
                        placeholder="如 http://127.0.0.1:7890（留空表示直连）"
                        className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs outline-none focus:border-[var(--color-accent)]"
                    />
                </div>
                <p className="text-[11px] leading-4 text-[var(--color-text-secondary)]">
                    下载模型较慢时，可填写本机代理地址。修改后需重新启动服务；若 Ollama 已在运行，请先重启 Ollama。
                </p>
            </div>

            {/* 离线导入引导（加速项 D：魔搭下 GGUF → Modelfile → ollama create，绕开海外网络） */}
            <div className="flex flex-col gap-1.5 rounded-md border border-[var(--color-border)] px-2.5 py-2">
                <button
                    onClick={() => setShowOffline((v) => !v)}
                    className="self-start text-[11px] text-[var(--color-accent)] hover:underline"
                >
                    {showOffline ? "收起" : "离线导入模型（魔搭）"} {showOffline ? "▴" : "▾"}
                </button>
                {showOffline && (
                    <ol className="list-decimal space-y-1 pl-4 text-[11px] leading-4 text-[var(--color-text-secondary)]">
                        <li>
                            浏览器打开魔搭 modelscope.cn，搜「你的模型名 + GGUF」（如 Qwen3.5-4B-GGUF），下载 .gguf 文件。
                        </li>
                        <li>在 .gguf 同目录新建文件 <code className="rounded bg-[var(--color-surface)] px-1">Modelfile</code>，内容一行：<code className="rounded bg-[var(--color-surface)] px-1">FROM ./你的模型.gguf</code></li>
                        <li>
                            在该目录打开终端执行：<code className="rounded bg-[var(--color-surface)] px-1">ollama create 我的模型 -f ./Modelfile</code>
                        </li>
                        <li>返回本页点击「已安装 · 使用」，或从下拉列表选择模型即可。适用于无法直接下载模型的网络环境。</li>
                    </ol>
                )}
            </div>
        </div>
    );
}

/** db::CustomSource → TS CustomSource 映射（用于 draft 回填） */
function toCustom(src: DownloadSource): CustomSource {
    return { id: src.id, label: src.label, url: src.url };
}

const PHASE_LABELS: Record<string, string> = {
    download: "下载",
    install: "安装",
    verify: "就绪复检",
    done: "完成",
};

/** 阶段/安装已运行时长（mm:ss） */
function fmtElapsed(startAt: number): string {
    if (!startAt) return "—";
    const s = Math.max(0, Math.floor((Date.now() - startAt) / 1000));
    const m = Math.floor(s / 60);
    return `${String(m).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}

/** 日志行时间戳（HH:MM:SS） */
function fmtLogTime(t: number): string {
    const d = new Date(t);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
