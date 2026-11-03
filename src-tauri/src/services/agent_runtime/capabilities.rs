//! A3-03: Host capability requests.
//!
//! The Host never receives domain authority. Each request names a capability and
//! carries the model's tool arguments; the Rust side builds the caller context
//! from the Host identity and the active run, injects the protocol fields the
//! model is not allowed to choose, and executes through `services/agent_tools`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use super::permission::AgentPermission;
use super::protocol::MAX_CAPABILITY_PAYLOAD_BYTES;
use crate::contracts::agent::AccessKind;
use crate::contracts::agent::{
    decode_capability_input, encode_agent_output, AgentErrorCode, CapabilityId, CapabilityInput,
    ResourceRef,
};
use crate::contracts::caller::capability_descriptor;
use crate::contracts::tools::pi_tool;
use crate::error::AppError;
use crate::services::agent_tools;
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::library_migration::LibraryMaintenanceGate;
use crate::state::Database;

/// Single trusted Agent principal. The Host can never select or override it.
pub const PI_HOST_PRINCIPAL: &str = "principal_pi_host";

/// Capabilities the Host may call today. Adding one requires a matching Node
/// registration and a schema/handshake change, enforced below.
///
/// `imports.prepare` / `imports.submit` are deliberately absent. After A3-03 the
/// three original blockers stand as follows (see `docs/contracts/agent-v1.md`
/// §「各类入口行为」and `docs/contracts/authorization-v1.md` §3):
/// - **Resolved (execution owner):** an accepted import operation is no longer
///   drained only for `principal_gui_host`. `import_dispatch::drain` now rebuilds
///   the caller with `operation_dispatch::execution_caller`, so an import
///   operation created by any persisted origin (gui/pi/mcp) can be drained.
/// - **Resolved (output shape):** `imports.prepare`'s declared output is
///   `PlanPreview`, and `ImportPlanView` is explicitly *not* that type — it is the
///   GUI workbench checklist and carries no `reviewRef`. The reviewable preview is
///   the `imports.submit` draft built by `import_submission::submit_draft`, which
///   is now the single construction site, so no second copy can drift into
///   `PlanChanged`.
/// - **Decided, not pending (source handle):** the contract input only carries an
///   opaque `sourceRef`, and every directory handle is bound to the issuing session
///   (`authorization::validate_handle_mask` requires principal+instance+session
///   equality), so the Agent has no way to obtain a source directory of its own.
///   A5-03 closed this as a scope decision rather than a gap to be filled in 2.0:
///   no import-source handle is issued to Pi or MCP (an MCP caller has no session,
///   so a handle issued for a GUI session can never validate), and the unverified
///   `paths` route must not be used as a substitute. See
///   `docs/contracts/mcp-bridge-v1.md` §4.1.1 and the design conclusion in
///   `docs/contracts/agent-v1.md`. Re-opening it needs a new signed-off handoff
///   design, not another local patch.
///
/// Registering a tool that can only ever fail would be worse than an honest
/// absence: the model can still search, prepare tagging plans and run workflows.
pub const SUPPORTED_PI_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::AssetsSearch,
    CapabilityId::TaggingPrepare,
    CapabilityId::TaggingSubmit,
    CapabilityId::ExportsPrepare,
    CapabilityId::ExportsSubmit,
    CapabilityId::AssetsMovePrepare,
    CapabilityId::AssetsMoveSubmit,
    CapabilityId::AssetsTrashPrepare,
    CapabilityId::AssetsTrashSubmit,
    CapabilityId::AssetsRestorePrepare,
    CapabilityId::AssetsRestoreSubmit,
    CapabilityId::OperationsGet,
    CapabilityId::OperationsCancel,
    CapabilityId::WorkflowsLoad,
    CapabilityId::WorkflowsPrepare,
    CapabilityId::WorkflowsSubmit,
];

/// Exact tool-name list the Host must declare in `ready`.
pub fn supported_tool_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = SUPPORTED_PI_CAPABILITIES
        .iter()
        .filter_map(|capability| pi_tool(*capability).map(|tool| tool.name.as_str()))
        .collect();
    names.sort_unstable();
    names
}

/// True only for the exact supported tool set: no duplicates, no extras, none missing.
pub fn declared_tools_match(tools: &[Value]) -> bool {
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
    declared == supported_tool_names()
}

/// Resolve a wire capability id; anything outside the supported set is rejected.
pub fn capability_from_id(value: &str) -> Option<CapabilityId> {
    let parsed: CapabilityId = serde_json::from_value(Value::String(value.to_string())).ok()?;
    SUPPORTED_PI_CAPABILITIES
        .contains(&parsed)
        .then_some(parsed)
}

/// Execute one capability request and return the encoded result payload.
///
/// `permission` 是 A3-10 的安全门：Host 子进程是外部输入来源，
/// `capability.request` 帧里的 `capabilityId` 不能因为 Node 侧裁剪过就免检。
/// `None` 表示该会话没有档位记录（未打开或已关闭），一律拒绝，不回落更宽档。
// 参数已经排到一个入口函数的自然上限：这里刻意保持扁平的显式参数，而不是引入
// 一个只被调用一次的上下文结构体——后者会把「谁提供了身份、谁提供了档位」
// 藏进字段默认值里，正是这道门要避免的模糊。
#[allow(clippy::too_many_arguments)]
pub fn execute(
    db: &Database,
    maintenance: Option<&LibraryMaintenanceGate>,
    permission: Option<AgentPermission>,
    instance_id: &str,
    conversation_id: &str,
    request_id: &str,
    capability_id: &str,
    payload: &str,
) -> Result<Vec<u8>, AgentErrorCode> {
    let capability =
        capability_from_id(capability_id).ok_or(AgentErrorCode::UnsupportedCapability)?;
    ensure_permission_allows(permission, capability)?;
    let arguments = decode_payload(payload)?;
    let caller = build_caller(db, instance_id, conversation_id, request_id)?;
    dispatch(db, &caller, capability, &arguments, maintenance)
}

/// A3-10 的会话档位门：在执行边界上唯一的档位判定。
///
/// 判据只有一处——[`AgentPermission::allows`]，工具裁剪与安全门共用它，
/// 所以「能看见」与「能执行」不可能分叉。判据本身从能力描述表推导并强制收窄
/// 到 [`SUPPORTED_PI_CAPABILITIES`]，描述表缺项时**不放行**。
fn ensure_permission_allows(
    permission: Option<AgentPermission>,
    capability: CapabilityId,
) -> Result<(), AgentErrorCode> {
    match permission {
        Some(permission) if permission.allows(capability) => Ok(()),
        _ => Err(AgentErrorCode::Unauthorized),
    }
}

/// 迁仓/恢复期间在**执行边界**上唯一的维护门判定（A4-05）。
///
/// 判定放在这里而不是各入口各写一遍，是因为这个函数同时服务 Host（Pi 工具调用）
/// 与 MCP 桥：只在 MCP 桥写检查会漏掉 Pi，只在 GUI 命令写检查会漏掉「排队时未维护、
/// 开始执行时已经在维护」的那笔写入。语义与 GUI 侧一致——查询与取消继续可用，
/// 写入被挡；这不是让外部客户端失去看进度的能力，而是不允许它在总仓位置正在
/// 变化时落一笔写。
///
/// 描述表缺项时**不放行**：未登记的写入不能因为查不到 access 就绕过维护门。
fn ensure_maintenance_allows(
    capability: CapabilityId,
    maintenance: Option<&LibraryMaintenanceGate>,
) -> Result<(), AgentErrorCode> {
    let Some(gate) = maintenance else {
        // 未接线（单元测试直接构造）时视为未维护，与 MCP 桥的既有约定一致。
        return Ok(());
    };
    if !gate.is_active() {
        return Ok(());
    }
    match capability_descriptor(capability).map(|descriptor| descriptor.access) {
        Some(AccessKind::Query) | Some(AccessKind::Cancel) => Ok(()),
        _ => Err(AgentErrorCode::MaintenanceActive),
    }
}

/// Execute an already-authorised capability for any caller kind.
///
/// A5-01 reuses this so the MCP bridge runs through exactly the same domain
/// functions as the Host; only the `CallerContext` differs. Keeping one match
/// arm means a capability cannot be wired for one caller and silently diverge
/// for the other.
pub(crate) fn dispatch(
    db: &Database,
    caller: &crate::contracts::caller::CallerContext,
    capability: CapabilityId,
    arguments: &[u8],
    maintenance: Option<&LibraryMaintenanceGate>,
) -> Result<Vec<u8>, AgentErrorCode> {
    ensure_maintenance_allows(capability, maintenance)?;
    let input = decode_capability_input(capability, arguments)?;
    match input {
        CapabilityInput::AssetsSearch(input) => agent_tools::search_assets(db, caller, &input)
            .map_err(agent_code)
            .and_then(|page| encode_agent_output(&page)),
        CapabilityInput::AssetsTrashPrepare(input) => {
            agent_tools::prepare_trash(db, caller, &input)
                .map_err(agent_code)
                .and_then(|preview| encode_agent_output(&preview))
        }
        CapabilityInput::AssetsTrashSubmit(input) => agent_tools::submit_trash(db, caller, &input)
            .map_err(agent_code)
            .and_then(|result| encode_agent_output(&result)),
        // `imports.prepare` / `imports.submit` fall through to the reject arm: see
        // `SUPPORTED_PI_CAPABILITIES` for the three concrete reasons they have no
        // honest Agent path yet (explicit gap until A4-08).
        CapabilityInput::TaggingPrepare(input) => agent_tools::prepare_tagging(db, caller, &input)
            .map_err(agent_code)
            .and_then(|preview| encode_agent_output(&preview)),
        CapabilityInput::TaggingSubmit(input) => agent_tools::submit_tagging(db, caller, &input)
            .map_err(agent_code)
            .and_then(|result| encode_agent_output(&result)),
        CapabilityInput::ExportsPrepare(input) => agent_tools::prepare_export(db, caller, &input)
            .map_err(agent_code)
            .and_then(|preview| encode_agent_output(&preview)),
        CapabilityInput::ExportsSubmit(input) => agent_tools::submit_export(db, caller, &input)
            .map_err(agent_code)
            .and_then(|result| encode_agent_output(&result)),
        CapabilityInput::AssetsMovePrepare(input) => agent_tools::prepare_move(db, caller, &input)
            .map_err(agent_code)
            .and_then(|preview| encode_agent_output(&preview)),
        CapabilityInput::AssetsMoveSubmit(input) => agent_tools::submit_move(db, caller, &input)
            .map_err(agent_code)
            .and_then(|result| encode_agent_output(&result)),
        CapabilityInput::AssetsRestorePrepare(input) => {
            agent_tools::prepare_restore(db, caller, &input)
                .map_err(agent_code)
                .and_then(|preview| encode_agent_output(&preview))
        }
        CapabilityInput::AssetsRestoreSubmit(input) => {
            agent_tools::submit_restore(db, caller, &input)
                .map_err(agent_code)
                .and_then(|result| encode_agent_output(&result))
        }
        CapabilityInput::OperationsGet(input) => {
            agent_tools::get_operation(db, caller, &input.operation_id)
                .map_err(agent_code)
                .and_then(|snapshot| encode_agent_output(&snapshot))
        }
        CapabilityInput::OperationsCancel(input) => {
            agent_tools::cancel_operation(db, caller, &input.operation_id)
                .map_err(agent_code)
                .and_then(|snapshot| encode_agent_output(&snapshot))
        }
        CapabilityInput::WorkflowsLoad(input) => agent_tools::load_workflow(db, caller, &input)
            .map_err(agent_code)
            .and_then(|summary| encode_agent_output(&summary)),
        CapabilityInput::WorkflowsPrepare(input) => {
            agent_tools::prepare_workflow(db, caller, &input)
                .map_err(agent_code)
                .and_then(|preview| encode_agent_output(&preview))
        }
        CapabilityInput::WorkflowsSubmit(input) => agent_tools::submit_workflow(db, caller, &input)
            .map_err(agent_code)
            .and_then(|result| encode_agent_output(&result)),
        _ => Err(AgentErrorCode::UnsupportedCapability),
    }
}

/// Decode the JSON object the model produced, then add the protocol fields the
/// Host is not allowed to supply (currently only `schemaVersion`).
fn decode_payload(payload: &str) -> Result<Vec<u8>, AgentErrorCode> {
    let bytes = BASE64
        .decode(payload)
        .map_err(|_| AgentErrorCode::InvalidInput)?;
    if bytes.is_empty() || bytes.len() > MAX_CAPABILITY_PAYLOAD_BYTES {
        return Err(AgentErrorCode::InvalidInput);
    }
    let mut value: Value =
        serde_json::from_slice(&bytes).map_err(|_| AgentErrorCode::InvalidInput)?;
    let Some(object) = value.as_object_mut() else {
        return Err(AgentErrorCode::InvalidInput);
    };
    object.insert("schemaVersion".to_string(), json!(1));
    serde_json::to_vec(&value).map_err(|_| AgentErrorCode::Internal)
}

fn build_caller(
    db: &Database,
    instance_id: &str,
    conversation_id: &str,
    request_id: &str,
) -> Result<crate::contracts::caller::CallerContext, AgentErrorCode> {
    let request = ResourceRef::from_trusted(request_id.to_string())?;
    let conn = db.lock().map_err(|_| AgentErrorCode::Internal)?;
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Pi {
            principal_id: PI_HOST_PRINCIPAL.to_string(),
            session_id: conversation_id.to_string(),
            instance_id: instance_id.to_string(),
        },
        request,
        Vec::new(),
    )
    .map_err(agent_code)
}

fn agent_code(error: AppError) -> AgentErrorCode {
    match error.into_agent_error() {
        AppError::Agent(code) => code,
        _ => AgentErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_contract_tools_are_declared_and_approval_entries_stay_hidden() {
        let names = supported_tool_names();
        assert_eq!(
            names,
            vec![
                "tea_assets_move_prepare",
                "tea_assets_move_submit",
                "tea_assets_restore_prepare",
                "tea_assets_restore_submit",
                "tea_assets_search",
                "tea_assets_trash_prepare",
                "tea_assets_trash_submit",
                "tea_exports_prepare",
                "tea_exports_submit",
                "tea_operations_cancel",
                "tea_operations_get",
                "tea_tagging_prepare",
                "tea_tagging_submit",
                "tea_workflows_load",
                "tea_workflows_prepare",
                "tea_workflows_submit"
            ]
        );
        assert!(declared_tools_match(&[
            json!("tea_assets_move_prepare"),
            json!("tea_assets_move_submit"),
            json!("tea_assets_restore_prepare"),
            json!("tea_assets_restore_submit"),
            json!("tea_assets_search"),
            json!("tea_assets_trash_prepare"),
            json!("tea_assets_trash_submit"),
            json!("tea_exports_prepare"),
            json!("tea_exports_submit"),
            json!("tea_operations_cancel"),
            json!("tea_operations_get"),
            json!("tea_tagging_prepare"),
            json!("tea_tagging_submit"),
            json!("tea_workflows_load"),
            json!("tea_workflows_prepare"),
            json!("tea_workflows_submit")
        ]));
        assert!(!declared_tools_match(&[]));
        assert!(!declared_tools_match(&[json!("tea_assets_search")]));
        assert!(!declared_tools_match(&[
            json!("tea_assets_search"),
            json!("tea_assets_search")
        ]));
        let mut with_approval: Vec<serde_json::Value> = supported_tool_names()
            .into_iter()
            .map(serde_json::Value::from)
            .collect();
        with_approval.push(json!("tea_plans_approve"));
        assert!(!declared_tools_match(&with_approval));
        let mut with_resume: Vec<serde_json::Value> = supported_tool_names()
            .into_iter()
            .map(serde_json::Value::from)
            .collect();
        with_resume.push(json!("tea_operations_resume"));
        assert!(!declared_tools_match(&with_resume));
        assert!(!declared_tools_match(&[json!("tea_operations_resume")]));
        assert!(!declared_tools_match(&[json!(7)]));
    }

    #[test]
    fn capability_ids_outside_the_supported_set_are_rejected() {
        assert_eq!(
            capability_from_id("assets.search"),
            Some(CapabilityId::AssetsSearch)
        );
        assert_eq!(
            capability_from_id("operations.get"),
            Some(CapabilityId::OperationsGet)
        );
        assert_eq!(
            capability_from_id("operations.cancel"),
            Some(CapabilityId::OperationsCancel)
        );
        assert_eq!(
            capability_from_id("assets.trash.prepare"),
            Some(CapabilityId::AssetsTrashPrepare)
        );
        assert_eq!(
            capability_from_id("assets.trash.submit"),
            Some(CapabilityId::AssetsTrashSubmit)
        );
        assert_eq!(capability_from_id("trash.empty.prepare"), None);
        assert_eq!(capability_from_id("operations.resume"), None);
        assert_eq!(capability_from_id("plans.approve"), None);
        assert_eq!(capability_from_id("../assets.search"), None);
        assert_eq!(capability_from_id(""), None);
    }

    #[test]
    fn payloads_must_be_bounded_base64_json_objects() {
        let valid = BASE64.encode(br#"{"text":"photo"}"#);
        let decoded = decode_payload(&valid).unwrap();
        let value: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(value["schemaVersion"], json!(1));
        assert_eq!(value["text"], json!("photo"));

        assert_eq!(decode_payload(""), Err(AgentErrorCode::InvalidInput));
        assert_eq!(
            decode_payload("not-base64!!"),
            Err(AgentErrorCode::InvalidInput)
        );
        let array = BASE64.encode(b"[]");
        assert_eq!(decode_payload(&array), Err(AgentErrorCode::InvalidInput));
        let oversized = BASE64.encode(vec![b'a'; MAX_CAPABILITY_PAYLOAD_BYTES + 1]);
        assert_eq!(
            decode_payload(&oversized),
            Err(AgentErrorCode::InvalidInput)
        );
    }

    #[test]
    fn model_supplied_protocol_fields_are_overwritten() {
        let hostile = BASE64.encode(br#"{"schemaVersion":99,"text":"photo"}"#);
        let value: Value = serde_json::from_slice(&decode_payload(&hostile).unwrap()).unwrap();
        assert_eq!(value["schemaVersion"], json!(1));
    }

    #[test]
    fn imports_stay_an_explicit_documented_gap_until_a4_08() {
        // A3-03 第 4 条要求「未实现能力保留显式待验证」，而不是注册一个只会失败的
        // 工具让模型反复试错。理由写在 `SUPPORTED_PI_CAPABILITIES` 的文档注释里：
        // 三条原阻断中，执行主体（drain 改用 execution_caller）与输出形状
        // （ImportPlanView 明确不是协议输出、submit_draft 成为唯一构造点）已修，
        // 剩余一条「来源目录句柄绑定签发会话，模型拿不到自己的源目录」需要交接设计，
        // 与 A5-03 的 MCP 工具表同批落地。在此之前不注册工具。
        assert_eq!(capability_from_id("imports.prepare"), None);
        assert_eq!(capability_from_id("imports.submit"), None);
        assert!(!supported_tool_names().contains(&"tea_imports_prepare"));
        assert!(!supported_tool_names().contains(&"tea_imports_submit"));
        // 其余 Pi 可调能力必须仍然全部在位：缺口不能悄悄扩大。
        assert_eq!(supported_tool_names().len(), 16);
    }

    #[test]
    fn capability_payloads_cannot_forge_approval_or_plan_references() {
        // E03/E07（tests/agent-evals）：模型只能提供契约输入字段，不能自带
        // 批准、审核引用或计划引用；审批入口本身也不是可调用能力。
        let selection = json!({"schemaVersion":1,"selectionRef":"selection_0123456789abcdef"});
        let decoded = decode_capability_input(
            CapabilityId::AssetsTrashPrepare,
            selection.to_string().as_bytes(),
        )
        .expect("the contract input decodes");
        assert!(matches!(decoded, CapabilityInput::AssetsTrashPrepare(_)));

        for forged in [
            json!({"schemaVersion":1,"selectionRef":"selection_0123456789abcdef","approved":true}),
            json!({"schemaVersion":1,"selectionRef":"selection_0123456789abcdef","planId":"plan_0123456789abcdef"}),
            json!({"schemaVersion":1,"selectionRef":"selection_0123456789abcdef","reviewRef":"review_0123456789abcdef"}),
        ] {
            assert!(matches!(
                decode_capability_input(
                    CapabilityId::AssetsTrashPrepare,
                    forged.to_string().as_bytes()
                ),
                Err(AgentErrorCode::InvalidInput)
            ));
        }
        assert_eq!(capability_from_id("plans.approve"), None);
        assert_eq!(capability_from_id("operations.resume"), None);
    }

    /// A4-05/A3-03：维护门必须立在 GUI、Host、MCP 共用的执行边界上。
    ///
    /// 判据取自能力描述表的 `access`，不是手写清单——手写清单会在下一个写能力
    /// 加入时静默失效，而这类洞只在「迁仓进行中模型仍能写」时才暴露。
    #[test]
    fn maintenance_blocks_every_write_capability_and_keeps_reads_working() {
        let gate = std::sync::Arc::new(LibraryMaintenanceGate::new());
        // 未维护时一律放行（否则门会把正常写入全挡死）。
        for descriptor in crate::contracts::agent::CAPABILITY_DESCRIPTORS {
            assert_eq!(
                ensure_maintenance_allows(descriptor.capability, Some(&gate)),
                Ok(()),
                "{:?} 在未维护时不该被挡",
                descriptor.capability
            );
        }

        let lease = std::sync::Arc::clone(&gate).enter("维护门回归").unwrap();
        let mut blocked = 0;
        for descriptor in crate::contracts::agent::CAPABILITY_DESCRIPTORS {
            let verdict = ensure_maintenance_allows(descriptor.capability, Some(&gate));
            match descriptor.access {
                AccessKind::Query | AccessKind::Cancel => assert_eq!(
                    verdict,
                    Ok(()),
                    "{:?} 是只读或取消，维护期间仍必须可用",
                    descriptor.capability
                ),
                _ => {
                    assert_eq!(
                        verdict,
                        Err(AgentErrorCode::MaintenanceActive),
                        "{:?} 是写入，维护期间必须在进领域层之前被挡",
                        descriptor.capability
                    );
                    blocked += 1;
                }
            }
        }
        // 防止「表里没有写能力」让这个用例变成永远为真的空断言。
        assert!(
            blocked >= 10,
            "登记为写入的能力只有 {blocked} 个，判据可能失效"
        );

        drop(lease);
        // 租约释放后立刻恢复写入，不需要重启。
        assert_eq!(
            ensure_maintenance_allows(CapabilityId::AssetsMoveSubmit, Some(&gate)),
            Ok(())
        );
        // 未接线（测试直接构造）视为未维护，与 MCP 桥既有约定一致。
        assert_eq!(
            ensure_maintenance_allows(CapabilityId::AssetsMoveSubmit, None),
            Ok(())
        );
    }
}
