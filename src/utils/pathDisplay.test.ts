import { beforeEach, describe, expect, it } from "vitest";
import { displayBasename } from "@/utils/pathDisplay";
import { usePlatformStore } from "@/stores/platformStore";

beforeEach(() => {
  usePlatformStore.setState({ status: "idle", capabilities: null, error: null, loadGeneration: 0 });
});

describe("displayBasename", () => {
  it("treats both separators as Windows separators", () => {
    expect(displayBasename("C:\\Users\\me\\photo.jpg", "windows")).toBe("photo.jpg");
    expect(displayBasename("C:/Users/me/photo.jpg", "windows")).toBe("photo.jpg");
  });

  it("preserves a Unix filename containing a backslash", () => {
    expect(displayBasename("/home/a\\b.jpg", "linux")).toBe("a\\b.jpg");
  });

  it("keeps the original value for a trailing separator", () => {
    expect(displayBasename("/home/me/", "linux")).toBe("/home/me/");
    expect(displayBasename("", "linux")).toBe("");
  });
});
