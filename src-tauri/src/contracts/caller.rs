//! Host-only context. Adapters construct this after authentication, never from tool input.
use super::agent::{
    AgentErrorCode, CallerKind, CapabilityDescriptor, CapabilityId, LibraryScope, ResourceRef,
    CAPABILITY_DESCRIPTORS,
};

/// Deliberately has no serde implementation. This cannot be a Tauri/MCP input record.
///
/// ```compile_fail
/// use teacell_ai_media_manager_lib::contracts::caller::CallerContext;
/// let _: CallerContext = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct CallerContext {
    pub kind: CallerKind,
    pub principal_id: ResourceRef,
    pub session_id: Option<ResourceRef>,
    pub instance_id: ResourceRef,
    pub request_id: ResourceRef,
    pub grant_epoch: ResourceRef,
    pub library: LibraryScope,
    pub origin_chain: Vec<ResourceRef>,
}

/// The frozen descriptor for a capability, when the table has one.
///
/// Callers that must decide something *before* handing work to the domain layer
/// (for example "is this a write, therefore blocked during maintenance") should
/// read `access` from here rather than hard-code a list of write capabilities:
/// a hand-written list silently stops covering the next capability added.
pub fn capability_descriptor(capability: CapabilityId) -> Option<&'static CapabilityDescriptor> {
    CAPABILITY_DESCRIPTORS
        .iter()
        .find(|value| value.capability == capability)
}

/// Entry visibility only. Resource ownership, grants and approval need separate checks.
pub fn check_capability_entry(
    capability: CapabilityId,
    caller: &CallerContext,
) -> Result<(), AgentErrorCode> {
    let descriptor = CAPABILITY_DESCRIPTORS
        .iter()
        .find(|value| value.capability == capability)
        .ok_or(AgentErrorCode::UnsupportedCapability)?;
    if !descriptor.callers.contains(&caller.kind) {
        return Err(AgentErrorCode::Unauthorized);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_and_mcp_cannot_use_gui_approval_or_resume_entries() {
        let token: ResourceRef = serde_json::from_str("\"resource_0123456789abcdef\"").unwrap();
        let mut context = CallerContext {
            kind: CallerKind::Pi,
            principal_id: token.clone(),
            session_id: Some(token.clone()),
            instance_id: token.clone(),
            request_id: token.clone(),
            grant_epoch: token.clone(),
            library: LibraryScope {
                library_id: token.clone(),
                library_epoch: token,
            },
            origin_chain: Vec::new(),
        };
        for kind in [CallerKind::Pi, CallerKind::Mcp] {
            context.kind = kind;
            for capability in [CapabilityId::PlansApprove, CapabilityId::OperationsResume] {
                assert_eq!(
                    check_capability_entry(capability, &context),
                    Err(AgentErrorCode::Unauthorized)
                );
            }
            assert!(check_capability_entry(CapabilityId::ImportsPrepare, &context).is_ok());
        }
        context.kind = CallerKind::Gui;
        assert!(check_capability_entry(CapabilityId::PlansApprove, &context).is_ok());
    }
}
