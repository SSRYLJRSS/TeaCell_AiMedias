/** displayBasename 测试（X-18）：分隔符按平台判定，Unix 反斜杠是合法文件名字符。 */
import { beforeEach, describe, expect, it } from "vitest";
import { displayBasename } from "@/utils/pathDisplay";
import { usePlatformStore } from "@/stores/platformStore";

function setOs(os: "windows" | "macos" | "linux") {
  usePlatformStore.setState({
    status: "ready",
    error: null,
    capabilities: {
      schemaVersion: 1,
      os,
      arch: os === "macos" ? "aarch64" : "x86_64",
      managedOllama: os === "windows",
      nativeWindowControls: os === "macos",
      primaryModifier: os === "macos" ? "meta" : "ctrl",
      libraryTransferVersion: null,
    },
  });
}

beforeEach(() => {
  usePlatformStore.setState({ status: "idle", capabilities: null, error: null });
});

describe("displayBasename（X-18）", () => {
  it("Windows：反斜杠与正斜杠都是分隔符", () => {
    setOs("windows");
    expect(displayBasename("C:\\Users\\me\\photo.jpg")).toBe("photo.jpg");
    expect(displayBasename("C:/Users/me/photo.jpg")).toBe("photo.jpg");
  });

  it("Unix：反斜杠是合法文件名字符，不当分隔符", () => {
    setOs("linux");
    // /home/a\b.jpg 的文件名是 a\b.jpg，不能截成 b.jpg
    expect(displayBasename("/home/a\\b.jpg")).toBe("a\\b.jpg");
    expect(displayBasename("/home/me/photo.jpg")).toBe("photo.jpg");
  });

  it("macOS：只认正斜杠", () => {
    setOs("macos");
    expect(displayBasename("/Users/me/weird\\name.png")).toBe("weird\\name.png");
  });

  it("store 未就绪：按仅 / 处理，不误伤合法文件名", () => {
    // idle：os 未知 → 只用 / 拆
    expect(displayBasename("/home/a\\b.jpg")).toBe("a\\b.jpg");
    expect(displayBasename("/home/me/photo.jpg")).toBe("photo.jpg");
  });

  it("尾部分隔符/空串回退原值", () => {
    setOs("linux");
    expect(displayBasename("/home/me/")).toBe("/home/me/");
    expect(displayBasename("")).toBe("");
  });
});
