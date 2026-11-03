//! A3-03: the Pi tool surface derived from the frozen capability contract.
//!
//! The Host never receives the GUI-only entries (`plans.approve`,
//! `operations.resume`): tool names are derived from the same
//! `CAPABILITY_DESCRIPTORS` table that gates every GUI/Pi/MCP entry, so an
//! approval interface cannot be registered by editing a second list.
//!
//! Tool names are stable ASCII (`tea_` + the capability id with `.` replaced by
//! `_`). The Host declares exactly these names back in the `ready` handshake and
//! must not invent, rename or drop entries; the Rust side rejects any mismatch.

use std::sync::OnceLock;

use super::agent::{
    AccessKind, ApprovalRequirement, CallerKind, CapabilityId, CAPABILITY_DESCRIPTORS,
};

/// Prefix reserved for the application's own Pi tools.
pub const PI_TOOL_PREFIX: &str = "tea_";
/// Longest tool name accepted by the Host protocol and by provider tool APIs.
pub const MAX_PI_TOOL_NAME_BYTES: usize = 64;

/// One model-visible tool bound to exactly one domain capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiToolDescriptor {
    pub name: String,
    pub capability: CapabilityId,
    pub access: AccessKind,
    pub approval: ApprovalRequirement,
}

/// Stable Pi tool name for a capability. The result is only a naming scheme;
/// it grants nothing on its own.
pub fn pi_tool_name(capability: CapabilityId) -> String {
    format!("{PI_TOOL_PREFIX}{}", capability.as_str().replace('.', "_"))
}

static PI_TOOLS: OnceLock<Vec<PiToolDescriptor>> = OnceLock::new();

/// The complete Pi tool surface, sorted by tool name.
///
/// Every entry comes from a capability whose caller list includes `pi`. The
/// GUI-only approval/resume entries are therefore absent by construction.
pub fn pi_tools() -> &'static [PiToolDescriptor] {
    PI_TOOLS.get_or_init(|| {
        let mut tools: Vec<PiToolDescriptor> = CAPABILITY_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.callers.contains(&CallerKind::Pi))
            .map(|descriptor| PiToolDescriptor {
                name: pi_tool_name(descriptor.capability),
                capability: descriptor.capability,
                access: descriptor.access,
                approval: descriptor.approval,
            })
            .collect();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        tools
    })
}

/// Look up the tool registered for one capability, if Pi may call it.
pub fn pi_tool(capability: CapabilityId) -> Option<&'static PiToolDescriptor> {
    pi_tools().iter().find(|tool| tool.capability == capability)
}

/// Resolve a Host-declared tool name back to its capability.
pub fn pi_capability_for_tool(name: &str) -> Option<CapabilityId> {
    pi_tools()
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.capability)
}

/// The exact tool-name list the Host must declare in its `ready` handshake.
pub fn pi_tool_names() -> Vec<&'static str> {
    pi_tools().iter().map(|tool| tool.name.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_only_approval_and_resume_entries_have_no_pi_tool() {
        assert!(pi_tool(CapabilityId::PlansApprove).is_none());
        assert!(pi_tool(CapabilityId::OperationsResume).is_none());
        assert!(!pi_tool_names().contains(&"tea_plans_approve"));
        assert!(!pi_tool_names().contains(&"tea_operations_resume"));
        for tool in pi_tools() {
            assert_ne!(tool.approval, ApprovalRequirement::TrustedReview);
            assert_ne!(tool.approval, ApprovalRequirement::ResumeReview);
        }
    }

    #[test]
    fn every_pi_callable_capability_has_exactly_one_tool() {
        let expected = CAPABILITY_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.callers.contains(&CallerKind::Pi))
            .count();
        assert_eq!(pi_tools().len(), expected);
        assert_eq!(pi_tools().len(), 18);
        for descriptor in CAPABILITY_DESCRIPTORS {
            let callable = descriptor.callers.contains(&CallerKind::Pi);
            assert_eq!(pi_tool(descriptor.capability).is_some(), callable);
        }
    }

    #[test]
    fn pi_tool_names_are_stable_ascii_and_unique() {
        let mut seen = Vec::new();
        for name in pi_tool_names() {
            assert!(name.starts_with(PI_TOOL_PREFIX));
            assert!(name.len() <= MAX_PI_TOOL_NAME_BYTES);
            assert!(name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
            assert!(!seen.contains(&name), "duplicate tool name {name}");
            seen.push(name);
        }
        assert_eq!(seen.len(), 18);
        assert_eq!(
            pi_tool_name(CapabilityId::AssetsSearch),
            "tea_assets_search"
        );
        assert_eq!(
            pi_tool_name(CapabilityId::AssetsMovePrepare),
            "tea_assets_move_prepare"
        );
        assert_eq!(
            pi_capability_for_tool("tea_imports_prepare"),
            Some(CapabilityId::ImportsPrepare)
        );
        assert_eq!(pi_capability_for_tool("tea_plans_approve"), None);
    }

    #[test]
    fn every_pi_tool_maps_to_the_generated_json_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/contracts/agent-v1.schema.json"))
                .expect("generated agent schema must parse");
        let capabilities = schema["x-capabilities"]
            .as_object()
            .expect("generated schema must expose x-capabilities");
        let defs = schema["$defs"]
            .as_object()
            .expect("generated schema must expose $defs");
        for tool in pi_tools() {
            let id = tool.capability.as_str();
            let entry = capabilities
                .get(id)
                .unwrap_or_else(|| panic!("schema is missing capability {id}"));
            let reference = entry["input"]["$ref"]
                .as_str()
                .unwrap_or_else(|| panic!("capability {id} has no input reference"));
            let definition = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("capability {id} has an unexpected input reference"));
            assert!(
                defs.contains_key(definition),
                "schema $defs is missing {definition} for capability {id}"
            );
        }
    }
}
