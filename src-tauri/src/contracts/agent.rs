//! Shared agent boundary types. Decoding a reference never grants access.
pub use super::agent_enums::*;
pub use super::agent_records::*;
use serde::{Deserialize, Deserializer, Serialize};

macro_rules! bounded_text {
    ($name:ident, $max:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                if value.len() > $max || value.contains('\0') {
                    return Err(serde::de::Error::custom("INVALID_INPUT"));
                }
                Ok(Self(value))
            }
        }
    };
}

bounded_text!(ShortText, SHORT_TEXT_MAX_BYTES);
bounded_text!(Instructions, INSTRUCTIONS_MAX_BYTES);

pub(super) mod sealed {
    pub trait Sealed {}
    pub trait OutputSealed {}
}

/// Only registered generated input types may use the boundary decoder.
///
/// ```compile_fail
/// use teacell_ai_media_manager_lib::contracts::agent::decode_agent_input;
/// let _ = decode_agent_input::<serde_json::Value>(b"{}");
/// ```
pub trait AgentInput: sealed::Sealed + serde::de::DeserializeOwned {
    fn validate_input(&self) -> Result<(), AgentErrorCode>;
}

pub trait AgentOutput: sealed::OutputSealed + Serialize {
    fn validate_output(&self) -> Result<(), AgentErrorCode>;
}

pub(super) fn validate_review_kind(plan: &PlanPreview) -> Result<(), AgentErrorCode> {
    let descriptor = CAPABILITY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.capability == plan.capability)
        .ok_or(AgentErrorCode::Internal)?;
    let expected = match (descriptor.access, descriptor.approval) {
        (AccessKind::Submit, ApprovalRequirement::PermanentTrash) => ReviewKind::PermanentTrash,
        (AccessKind::Submit, ApprovalRequirement::Plan) => ReviewKind::Ordinary,
        _ => return Err(AgentErrorCode::Internal),
    };
    if plan.review_kind != expected {
        return Err(AgentErrorCode::Internal);
    }
    Ok(())
}

fn validate_workflow_steps(steps: &[WorkflowStepSummary]) -> Result<(), AgentErrorCode> {
    if steps.is_empty() || steps.len() > MAX_WORKFLOW_STEPS {
        return Err(AgentErrorCode::Internal);
    }
    for (index, step) in steps.iter().enumerate() {
        let descriptor = CAPABILITY_DESCRIPTORS
            .iter()
            .find(|value| value.capability == step.capability)
            .ok_or(AgentErrorCode::Internal)?;
        let expected = match (descriptor.access, descriptor.approval) {
            (AccessKind::Submit, ApprovalRequirement::Plan) => ReviewKind::Ordinary,
            (AccessKind::Submit, ApprovalRequirement::PermanentTrash) => ReviewKind::PermanentTrash,
            _ => return Err(AgentErrorCode::Internal),
        };
        if step.capability == CapabilityId::WorkflowsSubmit
            || step.review_kind != expected
            || step
                .depends_on
                .is_some_and(|dependency| dependency as usize >= index)
        {
            return Err(AgentErrorCode::Internal);
        }
    }
    Ok(())
}

pub(super) fn validate_workflow_summary(summary: &WorkflowSummary) -> Result<(), AgentErrorCode> {
    if summary.parameters.len() > MAX_WORKFLOW_PARAMETERS {
        return Err(AgentErrorCode::Internal);
    }
    for (index, parameter) in summary.parameters.iter().enumerate() {
        if summary.parameters[..index]
            .iter()
            .any(|previous| previous.key == parameter.key)
        {
            return Err(AgentErrorCode::Internal);
        }
    }
    validate_workflow_steps(&summary.steps)
}

pub(super) fn validate_workflow_preview(
    preview: &WorkflowPlanPreview,
) -> Result<(), AgentErrorCode> {
    if preview.plan.capability != CapabilityId::WorkflowsSubmit {
        return Err(AgentErrorCode::Internal);
    }
    preview.plan.validate_output()?;
    validate_workflow_steps(&preview.steps)
}

/// Produce the whole bounded response before an adapter sends any bytes.
pub fn encode_agent_output<T: AgentOutput>(value: &T) -> Result<Vec<u8>, AgentErrorCode> {
    value.validate_output()?;
    struct ResponseBuffer(Vec<u8>);
    impl std::io::Write for ResponseBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RESPONSE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("agent response limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = ResponseBuffer(Vec::with_capacity(MAX_RESPONSE_BYTES));
    serde_json::to_writer(&mut buffer, value).map_err(|_| AgentErrorCode::Internal)?;
    Ok(buffer.0)
}

/// Pure byte-boundary decoder for future Pi/MCP adapters. It performs no IO.
/// Caller identity must still be supplied separately by the trusted adapter.
pub fn decode_agent_input<T: AgentInput>(bytes: &[u8]) -> Result<T, AgentErrorCode> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(AgentErrorCode::InvalidInput);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Header {
        schema_version: u32,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| AgentErrorCode::InvalidInput)?;
    if header.schema_version != 1 {
        return Err(AgentErrorCode::UnsupportedProtocol);
    }
    let input: T = serde_json::from_slice(bytes).map_err(|_| AgentErrorCode::InvalidInput)?;
    input.validate_input()?;
    Ok(input)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ResourceRef(String);

impl ResourceRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Host-only constructor for values generated from authenticated state.
    /// Public input must still pass through serde validation.
    pub(crate) fn from_trusted(value: impl Into<String>) -> Result<Self, AgentErrorCode> {
        let value = value.into();
        if !(RESOURCE_REF_MIN_BYTES..=RESOURCE_REF_MAX_BYTES).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(AgentErrorCode::InvalidInput);
        }
        Ok(Self(value))
    }

    pub(crate) fn generated(prefix: &str) -> Self {
        Self::from_trusted(format!("{prefix}_{}", uuid::Uuid::new_v4().simple()))
            .expect("host-generated resource references are valid")
    }
}

impl<'de> Deserialize<'de> for ResourceRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if !(RESOURCE_REF_MIN_BYTES..=RESOURCE_REF_MAX_BYTES).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(serde::de::Error::custom("INVALID_INPUT"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProtocolVersion(u32);

impl ProtocolVersion {
    pub const V1: Self = Self(1);
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u32::deserialize(deserializer)? {
            1 => Ok(Self::V1),
            _ => Err(serde::de::Error::custom("UNSUPPORTED_PROTOCOL")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn input_example(catalog: &Value, type_name: &str) -> Value {
        match type_name {
            "ProtocolVersion" | "u32" => json!(1),
            "ResourceRef" => json!("resource_0123456789abcdef"),
            "ShortText" | "Instructions" => json!("fixture"),
            "bool" => json!(false),
            _ if catalog["enums"][type_name].is_array() => catalog["enums"][type_name][0].clone(),
            _ => {
                let fields = catalog["records"][type_name].as_object().unwrap();
                let mut object = serde_json::Map::new();
                for (field, field_type) in fields {
                    let field_type = field_type.as_str().unwrap();
                    if !field_type.ends_with('?') {
                        object.insert(field.clone(), input_example(catalog, field_type));
                    }
                }
                Value::Object(object)
            }
        }
    }

    #[test]
    fn every_registered_capability_has_positive_and_forged_input_examples() {
        let catalog: Value =
            serde_json::from_str(include_str!("../../../docs/contracts/agent-v1.json")).unwrap();
        for (id, descriptor) in catalog["capabilities"].as_object().unwrap() {
            let capability: CapabilityId = serde_json::from_value(json!(id)).unwrap();
            let valid = input_example(&catalog, descriptor["input"].as_str().unwrap());
            let bytes = serde_json::to_vec(&valid).unwrap();
            assert!(decode_capability_input(capability, &bytes).is_ok(), "{id}");
            for field in [
                "approved",
                "principalId",
                "grantEpoch",
                "sql",
                "shell",
                "path",
            ] {
                let mut forged = valid.clone();
                forged[field] = json!(true);
                assert_eq!(
                    decode_capability_input(capability, &serde_json::to_vec(&forged).unwrap())
                        .unwrap_err(),
                    AgentErrorCode::InvalidInput,
                    "{id}:{field}"
                );
            }
            let mut future = valid;
            future["schemaVersion"] = json!(2);
            assert_eq!(
                decode_capability_input(capability, &serde_json::to_vec(&future).unwrap())
                    .unwrap_err(),
                AgentErrorCode::UnsupportedProtocol,
                "{id}"
            );
        }
    }

    #[test]
    fn workflow_outputs_reject_cycles_nested_workflows_and_reused_approval_kinds() {
        let token = "resource_0123456789abcdef";
        let valid = json!({"schemaVersion":1,"workflowRef":token,"version":2,"name":"入库打标","description":"每次新计划","parameters":[{"key":"sourceRef","required":true,"description":"本次选择"}],"steps":[{"capability":"imports.submit","reviewKind":"ordinary"},{"capability":"tagging.submit","dependsOn":0,"reviewKind":"ordinary"},{"capability":"trash.empty.submit","dependsOn":1,"reviewKind":"permanent_trash"}]});
        let summary: WorkflowSummary = serde_json::from_value(valid.clone()).unwrap();
        assert!(encode_agent_output(&summary).is_ok());
        let mut variants = Vec::new();
        let mut cyclic = valid.clone();
        cyclic["steps"][1]["dependsOn"] = json!(1);
        variants.push(cyclic);
        let mut nested = valid.clone();
        nested["steps"][0]["capability"] = json!("workflows.submit");
        variants.push(nested);
        let mut wrong_review = valid.clone();
        wrong_review["steps"][2]["reviewKind"] = json!("ordinary");
        variants.push(wrong_review);
        let mut duplicate = valid.clone();
        duplicate["parameters"]
            .as_array_mut()
            .unwrap()
            .push(valid["parameters"][0].clone());
        variants.push(duplicate);
        let mut zero_version = valid;
        zero_version["version"] = json!(0);
        variants.push(zero_version);
        for invalid in variants {
            let summary: WorkflowSummary = serde_json::from_value(invalid).unwrap();
            assert_eq!(
                encode_agent_output(&summary).unwrap_err(),
                AgentErrorCode::Internal
            );
        }
        let zero_request = json!({"schemaVersion":1,"workflowRef":token,"version":0});
        assert_eq!(
            decode_agent_input::<LoadWorkflowInput>(&serde_json::to_vec(&zero_request).unwrap())
                .unwrap_err(),
            AgentErrorCode::InvalidInput
        );
    }

    fn assert_strict<T: for<'de> Deserialize<'de> + Serialize>(valid: Value) {
        let decoded: T = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), valid);
        for field in [
            "approved",
            "principalId",
            "originChain",
            "grantEpoch",
            "sql",
            "path",
        ] {
            let mut forged = valid.clone();
            forged[field] = json!(true);
            assert!(serde_json::from_value::<T>(forged).is_err(), "{field}");
        }
    }

    #[test]
    fn domain_inputs_round_trip_and_reject_untrusted_authority() {
        let token = "resource_0123456789abcdef";
        assert_strict::<SearchAssetsInput>(json!({"schemaVersion":1,"text":"找有茶杯的照片"}));
        assert_strict::<PrepareImportInput>(
            json!({"schemaVersion":1,"sourceRef":token,"collectionRef":token,"renamePattern":"{原名}"}),
        );
        assert_strict::<PrepareTaggingInput>(
            json!({"schemaVersion":1,"selectionRef":token,"serviceRef":token,"instructions":"识别主题，等待审核"}),
        );
        assert_strict::<PrepareExportInput>(
            json!({"schemaVersion":1,"selectionRef":token,"destinationRef":token,"layout":"flat"}),
        );
        assert_strict::<PrepareMoveInput>(
            json!({"schemaVersion":1,"selectionRef":token,"collectionRef":token}),
        );
        assert_strict::<PrepareSelectionInput>(json!({"schemaVersion":1,"selectionRef":token}));
        assert_strict::<ApprovePlanInput>(
            json!({"schemaVersion":1,"planId":token,"reviewRef":token}),
        );
        assert_strict::<LoadWorkflowInput>(
            json!({"schemaVersion":1,"workflowRef":token,"version":1}),
        );
        assert_strict::<PrepareWorkflowInput>(
            json!({"schemaVersion":1,"workflowRef":token,"version":1,"inputs":{"sourceRef":token,"taggingInstructions":"主题"}}),
        );
        let forged =
            json!({"schemaVersion":1,"workflowRef":token,"version":1,"inputs":{"approved":true}});
        assert!(serde_json::from_value::<PrepareWorkflowInput>(forged).is_err());
    }

    #[test]
    fn text_limits_are_utf8_bytes_and_export_has_no_move_mode() {
        assert!(serde_json::from_value::<ShortText>(json!("a".repeat(256))).is_ok());
        assert!(serde_json::from_value::<ShortText>(json!("a".repeat(257))).is_err());
        assert!(serde_json::from_value::<Instructions>(json!("茶".repeat(1366))).is_err());
        assert!(serde_json::from_value::<Instructions>(json!("a\0b")).is_err());
        let mut export = json!({"schemaVersion":1,"selectionRef":"resource_0123456789abcdef","destinationRef":"resource_0123456789abcdef","layout":"flat"});
        export["mode"] = json!("move");
        assert!(serde_json::from_value::<PrepareExportInput>(export).is_err());
    }

    #[test]
    fn bounded_decoder_classifies_versions_without_error_string_matching() {
        let valid = br#"{"schemaVersion":1,"operationId":"operation_0123456789abcdef"}"#;
        assert!(decode_agent_input::<OperationInput>(valid).is_ok());
        let future = br#"{"schemaVersion":2,"operationId":"operation_0123456789abcdef"}"#;
        assert_eq!(
            decode_agent_input::<OperationInput>(future).unwrap_err(),
            AgentErrorCode::UnsupportedProtocol
        );
        for invalid in [
            br#"{"schemaVersion":1,"schemaVersion":1,"operationId":"operation_0123456789abcdef"}"#
                .as_slice(),
            br#"{"schemaVersion":1,"operationId":null}"#.as_slice(),
            br#"{"schemaVersion":1,"operationId":"operation_0123456789abcdef","approved":true}"#
                .as_slice(),
            b"\xff".as_slice(),
        ] {
            assert_eq!(
                decode_agent_input::<OperationInput>(invalid).unwrap_err(),
                AgentErrorCode::InvalidInput
            );
        }
        let mut padded = vec![b' '; MAX_REQUEST_BYTES];
        padded.extend_from_slice(valid);
        assert_eq!(
            decode_agent_input::<OperationInput>(&padded).unwrap_err(),
            AgentErrorCode::InvalidInput
        );
    }

    #[test]
    fn generated_outputs_round_trip_without_paths_or_unsafe_integer_counts() {
        let token = "resource_0123456789abcdef";
        let library = json!({"libraryId":token,"libraryEpoch":token});
        assert_strict::<SearchPage>(
            json!({"schemaVersion":1,"library":library,"selectionRef":token,"total":10,"truncated":true,"items":[{"assetRef":token,"displayName":"茶杯.jpg"}]}),
        );
        assert_strict::<PlanPreview>(
            json!({"schemaVersion":1,"planId":token,"capability":"imports.submit","library":library,"reviewStatus":"pending","reviewKind":"ordinary","itemCount":2,"summary":"复制两项入库","reviewRef":token,"expiresAtUnixSeconds":1791500000,"scope":{"scopeRef":token,"sourceRef":token,"targetRef":token},"budget":{"maxRequests":0,"maxTokens":0,"maxSeconds":300,"maxItems":2}}),
        );
        assert_strict::<OperationSnapshot>(
            json!({"schemaVersion":1,"operationId":token,"library":library,"revision":3,"state":"outcome_unknown","phase":"reconciling","completed":1,"failed":0,"errorCode":"OUTCOME_UNKNOWN","canCancel":false,"canResume":false}),
        );
        assert_strict::<ApprovalResult>(
            json!({"schemaVersion":1,"planId":token,"reviewStatus":"approved"}),
        );
        assert_strict::<WorkflowSummary>(
            json!({"schemaVersion":1,"workflowRef":token,"version":1,"name":"入库打标","description":"每次重新准备计划","parameters":[{"key":"sourceRef","required":true,"description":"本次输入"}],"steps":[{"capability":"imports.submit","reviewKind":"ordinary"}]}),
        );
        let unsafe_count = json!({"schemaVersion":1,"library":library,"selectionRef":token,"total":9007199254740992u64,"truncated":false,"items":[]});
        assert!(serde_json::from_value::<SearchPage>(unsafe_count).is_err());
    }

    #[test]
    fn registered_dispatch_uses_the_capability_specific_input() {
        let request = br#"{"schemaVersion":1,"operationId":"operation_0123456789abcdef"}"#;
        assert!(matches!(
            decode_capability_input(CapabilityId::OperationsGet, request).unwrap(),
            CapabilityInput::OperationsGet(_)
        ));
        assert_eq!(
            decode_capability_input(CapabilityId::PlansApprove, request).unwrap_err(),
            AgentErrorCode::InvalidInput
        );
        let approve = br#"{"schemaVersion":1,"planId":"plan_0123456789abcdef","reviewRef":"review_0123456789abcdef"}"#;
        assert!(matches!(
            decode_capability_input(CapabilityId::PlansApprove, approve).unwrap(),
            CapabilityInput::PlansApprove(_)
        ));
        assert_eq!(
            decode_capability_input(CapabilityId::ImportsSubmit, approve).unwrap_err(),
            AgentErrorCode::InvalidInput
        );
    }

    #[test]
    fn response_limit_rejects_oversized_pages_and_escaped_json_before_publication() {
        let token = "resource_0123456789abcdef";
        let item = json!({"assetRef":token,"displayName":"茶杯.jpg"});
        let make_page = |items: Vec<Value>| -> SearchPage {
            serde_json::from_value(json!({"schemaVersion":1,"library":{"libraryId":token,"libraryEpoch":token},"selectionRef":token,"total":items.len(),"truncated":false,"items":items})).unwrap()
        };
        let valid = make_page(vec![item.clone(); MAX_PAGE_ITEMS]);
        let encoded = encode_agent_output(&valid).unwrap();
        assert!(encoded.len() <= MAX_RESPONSE_BYTES);
        assert_eq!(
            serde_json::from_slice::<SearchPage>(&encoded).unwrap(),
            valid
        );
        let excessive = make_page(vec![item; MAX_PAGE_ITEMS + 1]);
        assert_eq!(
            encode_agent_output(&excessive).unwrap_err(),
            AgentErrorCode::Internal
        );
        // Raw strings meet their own byte limit; JSON escaping makes the response exceed it.
        let escaped = make_page(vec![
            json!({"assetRef":token,"displayName":"\u{1}".repeat(SHORT_TEXT_MAX_BYTES)});
            MAX_PAGE_ITEMS
        ]);
        assert!(serde_json::to_vec(&escaped).unwrap().len() > MAX_RESPONSE_BYTES);
        assert_eq!(
            encode_agent_output(&escaped).unwrap_err(),
            AgentErrorCode::Internal
        );
    }

    #[test]
    fn review_output_rejects_incomplete_service_and_budget_snapshots() {
        let token = "resource_0123456789abcdef";
        let valid = json!({"schemaVersion":1,"planId":token,"capability":"tagging.submit","library":{"libraryId":token,"libraryEpoch":token},"reviewStatus":"pending","reviewKind":"ordinary","itemCount":2,"summary":"识别主题后等待标签审核","reviewRef":token,"expiresAtUnixSeconds":1791500000,"scope":{"scopeRef":token,"selectionRef":token,"serviceRef":token,"serviceRevision":token,"model":"vision-model"},"budget":{"maxRequests":2,"maxTokens":2000,"maxSeconds":300,"maxItems":2,"monetaryLimit":{"currency":"USD","amountMicrounits":1000000}}});
        let preview: PlanPreview = serde_json::from_value(valid.clone()).unwrap();
        assert!(encode_agent_output(&preview).is_ok());
        let mut wrong_review = valid.clone();
        wrong_review["capability"] = json!("trash.empty.submit");
        let preview: PlanPreview = serde_json::from_value(wrong_review.clone()).unwrap();
        assert_eq!(
            encode_agent_output(&preview).unwrap_err(),
            AgentErrorCode::Internal
        );
        wrong_review["reviewKind"] = json!("permanent_trash");
        let preview: PlanPreview = serde_json::from_value(wrong_review.clone()).unwrap();
        assert!(encode_agent_output(&preview).is_ok());
        wrong_review["capability"] = json!("tagging.submit");
        let preview: PlanPreview = serde_json::from_value(wrong_review).unwrap();
        assert_eq!(
            encode_agent_output(&preview).unwrap_err(),
            AgentErrorCode::Internal
        );
        let mut non_submit = valid.clone();
        non_submit["capability"] = json!("assets.search");
        let preview: PlanPreview = serde_json::from_value(non_submit).unwrap();
        assert_eq!(
            encode_agent_output(&preview).unwrap_err(),
            AgentErrorCode::Internal
        );
        for field in ["serviceRevision", "model"] {
            let mut incomplete = valid.clone();
            incomplete["scope"].as_object_mut().unwrap().remove(field);
            let preview: PlanPreview = serde_json::from_value(incomplete).unwrap();
            assert_eq!(
                encode_agent_output(&preview).unwrap_err(),
                AgentErrorCode::Internal
            );
        }
        let mut over_budget = valid.clone();
        over_budget["budget"]["maxItems"] = json!(1);
        let preview: PlanPreview = serde_json::from_value(over_budget).unwrap();
        assert_eq!(
            encode_agent_output(&preview).unwrap_err(),
            AgentErrorCode::Internal
        );
        let mut wrong_currency = valid;
        wrong_currency["budget"]["monetaryLimit"]["currency"] = json!("usd");
        let preview: PlanPreview = serde_json::from_value(wrong_currency).unwrap();
        assert_eq!(
            encode_agent_output(&preview).unwrap_err(),
            AgentErrorCode::Internal
        );
    }

    #[test]
    fn submission_returns_snapshot_and_replay_flag_with_checked_progress() {
        let token = "resource_0123456789abcdef";
        let valid = json!({"schemaVersion":1,"operation":{"schemaVersion":1,"operationId":token,"library":{"libraryId":token,"libraryEpoch":token},"revision":1,"state":"queued","phase":"ready","completed":0,"failed":0,"total":2,"canCancel":true,"canResume":false},"replayed":true});
        let result: SubmissionResult = serde_json::from_value(valid.clone()).unwrap();
        assert!(encode_agent_output(&result).is_ok());
        let mut invalid = valid;
        invalid["operation"]["completed"] = json!(3);
        let result: SubmissionResult = serde_json::from_value(invalid).unwrap();
        assert_eq!(
            encode_agent_output(&result).unwrap_err(),
            AgentErrorCode::Internal
        );
    }

    #[test]
    fn submit_rejects_forged_identity_approval_and_unknown_versions() {
        let valid = json!({"schemaVersion":1,"planId":"plan_0123456789abcdef","idempotencyKey":"submit_0123456789abcdef"});
        let decoded: SubmitPlanInput = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), valid);
        for field in [
            "approved",
            "principalId",
            "libraryEpoch",
            "directory",
            "sql",
        ] {
            let mut forged = valid.clone();
            forged[field] = json!(true);
            assert!(
                serde_json::from_value::<SubmitPlanInput>(forged).is_err(),
                "{field}"
            );
        }
        for version in [0, 2, 99] {
            let mut future = valid.clone();
            future["schemaVersion"] = json!(version);
            assert!(serde_json::from_value::<SubmitPlanInput>(future).is_err());
        }
    }

    #[test]
    fn references_are_bounded_tokens_not_paths_or_authorization() {
        for value in [
            "short",
            "../../outside",
            "C:/Users/secret.jpg",
            "ref_with_空格字符",
            &"x".repeat(129),
        ] {
            assert!(serde_json::from_value::<ResourceRef>(json!(value)).is_err());
        }
        let token: ResourceRef =
            serde_json::from_value(json!("resource_0123456789abcdef")).unwrap();
        assert_eq!(token.as_str(), "resource_0123456789abcdef");
    }

    #[test]
    fn canonical_catalog_round_trips_and_unknown_enum_values_fail() {
        let catalog: Value =
            serde_json::from_str(include_str!("../../../docs/contracts/agent-v1.json")).unwrap();
        for value in catalog["enums"]["CapabilityId"].as_array().unwrap() {
            let capability: CapabilityId = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(capability).unwrap(), *value);
        }
        assert!(serde_json::from_value::<CapabilityId>(json!("shell.execute")).is_err());
        for value in catalog["enums"]["OperationState"].as_array().unwrap() {
            let state: OperationState = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(state).unwrap(), *value);
        }
        assert!(serde_json::from_value::<OperationState>(json!("approved")).is_err());
    }
}
