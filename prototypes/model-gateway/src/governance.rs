//! Trusted fixture control, supplied by the test owner at process startup.
//! This is not a production credential store or an OS security boundary.
use serde::Deserialize;
use serde_json::Value;
use std::{io::Read, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    revision: u32,
    enabled: bool,
}

#[derive(Clone)]
pub struct Governance {
    policy_path: Option<PathBuf>,
}
impl Governance {
    pub fn fixture(policy_path: Option<PathBuf>) -> Self {
        Self { policy_path }
    }
    pub fn check(&self, revision: u32) -> Result<(), &'static str> {
        let policy = match &self.policy_path {
            None => Policy {
                revision: 1,
                enabled: true,
            },
            Some(path) => {
                let metadata = std::fs::symlink_metadata(path).map_err(|_| "POLICY_UNAVAILABLE")?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err("POLICY_UNAVAILABLE");
                }
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .map_err(|_| "POLICY_UNAVAILABLE")?
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "POLICY_UNAVAILABLE")?;
                if bytes.len() > 4096 {
                    return Err("POLICY_UNAVAILABLE");
                }
                serde_json::from_slice(&bytes).map_err(|_| "POLICY_UNAVAILABLE")?
            }
        };
        if !policy.enabled {
            return Err("CONNECTION_REVOKED");
        }
        if revision != policy.revision {
            return Err("CONNECTION_CHANGED");
        }
        Ok(())
    }

    // The experiment has no approved asset handles. Known media blocks must
    // therefore fail closed instead of accepting model-supplied image URLs.
    pub fn check_text_only(body: &Value) -> Result<(), &'static str> {
        match body {
            Value::Object(object) => {
                if let Some(kind) = object.get("type").and_then(Value::as_str) {
                    if matches!(
                        kind,
                        "image"
                            | "image_url"
                            | "input_image"
                            | "input_audio"
                            | "audio"
                            | "file"
                            | "video"
                    ) {
                        return Err("MEDIA_APPROVAL_REQUIRED");
                    }
                }
                for value in object.values() {
                    Self::check_text_only(value)?;
                }
            }
            Value::Array(items) => {
                for value in items {
                    Self::check_text_only(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_in_nested_tool_result_is_not_an_approved_preview() {
        for kind in [
            "image",
            "image_url",
            "input_image",
            "input_audio",
            "file",
            "video",
        ] {
            let body = serde_json::json!({"messages":[{"content":[{"type":"tool_result","content":[{"type":kind,"source":{"data":"fixture"}}]}]}]});
            assert_eq!(
                Governance::check_text_only(&body),
                Err("MEDIA_APPROVAL_REQUIRED")
            );
        }
        assert!(Governance::check_text_only(
            &serde_json::json!({"messages":[{"content":[{"type":"text","text":"image"}]}]})
        )
        .is_ok());
    }
}
