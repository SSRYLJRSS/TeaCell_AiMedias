import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import RenameBuilder from "@/components/import/RenameBuilder";

vi.mock("@/api/import", () => ({
  renderNamePreview: vi.fn((template: string, collection: string, stem: string) =>
    Promise.resolve(`${collection}_${stem}_${template}`),
  ),
}));

function ControlledBuilder({ initial = "" }: { initial?: string }) {
  const [value, setValue] = useState(initial);
  return (
    <RenameBuilder
      value={value}
      onChange={setValue}
      collection="旅行"
      sampleStem="IMG_001"
      sampleExt="jpg"
    />
  );
}

describe("RenameBuilder 受控同步", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("父级模板换成另一个非空模式时，按钮态、序号位数与预览同步更新", async () => {
    render(<ControlledBuilder initial="{分库}" />);
    expect(screen.getByText("{分库}")).toBeInTheDocument();
    expect(screen.queryByTitle("序号位数（1-9）")).toBeNull();

    fireEvent.click(screen.getByTitle("{日期}"));
    fireEvent.click(screen.getByTitle("{序号:3}"));
    const digits = screen.getByTitle("序号位数（1-9）");
    fireEvent.change(digits, { target: { value: "5" } });

    expect(screen.getByText("{分库}_{日期}_{序号:5}")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByText("预览：旅行_IMG_001_{分库}_{日期}_{序号:5}.jpg")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByTitle("{分库}"));
    expect(screen.getByText("{日期}_{序号:5}")).toBeInTheDocument();
    expect(screen.getByTitle("序号位数（1-9）")).toHaveValue("5");
    // 最后一次改模板会再发一次预览请求；不等它在测试内落地，setPreview 就发生在测试之外（act 警告）。
    await waitFor(() => {
      expect(screen.getByText("预览：旅行_IMG_001_{日期}_{序号:5}.jpg")).toBeInTheDocument();
    });
  });

  it("父级清空时立即移除选中模板、位数输入和预览", async () => {
    render(<ControlledBuilder initial="{原名}_{序号:4}" />);
    expect(screen.getByText("{原名}_{序号:4}")).toBeInTheDocument();
    expect(screen.getByTitle("序号位数（1-9）")).toHaveValue("4");

    fireEvent.click(screen.getByRole("button", { name: "清空" }));
    expect(screen.queryByText("{原名}_{序号:4}")).toBeNull();
    expect(screen.queryByTitle("序号位数（1-9）")).toBeNull();
    expect(screen.queryByText(/预览：/)).toBeNull();
  });
});
