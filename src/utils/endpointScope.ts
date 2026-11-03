/** 端点范围判定（A3-06）。
 *
 * 为什么需要它：接口档案里的 `deployment` 是**用户声明的字段**，不是地址事实。
 * 后端 `agent_runtime::gateway::Route::endpoint_url` 只校验 http/https、无凭据、
 * 无 query/fragment，并不要求 `deployment=local` 的地址必须是本机。
 * 因此界面上「本机 / 在线」以及「不会离开这台电脑」这类话术不能直接采信
 * `deployment`，必须由地址推出——否则就是把标签当成了地址安全证明
 * （A3-06 停下条件明确点名这种行为）。
 *
 * 本函数只做纯字符串判定，不解析 DNS、不发请求：`localhost` 之外的任何域名
 * 一律按远端处理，宁可说「在线」也不谎称「本机」。
 */

export type EndpointScope = "loopback" | "remote" | "invalid";

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "[::1]", "0:0:0:0:0:0:0:1"]);

/** `127.0.0.0/8` 整段都是本机回环，不只是 `127.0.0.1`。 */
function isLoopbackIPv4(host: string): boolean {
  const parts = host.split(".");
  if (parts.length !== 4) return false;
  const octets = parts.map((part) => (/^\d{1,3}$/.test(part) ? Number(part) : Number.NaN));
  if (octets.some((value) => !Number.isInteger(value) || value < 0 || value > 255)) return false;
  return octets[0] === 127;
}

/**
 * 由接口地址判定端点范围。
 * - `loopback`：`localhost`、`127.0.0.0/8`、`::1`（含方括号写法）；
 * - `remote`：其余合法 http/https 地址；
 * - `invalid`：空串、非 http/https、缺主机名、带凭据或 query/fragment。
 */
export function classifyEndpoint(baseUrl: string | null | undefined): EndpointScope {
  const raw = typeof baseUrl === "string" ? baseUrl.trim() : "";
  if (!raw) return "invalid";
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return "invalid";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "invalid";
  // 与后端 `endpoint_url` 保持一致：带凭据或查询串的地址本身就不被接受。
  if (url.username || url.password || url.search || url.hash) return "invalid";
  const host = url.hostname.toLowerCase();
  if (!host) return "invalid";
  const bare = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
  if (LOOPBACK_HOSTS.has(host) || LOOPBACK_HOSTS.has(bare)) return "loopback";
  if (isLoopbackIPv4(bare)) return "loopback";
  return "remote";
}

/** 声明的部署方式与地址事实是否矛盾（矛盾时必须提示，不能静默沿用声明）。 */
export function deploymentMatchesEndpoint(
  deployment: "cloud" | "local",
  baseUrl: string | null | undefined,
): boolean {
  const scope = classifyEndpoint(baseUrl);
  if (scope === "invalid") return false;
  return deployment === "local" ? scope === "loopback" : scope === "remote";
}
