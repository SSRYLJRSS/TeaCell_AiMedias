use crate::{
    fail,
    workflow::{Definition, PreparedWorkflow},
    Result,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;

// A separate fixture DB. Production migrations are not touched by this experiment.
pub struct WorkflowStore {
    conn: Connection,
}

impl WorkflowStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS workflows(id TEXT PRIMARY KEY,enabled INTEGER NOT NULL DEFAULT 1);
          CREATE TABLE IF NOT EXISTS workflow_versions(workflow_id TEXT NOT NULL REFERENCES workflows(id),version INTEGER NOT NULL,payload TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(workflow_id,version));
          CREATE TABLE IF NOT EXISTS workflow_plans(id INTEGER PRIMARY KEY,workflow_id TEXT NOT NULL,version INTEGER NOT NULL,payload TEXT NOT NULL,state TEXT NOT NULL CHECK(state='awaiting_confirmation'),FOREIGN KEY(workflow_id,version) REFERENCES workflow_versions(workflow_id,version));")?;
        Ok(Self { conn })
    }
    pub(crate) fn save(&mut self, definition: &Definition, expected_version: u32) -> Result<()> {
        definition.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: u32 = tx.query_row(
            "SELECT COALESCE(MAX(version),0) FROM workflow_versions WHERE workflow_id=?",
            [&definition.id],
            |r| r.get(0),
        )?;
        if current != expected_version
            || definition.version != current.checked_add(1).ok_or("VERSION_LIMIT")?
        {
            return fail("VERSION_CONFLICT");
        }
        tx.execute(
            "INSERT INTO workflows(id) VALUES(?) ON CONFLICT(id) DO NOTHING",
            [&definition.id],
        )?;
        tx.execute(
            "INSERT INTO workflow_versions(workflow_id,version,payload,hash) VALUES(?,?,?,?)",
            params![
                definition.id,
                definition.version,
                serde_json::to_string(definition)?,
                definition.hash()?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn load(&self, id: &str, version: u32) -> Result<Definition> {
        let payload: Option<String>=self.conn.query_row("SELECT v.payload FROM workflow_versions v JOIN workflows w ON w.id=v.workflow_id WHERE w.id=? AND v.version=? AND w.enabled=1",params![id,version],|r|r.get(0)).optional()?;
        let definition: Definition = serde_json::from_str(&payload.ok_or("WORKFLOW_UNAVAILABLE")?)?;
        definition.validate()?;
        Ok(definition)
    }
    pub(crate) fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        if self.conn.execute(
            "UPDATE workflows SET enabled=? WHERE id=?",
            params![enabled, id],
        )? != 1
        {
            return fail("WORKFLOW_UNAVAILABLE");
        }
        Ok(())
    }
    pub(crate) fn insert_plan(&self, plan: &mut PreparedWorkflow) -> Result<()> {
        // Recheck availability in the same statement that creates the plan: hashing
        // inputs happens outside SQLite and another connection may disable the flow.
        let inserted = self.conn.execute("INSERT INTO workflow_plans(workflow_id,version,payload,state) SELECT v.workflow_id,v.version,?,'awaiting_confirmation' FROM workflow_versions v JOIN workflows w ON w.id=v.workflow_id WHERE v.workflow_id=? AND v.version=? AND v.hash=? AND w.enabled=1",params![serde_json::to_string(plan)?,plan.workflow_id,plan.workflow_version,plan.workflow_hash])?;
        if inserted != 1 {
            return fail("WORKFLOW_UNAVAILABLE");
        }
        plan.plan_id = self.conn.last_insert_rowid();
        Ok(())
    }
    pub fn plan(&self, id: i64) -> Result<PreparedWorkflow> {
        let payload: String =
            self.conn
                .query_row("SELECT payload FROM workflow_plans WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
        let mut plan: PreparedWorkflow = serde_json::from_str(&payload)?;
        plan.plan_id = id;
        Ok(plan)
    }
    pub fn plan_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM workflow_plans", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{workflow, Caller};

    #[test]
    fn disable_after_preparation_cannot_publish_another_plan() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("flows.sqlite");
        std::fs::write(temp.path().join("input.jpg"), b"fixture").unwrap();
        let mut store = WorkflowStore::open(&db).unwrap();
        let definition: Definition = serde_json::from_value(serde_json::json!({
            "schema":1,"capability_schema":1,"id":"flow","version":1,
            "name":"流程","defaults":{"collection":"素材","tagging_instructions":"","rename":false},
            "steps":[{"kind":"import"}]
        }))
        .unwrap();
        workflow::save(&mut store, &definition, 0, Caller::TrustedGui).unwrap();
        let grant = workflow::DirectoryGrant::fixture(Caller::TrustedGui, temp.path(), 1).unwrap();
        let mut plan = workflow::instantiate(
            &store,
            "flow",
            1,
            definition.defaults,
            &["input.jpg".into()],
            &grant,
            1,
        )
        .unwrap();
        let other = WorkflowStore::open(&db).unwrap();
        workflow::set_enabled(&other, "flow", false, Caller::TrustedGui).unwrap();
        plan.plan_id = 0;
        assert!(store.insert_plan(&mut plan).is_err());
        assert_eq!(plan.plan_id, 0);
        assert_eq!(store.plan_count().unwrap(), 1);
    }
}
