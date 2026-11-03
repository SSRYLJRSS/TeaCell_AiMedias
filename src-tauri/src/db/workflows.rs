//! A4-07: personal workflow storage with immutable versions.
//!
//! A workflow is *a template of allowed steps and parameter definitions*. It is
//! deliberately not an approval, not a credential, and not a recorded asset set:
//! every invocation must take fresh inputs, build a fresh plan and be approved
//! again (see `plans/v2/12-implementation-contracts.md` §6 and PRD §8).
//!
//! Two tables keep that promise enforceable:
//!
//! * `workflows` is the mutable header — a display name and which version is
//!   current. Renaming touches only this row, so an operation that already
//!   instantiated version 3 keeps resolving to version 3 under its old name.
//! * `workflow_versions` is append-only. A row is written once and never
//!   updated, which is why "编辑产生可识别的新版本" cannot silently rewrite a
//!   running task's snapshot: there is nothing to rewrite.
//!
//! `workflow_instantiations` records which version a plan came from. It exists
//! so an old plan can still explain its origin after the workflow was renamed,
//! archived or superseded by newer versions.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS workflows (
  workflow_ref    TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  current_version INTEGER NOT NULL,
  archived        INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_workflows_archived ON workflows(archived, updated_at);

CREATE TABLE IF NOT EXISTS workflow_versions (
  workflow_ref         TEXT NOT NULL,
  version              INTEGER NOT NULL,
  name                 TEXT NOT NULL,
  description          TEXT NOT NULL,
  steps_json           TEXT NOT NULL,
  parameters_json      TEXT NOT NULL,
  created_at           INTEGER NOT NULL,
  PRIMARY KEY(workflow_ref, version)
);

CREATE TABLE IF NOT EXISTS workflow_instantiations (
  plan_id      TEXT PRIMARY KEY,
  workflow_ref TEXT NOT NULL,
  version      INTEGER NOT NULL,
  created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_workflow_instantiations_workflow
  ON workflow_instantiations(workflow_ref, version);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHeader {
    pub workflow_ref: String,
    pub name: String,
    pub current_version: i64,
    pub archived: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowVersionRecord {
    pub workflow_ref: String,
    pub version: i64,
    pub name: String,
    pub description: String,
    pub steps_json: String,
    pub parameters_json: String,
    pub created_at: i64,
}

/// A brand-new workflow always starts at version 1.
pub fn create(
    conn: &Connection,
    workflow_ref: &str,
    name: &str,
    description: &str,
    steps_json: &str,
    parameters_json: &str,
    now: i64,
) -> AppResult<WorkflowVersionRecord> {
    if name.trim().is_empty() {
        return Err(AppError::invalid_arg("常用流程需要有名称"));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO workflows(workflow_ref,name,current_version,archived,created_at,updated_at)
         VALUES(?1,?2,1,0,?3,?3)",
        params![workflow_ref, name, now],
    )
    .map_err(|error| match error {
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            AppError::conflict("常用流程引用冲突，请重新保存")
        }
        other => other.into(),
    })?;
    tx.execute(
        "INSERT INTO workflow_versions(workflow_ref,version,name,description,steps_json,parameters_json,created_at)
         VALUES(?1,1,?2,?3,?4,?5,?6)",
        params![workflow_ref, name, description, steps_json, parameters_json, now],
    )?;
    tx.commit()?;
    version(conn, workflow_ref, 1)?.ok_or_else(|| AppError::internal("常用流程版本写入后不可读"))
}

/// The frozen definition carried by one version. Grouped into a struct so the
/// compare-and-set arguments stay readable instead of a five-string tail.
pub struct NewWorkflowVersion<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub steps_json: &'a str,
    pub parameters_json: &'a str,
}

/// Append a new immutable version. `expected_current_version` is a compare-and-set
/// so two windows editing the same workflow cannot both "win" with a silent
/// last-write-wins overwrite.
pub fn add_version(
    conn: &Connection,
    workflow_ref: &str,
    expected_current_version: i64,
    definition: NewWorkflowVersion<'_>,
    now: i64,
) -> AppResult<WorkflowVersionRecord> {
    let NewWorkflowVersion {
        name,
        description,
        steps_json,
        parameters_json,
    } = definition;
    if name.trim().is_empty() {
        return Err(AppError::invalid_arg("常用流程需要有名称"));
    }
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE workflows SET name=?2, current_version=current_version+1, updated_at=?3
          WHERE workflow_ref=?1 AND current_version=?4 AND archived=0",
        params![workflow_ref, name, now, expected_current_version],
    )?;
    if changed != 1 {
        return Err(AppError::conflict(
            "常用流程已在别处被修改，请重新打开后再保存",
        ));
    }
    let next = expected_current_version + 1;
    tx.execute(
        "INSERT INTO workflow_versions(workflow_ref,version,name,description,steps_json,parameters_json,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            workflow_ref,
            next,
            name,
            description,
            steps_json,
            parameters_json,
            now
        ],
    )?;
    tx.commit()?;
    version(conn, workflow_ref, next)?.ok_or_else(|| AppError::internal("常用流程版本写入后不可读"))
}

/// Header-only rename. Deliberately does not create a version: a running task's
/// snapshot keeps the name it was approved under.
pub fn rename(conn: &Connection, workflow_ref: &str, name: &str, now: i64) -> AppResult<bool> {
    if name.trim().is_empty() {
        return Err(AppError::invalid_arg("常用流程需要有名称"));
    }
    let changed = conn.execute(
        "UPDATE workflows SET name=?2, updated_at=?3 WHERE workflow_ref=?1",
        params![workflow_ref, name, now],
    )?;
    Ok(changed == 1)
}

/// Archiving is the delete semantic: an archived workflow cannot be invoked
/// again, but its versions and instantiations still resolve for history.
pub fn set_archived(
    conn: &Connection,
    workflow_ref: &str,
    archived: bool,
    now: i64,
) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE workflows SET archived=?2, updated_at=?3 WHERE workflow_ref=?1",
        params![workflow_ref, if archived { 1 } else { 0 }, now],
    )?;
    Ok(changed == 1)
}

pub fn header(conn: &Connection, workflow_ref: &str) -> AppResult<Option<WorkflowHeader>> {
    Ok(conn
        .query_row(
            "SELECT workflow_ref,name,current_version,archived,updated_at
               FROM workflows WHERE workflow_ref=?1",
            params![workflow_ref],
            map_header,
        )
        .optional()?)
}

pub fn version(
    conn: &Connection,
    workflow_ref: &str,
    version: i64,
) -> AppResult<Option<WorkflowVersionRecord>> {
    Ok(conn
        .query_row(
            "SELECT workflow_ref,version,name,description,steps_json,parameters_json,created_at
               FROM workflow_versions WHERE workflow_ref=?1 AND version=?2",
            params![workflow_ref, version],
            map_version,
        )
        .optional()?)
}

pub fn current(conn: &Connection, workflow_ref: &str) -> AppResult<Option<WorkflowVersionRecord>> {
    Ok(conn
        .query_row(
            "SELECT v.workflow_ref,v.version,v.name,v.description,v.steps_json,v.parameters_json,v.created_at
               FROM workflow_versions v
               JOIN workflows w ON w.workflow_ref=v.workflow_ref
              WHERE v.workflow_ref=?1 AND v.version=w.current_version",
            params![workflow_ref],
            map_version,
        )
        .optional()?)
}

/// Newest first, optionally filtered by a case-insensitive name substring.
/// Archived workflows are excluded unless asked for: the management list is a
/// picker, and an archived workflow cannot be invoked.
pub fn list(
    conn: &Connection,
    include_archived: bool,
    name_query: Option<&str>,
) -> AppResult<Vec<WorkflowHeader>> {
    let pattern = name_query
        .map(|query| query.trim())
        .filter(|query| !query.is_empty())
        .map(|query| format!("%{}%", escape_like(query)));
    let mut statement = conn.prepare(
        "SELECT workflow_ref,name,current_version,archived,updated_at
           FROM workflows
          WHERE (?1 = 1 OR archived = 0)
            AND (?2 IS NULL OR lower(name) LIKE lower(?2) ESCAPE '\\')
          ORDER BY archived, updated_at DESC, workflow_ref",
    )?;
    let rows = statement.query_map(params![include_archived as i64, pattern], map_header)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn record_instantiation(
    conn: &Connection,
    plan_id: &str,
    workflow_ref: &str,
    version: i64,
    now: i64,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO workflow_instantiations(plan_id,workflow_ref,version,created_at)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(plan_id) DO NOTHING",
        params![plan_id, workflow_ref, version, now],
    )?;
    Ok(())
}

pub fn instantiation(conn: &Connection, plan_id: &str) -> AppResult<Option<(String, i64)>> {
    Ok(conn
        .query_row(
            "SELECT workflow_ref,version FROM workflow_instantiations WHERE plan_id=?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn map_header(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowHeader> {
    Ok(WorkflowHeader {
        workflow_ref: row.get(0)?,
        name: row.get(1)?,
        current_version: row.get(2)?,
        archived: row.get::<_, i64>(3)? != 0,
        updated_at: row.get(4)?,
    })
}

fn map_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowVersionRecord> {
    Ok(WorkflowVersionRecord {
        workflow_ref: row.get(0)?,
        version: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        steps_json: row.get(4)?,
        parameters_json: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn create_fixture(conn: &Connection) -> WorkflowVersionRecord {
        create(
            conn,
            "workflow_aaaaaaaaaaaaaaaa",
            "整理风景",
            "把选中的风景图移进分库",
            r#"[{"capability":"assets.move.prepare"}]"#,
            r#"[{"key":"collectionRef","required":true,"description":"目标分库"}]"#,
            10,
        )
        .unwrap()
    }

    fn definition<'a>(name: &'a str) -> NewWorkflowVersion<'a> {
        NewWorkflowVersion {
            name,
            description: "",
            steps_json: "[]",
            parameters_json: "[]",
        }
    }

    #[test]
    fn structure_is_idempotent_and_versions_are_append_only() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        let first = create_fixture(&conn);
        assert_eq!(first.version, 1);
        assert_eq!(
            header(&conn, &first.workflow_ref)
                .unwrap()
                .unwrap()
                .current_version,
            1
        );

        let second = add_version(
            &conn,
            &first.workflow_ref,
            1,
            NewWorkflowVersion {
                name: "整理风景（改）",
                description: "新的说明",
                steps_json: r#"[{"capability":"assets.move.prepare"}]"#,
                parameters_json: r#"[{"key":"collectionRef","required":true,"description":"目标分库"}]"#,
            },
            11,
        )
        .unwrap();
        assert_eq!(second.version, 2);
        // The old version keeps its own name: renaming through edit must not
        // rewrite what a running task was approved under.
        assert_eq!(
            version(&conn, &first.workflow_ref, 1)
                .unwrap()
                .unwrap()
                .name,
            "整理风景"
        );
        assert_eq!(
            current(&conn, &first.workflow_ref)
                .unwrap()
                .unwrap()
                .version,
            2
        );
    }

    #[test]
    fn a_stale_edit_is_rejected_instead_of_overwriting() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        let first = create_fixture(&conn);
        add_version(&conn, &first.workflow_ref, 1, definition("v2"), 11).unwrap();
        let error =
            add_version(&conn, &first.workflow_ref, 1, definition("v2 again"), 12).unwrap_err();
        assert_eq!(error.code(), "CONFLICT");
        assert_eq!(
            header(&conn, &first.workflow_ref)
                .unwrap()
                .unwrap()
                .current_version,
            2
        );
    }

    #[test]
    fn renaming_only_touches_the_header_and_archiving_keeps_history() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        let first = create_fixture(&conn);

        assert!(rename(&conn, &first.workflow_ref, "换个名字", 20).unwrap());
        assert_eq!(
            version(&conn, &first.workflow_ref, 1)
                .unwrap()
                .unwrap()
                .name,
            "整理风景"
        );
        assert_eq!(
            header(&conn, &first.workflow_ref).unwrap().unwrap().name,
            "换个名字"
        );

        assert!(set_archived(&conn, &first.workflow_ref, true, 21).unwrap());
        assert!(list(&conn, false, None).unwrap().is_empty());
        assert_eq!(list(&conn, true, None).unwrap().len(), 1);
        // Archived workflows cannot gain versions, but their history still reads.
        assert!(add_version(&conn, &first.workflow_ref, 1, definition("v2"), 22).is_err());
        assert!(version(&conn, &first.workflow_ref, 1).unwrap().is_some());
    }

    #[test]
    fn search_matches_names_case_insensitively_and_escapes_wildcards() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        create_fixture(&conn);
        create(
            &conn,
            "workflow_bbbbbbbbbbbbbbbb",
            "100%_literal",
            "",
            "[]",
            "[]",
            30,
        )
        .unwrap();

        assert_eq!(list(&conn, false, Some("风景")).unwrap().len(), 1);
        assert_eq!(list(&conn, false, Some("%")).unwrap().len(), 1);
        assert_eq!(list(&conn, false, Some("_")).unwrap().len(), 1);
        assert!(list(&conn, false, Some("不存在")).unwrap().is_empty());
        assert_eq!(list(&conn, false, Some("  ")).unwrap().len(), 2);
    }

    #[test]
    fn instantiations_survive_renames_and_point_at_one_version() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        let first = create_fixture(&conn);
        add_version(&conn, &first.workflow_ref, 1, definition("v2"), 11).unwrap();

        record_instantiation(&conn, "plan_aaaaaaaaaaaaaaaa", &first.workflow_ref, 1, 12).unwrap();
        // Replay keeps the original version rather than rewriting it.
        record_instantiation(&conn, "plan_aaaaaaaaaaaaaaaa", &first.workflow_ref, 2, 13).unwrap();
        assert_eq!(
            instantiation(&conn, "plan_aaaaaaaaaaaaaaaa").unwrap(),
            Some((first.workflow_ref.clone(), 1))
        );
        assert!(version(&conn, &first.workflow_ref, 1).unwrap().is_some());
    }
}
