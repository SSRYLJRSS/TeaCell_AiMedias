/** Validate the source before generating any language bindings. */
export function validateAgentContract(source) {
  if (source.schemaVersion !== 1 || !source.enums || source.maxRequestBytes !== 65536) throw new Error('INVALID_AGENT_CONTRACT');
  if (source.maxResponseBytes !== 65536 || source.maxPageItems !== 100) throw new Error('INVALID_AGENT_OUTPUT_LIMITS');
  if (source.maxWorkflowSteps !== 8 || source.maxWorkflowParameters !== 7 || source.enums.WorkflowParameterKey.length !== source.maxWorkflowParameters || Object.keys(source.records.WorkflowInputs).some(key => !source.enums.WorkflowParameterKey.includes(key))) throw new Error('INVALID_WORKFLOW_CONTRACT');
  if (source.scalars?.ResourceRef?.asciiToken !== true || source.scalars?.ShortText?.rejectNul !== true || source.scalars?.Instructions?.rejectNul !== true || source.scalars.ResourceRef.minBytes > source.scalars.ResourceRef.maxBytes) throw new Error('INVALID_AGENT_SCALARS');
  for (const rules of Object.values(source.scalars)) {
    if (!Number.isSafeInteger(rules.maxBytes) || rules.maxBytes < 1 || rules.maxBytes > source.maxRequestBytes || (rules.minBytes !== undefined && (!Number.isSafeInteger(rules.minBytes) || rules.minBytes < 1 || rules.minBytes > rules.maxBytes))) throw new Error('INVALID_AGENT_SCALAR_BOUND');
  }
  for (const [record, fields] of Object.entries(source.fieldRules ?? {})) {
    for (const [field, rule] of Object.entries(fields)) {
      if (source.records[record]?.[field] !== 'u32' || Object.keys(rule).length !== 1 || !Number.isSafeInteger(rule.minimum) || rule.minimum < 1 || rule.minimum > 4294967295) throw new Error('INVALID_AGENT_FIELD_RULE');
    }
  }
  for (const record of ['LoadWorkflowInput', 'PrepareWorkflowInput', 'WorkflowSummary', 'WorkflowPlanPreview']) {
    if (source.fieldRules?.[record]?.version?.minimum !== 1) throw new Error('INVALID_WORKFLOW_VERSION_RULE');
  }
  if (Object.keys(source.errorMessages ?? {}).length !== source.enums.AgentErrorCode.length || source.enums.AgentErrorCode.some(code => typeof source.errorMessages[code] !== 'string' || !source.errorMessages[code].length)) throw new Error('INCOMPLETE_AGENT_ERRORS');
  const capabilities = source.enums.CapabilityId;
  if (!capabilities || Object.keys(source.capabilities ?? {}).length !== capabilities.length || capabilities.some(id => !source.capabilities[id])) throw new Error('INCOMPLETE_AGENT_CAPABILITIES');
  for (const [id, descriptor] of Object.entries(source.capabilities)) {
    if (!(descriptor.input in source.records)) throw new Error(`UNKNOWN_CAPABILITY_INPUT: ${id}`);
    if (!(source.resultTypes?.[descriptor.input] in source.records)) throw new Error(`UNKNOWN_CAPABILITY_OUTPUT: ${id}`);
    if (!source.enums.AccessKind.includes(descriptor.access) || !source.enums.ApprovalRequirement.includes(descriptor.approval)) throw new Error(`INVALID_CAPABILITY_POLICY: ${id}`);
    if (!Array.isArray(descriptor.callers) || !descriptor.callers.length || new Set(descriptor.callers).size !== descriptor.callers.length || descriptor.callers.some(caller => !source.enums.CallerKind.includes(caller))) throw new Error(`INVALID_CAPABILITY_CALLERS: ${id}`);
    const trusted = id === 'plans.approve' || id === 'operations.resume';
    if (trusted && (descriptor.callers.length !== 1 || descriptor.callers[0] !== 'gui')) throw new Error(`UNTRUSTED_APPROVAL_ENTRY: ${id}`);
    const access = id === 'plans.approve' ? 'approve' : id === 'operations.resume' ? 'resume' : id === 'operations.cancel' ? 'cancel' : id.endsWith('.submit') ? 'submit' : id.endsWith('.prepare') ? 'prepare' : 'query';
    const approval = id === 'plans.approve' ? 'trusted_review' : id === 'operations.resume' ? 'resume_review' : id === 'trash.empty.submit' ? 'permanent_trash' : id.endsWith('.submit') ? 'plan' : 'none';
    if (descriptor.access !== access || descriptor.approval !== approval) throw new Error(`CAPABILITY_POLICY_CHANGED: ${id}`);
  }
}
