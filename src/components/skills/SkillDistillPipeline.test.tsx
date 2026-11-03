import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, screen } from "@testing-library/react";
import { actRender } from "@/test/render";
import SkillDistillPipeline from "./SkillDistillPipeline";

afterEach(cleanup);

describe("SkillDistillPipeline", () => {
  it("走完录制→蒸馏→校验→审阅→入库并回调草稿", async () => {
    const onPublish = vi.fn();
    await actRender(<SkillDistillPipeline onPublish={onPublish} />);

    fireEvent.click(screen.getByLabelText("包含步骤 复制导出"));
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));

    fireEvent.change(screen.getByLabelText("技能名称"), { target: { value: "weekly-export" } });
    fireEvent.change(screen.getByLabelText("技能说明"), { target: { value: "每周导出素材到交付目录" } });
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));

    expect(screen.getByText(/封闭性校验通过/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText("步骤")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "去入库" }));
    fireEvent.click(screen.getByRole("button", { name: "加入本地技能库" }));

    expect(onPublish).toHaveBeenCalledTimes(1);
    expect(onPublish.mock.calls[0][0].steps[0].capability).toBe("exports.submit");
    expect(screen.getByText(/已加入本地技能库草稿/)).toBeTruthy();
  });

  it("未勾选步骤时下一步禁用", async () => {
    await actRender(<SkillDistillPipeline />);
    const next = screen.getByRole("button", { name: "下一步" }) as HTMLButtonElement;
    expect(next.disabled).toBe(true);
  });
});
