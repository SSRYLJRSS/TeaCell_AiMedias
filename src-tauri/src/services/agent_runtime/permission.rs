//! A3-10: Agent 会话权限档位。
//!
//! 档位回答「这次对话里模型能用哪一层工具」，**不回答**「数据能不能离开本机」——
//! 后者由网关准入（[`super::gateway`]）判定，两者正交：`Read` 档唯一能做的
//! `assets.search` 本身就是外发。因此这里不出现任何联网语义。
//!
//! 档位只是 Host 能力可见性的**上限**，只能收窄 [`SUPPORTED_PI_CAPABILITIES`]，
//! 永远不会放宽它；也永远不改变任何 `approval` 语义——`plans.approve`、
//! `operations.resume`、`trash.empty.*` 在任何档位下都保持 GUI 专属。
//!
//! 权威定义处是 `plans/v2/12-implementation-contracts.md` §6.2，本文件只实现它。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::capabilities::SUPPORTED_PI_CAPABILITIES;
use crate::contracts::agent::{AccessKind, CapabilityId};
use crate::contracts::caller::capability_descriptor;
use crate::contracts::tools::pi_tool;

/// 会话权限档位。默认 `Standard`：第一次用的用户不理解安全模型也能安全用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentPermission {
    /// 仅阅读：只能查询与取消。
    Read,
    /// 可执行改动：除「移动」「删除」两族以外的全部能力。
    #[default]
    Standard,
    /// 完全权限：全部 16 项。仍然全部走 prepare → 用户批准 → submit。
    Full,
}

/// `Standard` 排除的能力：会改变既有素材在库内位置的两族。
///
/// 排除它们是刻意的默认安全态：打标只改数据库记录，导出只写出副本，恢复只把
/// 素材从回收站放回，而移动与删除会挪动/移除用户原有的文件。该集合由
/// [`tests::tier_tool_sets_are_exact`] 逐名固定，新增能力必须显式回答属于哪一档。
const STANDARD_EXCLUDED: &[CapabilityId] = &[
    CapabilityId::AssetsMovePrepare,
    CapabilityId::AssetsMoveSubmit,
    CapabilityId::AssetsTrashPrepare,
    CapabilityId::AssetsTrashSubmit,
];

impl AgentPermission {
    /// 线上形态：`conversation.open` 帧的 `permission` 字段取值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    /// 解析线上形态；未知取值返回 `None`，调用方必须拒绝而不是回落默认档。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "standard" => Some(Self::Standard),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// 本档位是否放行这一项能力。这是安全门与工具裁剪**共用的唯一判据**。
    ///
    /// 三重收紧，任一不满足即拒绝：
    /// 1. 必须仍是 Host 可调能力（本档位不能放宽 [`SUPPORTED_PI_CAPABILITIES`]）；
    /// 2. 必须在能力描述表里有登记（缺项**不放行**，与
    ///    [`super::capabilities`] 的维护门同一手法）；
    /// 3. 必须满足本档位的判据。
    pub fn allows(self, capability: CapabilityId) -> bool {
        if !SUPPORTED_PI_CAPABILITIES.contains(&capability) {
            return false;
        }
        let Some(descriptor) = capability_descriptor(capability) else {
            return false;
        };
        match self {
            Self::Read => matches!(descriptor.access, AccessKind::Query | AccessKind::Cancel),
            Self::Standard => !STANDARD_EXCLUDED.contains(&capability),
            Self::Full => true,
        }
    }

    /// 本档位应暴露给模型的工具名，升序，与 Host `ready` 的排序约定一致。
    pub fn tool_names(self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = SUPPORTED_PI_CAPABILITIES
            .iter()
            .filter(|capability| self.allows(**capability))
            .filter_map(|capability| pi_tool(*capability).map(|tool| tool.name.as_str()))
            .collect();
        names.sort_unstable();
        names
    }

    /// 对「Host 在 `conversation.open` 之后注册的工具集合」做精确校验。
    ///
    /// 只在会话创建路径使用；`ready` 握手仍是注册全集（hello 早于任何会话，
    /// 此时不存在档位）。拒绝重复、多余与缺失。
    pub fn accepts_tool_names(self, tools: &[Value]) -> bool {
        let mut declared: Vec<&str> = Vec::with_capacity(tools.len());
        for value in tools {
            let Some(name) = value.as_str() else {
                return false;
            };
            if declared.contains(&name) {
                return false;
            }
            declared.push(name);
        }
        declared.sort_unstable();
        declared == self.tool_names()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(permission: AgentPermission) -> Vec<&'static str> {
        permission.tool_names()
    }

    #[test]
    fn tier_tool_sets_are_exact() {
        assert_eq!(
            names(AgentPermission::Read),
            vec![
                "tea_assets_search",
                "tea_operations_cancel",
                "tea_operations_get",
                "tea_workflows_load",
            ]
        );
        assert_eq!(
            names(AgentPermission::Standard),
            vec![
                "tea_assets_restore_prepare",
                "tea_assets_restore_submit",
                "tea_assets_search",
                "tea_exports_prepare",
                "tea_exports_submit",
                "tea_operations_cancel",
                "tea_operations_get",
                "tea_tagging_prepare",
                "tea_tagging_submit",
                "tea_workflows_load",
                "tea_workflows_prepare",
                "tea_workflows_submit",
            ]
        );
        assert_eq!(names(AgentPermission::Full).len(), 16);
    }

    #[test]
    fn tiers_are_strictly_monotonic_subsets() {
        let read = names(AgentPermission::Read);
        let standard = names(AgentPermission::Standard);
        let full = names(AgentPermission::Full);
        assert!(read.len() < standard.len() && standard.len() < full.len());
        assert!(read.iter().all(|name| standard.contains(name)));
        assert!(standard.iter().all(|name| full.contains(name)));
    }

    /// 死档防护：被暴露的每个 `*.prepare` 必须同时暴露其 `*.submit`。
    #[test]
    fn every_exposed_prepare_has_its_submit() {
        for permission in [
            AgentPermission::Read,
            AgentPermission::Standard,
            AgentPermission::Full,
        ] {
            let exposed = permission.tool_names();
            for name in &exposed {
                let Some(stem) = name.strip_suffix("_prepare") else {
                    continue;
                };
                let submit = format!("{stem}_submit");
                assert!(
                    exposed.contains(&submit.as_str()),
                    "{} 档暴露了 {name} 却没有 {submit}",
                    permission.as_str()
                );
            }
        }
    }

    #[test]
    fn read_tier_rejects_writes_and_keeps_queries() {
        for capability in SUPPORTED_PI_CAPABILITIES {
            let expected = matches!(
                capability_descriptor(*capability).map(|descriptor| descriptor.access),
                Some(AccessKind::Query) | Some(AccessKind::Cancel)
            );
            assert_eq!(
                AgentPermission::Read.allows(*capability),
                expected,
                "Read 档对 {} 的判定与描述表不一致",
                capability.as_str()
            );
        }
    }

    #[test]
    fn standard_excludes_only_move_and_trash() {
        assert!(!AgentPermission::Standard.allows(CapabilityId::AssetsMovePrepare));
        assert!(!AgentPermission::Standard.allows(CapabilityId::AssetsMoveSubmit));
        assert!(!AgentPermission::Standard.allows(CapabilityId::AssetsTrashPrepare));
        assert!(!AgentPermission::Standard.allows(CapabilityId::AssetsTrashSubmit));
        assert!(AgentPermission::Full.allows(CapabilityId::AssetsTrashSubmit));
        assert!(AgentPermission::Standard.allows(CapabilityId::TaggingSubmit));
        assert!(AgentPermission::Standard.allows(CapabilityId::ExportsSubmit));
    }

    /// 档位不能放宽 Host 可调集合，也不能把 GUI-only 入口放进来。
    #[test]
    fn full_tier_cannot_widen_the_host_surface() {
        for capability in [
            CapabilityId::PlansApprove,
            CapabilityId::OperationsResume,
            CapabilityId::TrashEmptyPrepare,
            CapabilityId::TrashEmptySubmit,
            CapabilityId::ImportsPrepare,
            CapabilityId::ImportsSubmit,
        ] {
            for permission in [
                AgentPermission::Read,
                AgentPermission::Standard,
                AgentPermission::Full,
            ] {
                assert!(
                    !permission.allows(capability),
                    "{} 档不该放行 {}",
                    permission.as_str(),
                    capability.as_str()
                );
            }
        }
        assert!(!names(AgentPermission::Full).contains(&"tea_plans_approve"));
        assert!(!names(AgentPermission::Full).contains(&"tea_operations_resume"));
    }

    #[test]
    fn wire_round_trips_and_rejects_unknown_values() {
        for permission in [
            AgentPermission::Read,
            AgentPermission::Standard,
            AgentPermission::Full,
        ] {
            assert_eq!(
                AgentPermission::parse(permission.as_str()),
                Some(permission)
            );
        }
        assert_eq!(AgentPermission::parse("Full"), None);
        assert_eq!(AgentPermission::parse(""), None);
        assert_eq!(AgentPermission::parse("admin"), None);
        assert_eq!(AgentPermission::default(), AgentPermission::Standard);
    }

    #[test]
    fn accepts_tool_names_rejects_extra_missing_and_duplicate() {
        let standard = AgentPermission::Standard;
        let declared: Vec<Value> = standard
            .tool_names()
            .into_iter()
            .map(|name| Value::String(name.to_string()))
            .collect();
        assert!(standard.accepts_tool_names(&declared));

        let mut extra = declared.clone();
        extra.push(Value::String("tea_assets_trash_submit".to_string()));
        assert!(!standard.accepts_tool_names(&extra));

        let missing = &declared[1..];
        assert!(!standard.accepts_tool_names(missing));

        let mut duplicate = declared.clone();
        duplicate.push(declared[0].clone());
        assert!(!standard.accepts_tool_names(&duplicate));

        let mut wrong_type = declared;
        wrong_type[0] = Value::from(7);
        assert!(!standard.accepts_tool_names(&wrong_type));
    }
}
