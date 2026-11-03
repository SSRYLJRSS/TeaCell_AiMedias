import { act, render } from "@testing-library/react";
import type { ReactElement } from "react";

/**
 * 把一次渲染连同它触发的 promise 一起收进 `act()`，返回与 `render` 相同的工具集。
 *
 * 为什么需要它：很多组件在挂载后会用 promise 补数据（封面 `getPreviewUrl`、
 * zustand 的 `refresh()`、`listAiConnections()` 等）。这些 `.then(() => setState(...))`
 * 在测试的同步 `render()` 之后才落地，React 就会打印
 * 「An update to X inside a test was not wrapped in act(...)」——断言其实是对的，
 * 但输出被噪声淹没，真出问题时反而看不见。
 *
 * 它不是「静音」：不做任何 mock、不屏蔽 console、不改断言，只是把渲染和它引发的
 * 微任务放进 act 边界内，让 React 按真实顺序处理这些更新。测试数、断言与失败方式
 * 都保持不变（用例函数需要写成 async 才能 await 它）。
 */
export async function actRender(ui: ReactElement) {
  let result!: ReturnType<typeof render>;
  await act(async () => {
    result = render(ui);
  });
  return result;
}
