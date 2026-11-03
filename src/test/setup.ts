import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";

// 路由改为 React.lazy + Suspense 后，切页要等懒加载 chunk（并行跑测试时 CPU 争用更明显）。
// 默认 1000ms 的 waitFor/findBy 超时会在并行下偶发超时（App.test「跳过引导后手动入口」的
// toolbar-filters）。统一放宽到 5s：只是让真正慢的渲染有足够时间，断言失败仍会失败。
configure({ asyncUtilTimeout: 5000 });
