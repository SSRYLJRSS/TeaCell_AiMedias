use crate::{digest, fail, workflow_db::WorkflowStore, Caller, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub collection: String,
    pub tagging_instructions: String,
    pub rename: bool,
}
impl Parameters {
    pub fn validate(&self) -> Result<()> {
        if self.collection.trim().is_empty()
            || self.collection.len() > 128
            || self.tagging_instructions.len() > 2048
        {
            return fail("INVALID_PARAMETERS");
        }
        if self
            .collection
            .chars()
            .any(|c| c.is_control() || "/\\:".contains(c))
            || matches!(self.collection.as_str(), "." | "..")
        {
            return fail("INVALID_COLLECTION");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Import,
    SuggestTags { input_step: usize },
    EmptyRecycleBin,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub schema: u32,
    pub capability_schema: u32,
    pub id: String,
    pub version: u32,
    pub name: String,
    pub defaults: Parameters,
    pub steps: Vec<Step>,
}
impl Definition {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 || self.capability_schema != 1 {
            return fail("CAPABILITY_VERSION_UNSUPPORTED");
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || self.version == 0
            || self.name.trim().is_empty()
            || self.name.len() > 128
        {
            return fail("INVALID_WORKFLOW");
        }
        self.defaults.validate()?;
        if self.steps.is_empty()
            || self.steps.len() > 8
            || self.steps.first() != Some(&Step::Import)
        {
            return fail("INVALID_STEPS");
        }
        let mut imported = false;
        for (index, step) in self.steps.iter().enumerate() {
            match step {
                Step::Import if index == 0 => imported = true,
                Step::SuggestTags { input_step } if imported && *input_step == 0 && index == 1 => {}
                Step::EmptyRecycleBin if index == self.steps.len() - 1 => {}
                _ => return fail("INVALID_STEP_DEPENDENCY"),
            }
        }
        Ok(())
    }
    pub fn hash(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
    pub fn skill_text(&self) -> Result<String> {
        self.validate()?;
        // The payload is data. It cannot add tools, approvals, resource handles or executable scripts.
        Ok(format!("个人流程数据，版本 {}，hash {}。\n{}\n请使用受控 prepare 工具产生本次计划。每次仍需新的用户确认；标签审核和清空回收站的独立确认保持有效。",self.version,self.hash()?,serde_json::to_string(self)?))
    }
}

// Trusted adapter context, deliberately not deserializable from a model request.
pub struct DirectoryGrant {
    root: PathBuf,
    epoch: u64,
    active: bool,
}
impl DirectoryGrant {
    pub fn fixture(caller: Caller, root: &Path, epoch: u64) -> Result<Self> {
        if caller != Caller::TrustedGui {
            return fail("TRUSTED_GRANT_REQUIRED");
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return fail("DIRECTORY_UNAVAILABLE");
        }
        Ok(Self {
            root,
            epoch,
            active: true,
        })
    }
    pub fn revoke(&mut self) {
        self.active = false;
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoundInput {
    pub location: PathBuf,
    pub digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedAction {
    CopyImport,
    GeneratePendingTags,
    AwaitIndependentTrashApproval,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparedWorkflow {
    pub plan_id: i64,
    pub workflow_id: String,
    pub workflow_version: u32,
    pub workflow_hash: String,
    pub library_epoch: u64,
    pub parameters: Parameters,
    pub inputs: Vec<BoundInput>,
    pub actions: Vec<PreparedAction>,
    pub state: String,
}
pub fn save(
    store: &mut WorkflowStore,
    definition: &Definition,
    expected_version: u32,
    caller: Caller,
) -> Result<()> {
    if caller != Caller::TrustedGui {
        return fail("TRUSTED_EDIT_REQUIRED");
    }
    store.save(definition, expected_version)
}
pub fn set_enabled(store: &WorkflowStore, id: &str, enabled: bool, caller: Caller) -> Result<()> {
    if caller != Caller::TrustedGui {
        return fail("TRUSTED_EDIT_REQUIRED");
    }
    store.set_enabled(id, enabled)
}
pub fn instantiate(
    store: &WorkflowStore,
    id: &str,
    version: u32,
    parameters: Parameters,
    files: &[PathBuf],
    grant: &DirectoryGrant,
    current_epoch: u64,
) -> Result<PreparedWorkflow> {
    let definition = store.load(id, version)?;
    parameters.validate()?;
    if !grant.active || grant.epoch != current_epoch {
        return fail("DIRECTORY_GRANT_EXPIRED");
    }
    if !grant.root.is_dir() {
        return fail("DIRECTORY_UNAVAILABLE");
    }
    if files.is_empty() || files.len() > 32 {
        return fail("INVALID_SELECTION");
    }
    let mut inputs = Vec::new();
    let mut selected = std::collections::HashSet::new();
    for relative in files {
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            || relative.as_os_str().is_empty()
        {
            return fail("INVALID_INPUT_LOCATION");
        }
        let mut path = grant.root.clone();
        for part in relative.components() {
            path.push(part);
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return fail("LINK_REJECTED");
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return fail("LINK_REJECTED");
                }
            }
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&grant.root) || !canonical.is_file() {
            return fail("DIRECTORY_NOT_GRANTED");
        }
        if !selected.insert(canonical.clone()) {
            return fail("DUPLICATE_INPUT");
        }
        inputs.push(BoundInput {
            location: relative.clone(),
            digest: digest(&canonical)?,
        });
    }
    let actions = definition
        .steps
        .iter()
        .map(|step| match step {
            Step::Import => PreparedAction::CopyImport,
            Step::SuggestTags { .. } => PreparedAction::GeneratePendingTags,
            Step::EmptyRecycleBin => PreparedAction::AwaitIndependentTrashApproval,
        })
        .collect();
    let mut plan = PreparedWorkflow {
        plan_id: 0,
        workflow_id: definition.id.clone(),
        workflow_version: definition.version,
        workflow_hash: definition.hash()?,
        library_epoch: current_epoch,
        parameters,
        inputs,
        actions,
        state: "awaiting_confirmation".into(),
    };
    store.insert_plan(&mut plan)?;
    Ok(plan)
}
