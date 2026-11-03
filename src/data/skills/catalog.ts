/**
 * 内置技能目录：10 条知识型 + 7 条可执行型，共 17 条。
 * 来源：artifacts/assistant-redesign/skill-library.md §B。
 */
import { EXECUTABLE_SKILLS } from "./executable";
import { KNOWLEDGE_SKILLS } from "./knowledge";
import { buildSkillIndex } from "./schema";
import type { SkillDefinition, SkillSummary } from "./types";

export const BUILTIN_SKILLS: SkillDefinition[] = [
  ...KNOWLEDGE_SKILLS,
  ...EXECUTABLE_SKILLS,
];

export const BUILTIN_KNOWLEDGE_SKILLS = KNOWLEDGE_SKILLS;
export const BUILTIN_EXECUTABLE_SKILLS = EXECUTABLE_SKILLS;

/** 按 id 取内置技能。 */
export function getBuiltinSkill(id: string): SkillDefinition | undefined {
  return BUILTIN_SKILLS.find((skill) => skill.id === id);
}

/** 内置技能 id 列表。 */
export function listBuiltinSkillIds(): string[] {
  return BUILTIN_SKILLS.map((skill) => skill.id);
}

/** L1 索引（开发期超预算会抛错，便于及早发现）。 */
export function getBuiltinSkillIndex(): SkillSummary[] {
  return buildSkillIndex(BUILTIN_SKILLS);
}
