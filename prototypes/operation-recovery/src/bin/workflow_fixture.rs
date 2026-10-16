use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};
use teacell_operation_recovery_poc::{
    workflow::{self, Definition, DirectoryGrant, Parameters},
    workflow_db::WorkflowStore,
    Caller, Result,
};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Save {
        definition: Definition,
        expected_version: u32,
    },
    Load {
        id: String,
        version: u32,
    },
    Prepare {
        id: String,
        version: u32,
        parameters: Parameters,
        input_names: Vec<PathBuf>,
    },
}
fn run() -> Result<serde_json::Value> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 || !matches!(args[1].as_str(), "agent" | "admin") {
        return Err("INVALID_FIXTURE_ARGUMENTS".into());
    }
    let db = Path::new(&args[2]);
    if args[1] == "agent" && !db.is_file() {
        return Err("WORKFLOW_STORE_UNAVAILABLE".into());
    }
    let mut input = String::new();
    io::stdin().take(65537).read_to_string(&mut input)?;
    if input.len() > 65536 {
        return Err("FRAME_LIMIT".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    let mut store = WorkflowStore::open(db)?;
    match request {
        Request::Save {
            definition,
            expected_version,
        } => {
            if args[1] != "admin" {
                return Err("TRUSTED_EDIT_REQUIRED".into());
            }
            workflow::save(
                &mut store,
                &definition,
                expected_version,
                Caller::TrustedGui,
            )?;
            Ok(json!({"id":definition.id,"version":definition.version,"hash":definition.hash()?}))
        }
        Request::Load { id, version } => {
            let definition = store.load(&id, version)?;
            Ok(json!({"definition":definition,"skill":definition.skill_text()?}))
        }
        Request::Prepare {
            id,
            version,
            parameters,
            input_names,
        } => {
            // The root is supplied by the fixture host argument, never taken from tool JSON.
            let grant = DirectoryGrant::fixture(Caller::TrustedGui, Path::new(&args[3]), 1)?;
            let plan =
                workflow::instantiate(&store, &id, version, parameters, &input_names, &grant, 1)?;
            Ok(serde_json::to_value(plan)?)
        }
    }
}
fn main() {
    match run() {
        Ok(data) => println!("{}", json!({"ok":true,"data":data})),
        // Fixture diagnostics are not a production logging/error serialization policy.
        Err(error) => {
            println!("{}", json!({"ok":false,"error":error.to_string()}));
            std::process::exit(1);
        }
    }
}
