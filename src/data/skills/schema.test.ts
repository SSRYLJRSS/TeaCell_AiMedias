import { describe, expect, it } from "vitest";
import {
  BUILTIN_EXECUTABLE_SKILLS,
  BUILTIN_KNOWLEDGE_SKILLS,
  BUILTIN_SKILLS,
  getBuiltinSkillIndex,
  listBuiltinSkillIds,
} from "./catalog";
import {
  compileSkillToWorkflowDraft,
  estimateSkillIndexBytes,
  injectionText,
  MAX_SKILL_BODY_BYTES,
  MAX_SKILL_INDEX_BYTES,
  serializeSkillIndex,
  toSummary,
  utf8Bytes,
  validateSkillDefinition,
} from "./schema";
import type { SkillDefinition } from "./types";

describe("内置技能库", () => {
  it("共 17 条（10 知识 + 7 可执行）且 id 唯一", () => {
    expect(BUILTIN_SKILLS).toHaveLength(17);
    expect(BUILTIN_KNOWLEDGE_SKILLS).toHaveLength(10);
    expect(BUILTIN_EXECUTABLE_SKILLS).toHaveLength(7);
    const ids = listBuiltinSkillIds();
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("每条技能都通过 skill/v1 校验", () => {
    for (const skill of BUILTIN_SKILLS) {
      const result = validateSkillDefinition(skill);
      expect(result.errors, `${skill.id}: ${result.errors.join("; ")}`).toEqual([]);
      expect(result.valid, skill.id).toBe(true);
    }
  });

  it("L1 紧凑索引不超过 4KB 预算", () => {
    const bytes = estimateSkillIndexBytes(BUILTIN_SKILLS);
    expect(bytes).toBeLessThanOrEqual(MAX_SKILL_INDEX_BYTES);
    expect(serializeSkillIndex(BUILTIN_SKILLS).length).toBeGreaterThan(0);
    expect(getBuiltinSkillIndex()).toHaveLength(17);
  });

  it("每条正文不超过 4096 字节", () => {
    for (const skill of BUILTIN_SKILLS) {
      expect(utf8Bytes(skill.body), skill.id).toBeLessThanOrEqual(MAX_SKILL_BODY_BYTES);
    }
  });

  it("toSummary 只保留 L1 概要字段", () => {
    const summary = toSummary(BUILTIN_SKILLS[0]);
    expect(Object.keys(summary).sort()).toEqual(
      ["appliesTo", "category", "description", "id", "kind", "name", "status", "whenToUse"].sort(),
    );
  });
});

describe("compileSkillToWorkflowDraft", () => {
  it("可执行技能编译出 WorkflowDraft（步骤 + 参数）", () => {
    const skill = BUILTIN_SKILLS.find((item) => item.id === "batch-tag-plan");
    expect(skill).toBeDefined();
    const result = compileSkillToWorkflowDraft(skill as SkillDefinition);
    expect(result.ok).toBe(true);
    expect(result.draft?.steps).toEqual([{ capability: "tagging.submit", dependsOn: null }]);
    expect(result.draft?.parameters.map((param) => param.key).sort()).toEqual(
      ["selectionRef", "serviceRef", "taggingInstructions"].sort(),
    );
  });

  it("clean-trash 两步且永久清空依赖第一步", () => {
    const skill = BUILTIN_SKILLS.find((item) => item.id === "clean-trash");
    const result = compileSkillToWorkflowDraft(skill as SkillDefinition);
    expect(result.ok).toBe(true);
    expect(result.draft?.steps).toEqual([
      { capability: "assets.trash.submit", dependsOn: null },
      { capability: "trash.empty.submit", dependsOn: 0 },
    ]);
  });

  it("知识型技能不编译为流程", () => {
    const skill = BUILTIN_SKILLS.find((item) => item.id === "semantic-search");
    const result = compileSkillToWorkflowDraft(skill as SkillDefinition);
    expect(result.ok).toBe(false);
    expect(result.draft).toBeNull();
    expect(result.errors.join(" ")).toContain("知识型技能");
  });

  it("界面引导型技能给出排除原因而不生成流程", () => {
    const skill = BUILTIN_SKILLS.find((item) => item.id === "workflow-assemble");
    const result = compileSkillToWorkflowDraft(skill as SkillDefinition);
    expect(result.ok).toBe(false);
    expect(result.draft).toBeNull();
    expect(result.excluded.length).toBeGreaterThan(0);
  });
});

describe("validateSkillDefinition 封闭性", () => {
  it("拒绝不在 STEP_CATALOG 白名单的能力", () => {
    const source = BUILTIN_EXECUTABLE_SKILLS[0];
    const bad: SkillDefinition = {
      ...source,
      params: [],
      steps: [{ id: "s1", title: "非法步骤", capability: "plans.approve" }],
    };
    const result = validateSkillDefinition(bad);
    expect(result.valid).toBe(false);
    expect(result.errors.join(" ")).toContain("不在常用流程步骤白名单");
  });

  it("拒绝持有 planId / receipt 等禁止字段", () => {
    const source = BUILTIN_SKILLS[0];
    const bad = { ...source, planId: "plan-123", receipt: "r1" } as unknown as SkillDefinition;
    const result = validateSkillDefinition(bad);
    expect(result.valid).toBe(false);
    expect(result.errors.some((error) => error.includes("planId"))).toBe(true);
    expect(result.errors.some((error) => error.includes("receipt"))).toBe(true);
  });

  it("拒绝超长正文", () => {
    const source = BUILTIN_SKILLS[0];
    const bad: SkillDefinition = { ...source, body: "x".repeat(MAX_SKILL_BODY_BYTES + 1) };
    const result = validateSkillDefinition(bad);
    expect(result.valid).toBe(false);
    expect(result.errors.join(" ")).toContain("body");
  });
});

describe("injectionText", () => {
  it("包含技能名、正文与批准边界提醒", () => {
    const skill = BUILTIN_SKILLS.find((item) => item.id === "semantic-search") as SkillDefinition;
    const text = injectionText(skill);
    expect(text).toContain("语义搜索");
    expect(text).toContain(skill.body.trim());
    expect(text).toContain("批准");
  });
});
