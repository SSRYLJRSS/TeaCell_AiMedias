import { describe, expect, it } from "vitest";
import { classifyEndpoint, deploymentMatchesEndpoint } from "./endpointScope";

describe("classifyEndpoint", () => {
  it("把 localhost 与 127.0.0.0/8 判为本机", () => {
    expect(classifyEndpoint("http://localhost:11434/v1")).toBe("loopback");
    expect(classifyEndpoint("http://127.0.0.1:3425")).toBe("loopback");
    expect(classifyEndpoint("http://127.9.9.9:8080/v1")).toBe("loopback");
    expect(classifyEndpoint("https://LocalHost/v1")).toBe("loopback");
    expect(classifyEndpoint("http://[::1]:8080/v1")).toBe("loopback");
  });

  it("把其余合法地址判为远端", () => {
    expect(classifyEndpoint("https://api.example.com/v1")).toBe("remote");
    expect(classifyEndpoint("http://192.168.1.10:11434/v1")).toBe("remote");
    // 只是域名里含 localhost 字样的公网主机不能被当成回环。
    expect(classifyEndpoint("https://localhost.example.com/v1")).toBe("remote");
    // 128.0.0.1 不属于 127.0.0.0/8。
    expect(classifyEndpoint("http://128.0.0.1:1/v1")).toBe("remote");
  });

  it("拒绝空值、非 http 协议、带凭据或查询串的地址", () => {
    expect(classifyEndpoint("")).toBe("invalid");
    expect(classifyEndpoint("   ")).toBe("invalid");
    expect(classifyEndpoint(null)).toBe("invalid");
    expect(classifyEndpoint(undefined)).toBe("invalid");
    expect(classifyEndpoint("file:///etc/passwd")).toBe("invalid");
    expect(classifyEndpoint("ftp://example.com/v1")).toBe("invalid");
    expect(classifyEndpoint("not-a-url")).toBe("invalid");
    expect(classifyEndpoint("https://user:secret@api.example.com/v1")).toBe("invalid");
    expect(classifyEndpoint("https://api.example.com/v1?key=1")).toBe("invalid");
  });
});

describe("deploymentMatchesEndpoint", () => {
  it("声明与地址一致时返回 true", () => {
    expect(deploymentMatchesEndpoint("local", "http://127.0.0.1:11434/v1")).toBe(true);
    expect(deploymentMatchesEndpoint("cloud", "https://api.example.com/v1")).toBe(true);
  });

  it("声明与地址矛盾时返回 false（不得把声明当地址事实）", () => {
    // 声明本机却指向公网：不能输出「不会离开这台电脑」。
    expect(deploymentMatchesEndpoint("local", "https://api.example.com/v1")).toBe(false);
    // 声明在线却指向回环。
    expect(deploymentMatchesEndpoint("cloud", "http://127.0.0.1:11434/v1")).toBe(false);
    // 地址本身不可判定，一律视为不一致。
    expect(deploymentMatchesEndpoint("local", "")).toBe(false);
    expect(deploymentMatchesEndpoint("cloud", "nope")).toBe(false);
  });
});
