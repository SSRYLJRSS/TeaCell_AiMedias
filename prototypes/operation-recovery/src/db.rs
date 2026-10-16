use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::{fail, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub source: PathBuf,
    pub digest: String,
}

pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS plans(id INTEGER PRIMARY KEY, payload TEXT NOT NULL, approved INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS operations(id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL UNIQUE REFERENCES plans(id), request_key TEXT NOT NULL UNIQUE, cancelled INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS items(operation_id INTEGER NOT NULL REFERENCES operations(id), step INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending', PRIMARY KEY(operation_id,step));
             CREATE TABLE IF NOT EXISTS assets(operation_id INTEGER NOT NULL, step INTEGER NOT NULL, digest TEXT NOT NULL, PRIMARY KEY(operation_id,step));
             CREATE TABLE IF NOT EXISTS batches(operation_id INTEGER NOT NULL UNIQUE REFERENCES operations(id), id INTEGER PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS external_attempts(operation_id INTEGER PRIMARY KEY REFERENCES operations(id), state TEXT NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    pub(crate) fn prepare(&self, inputs: &[Input]) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO plans(payload) VALUES(?)",
            [serde_json::to_string(inputs)?],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // Caller is supplied by the trusted adapter, never deserialized from model JSON.
    pub fn approve(&self, plan: i64, caller: crate::Caller) -> Result<()> {
        if caller != crate::Caller::TrustedGui {
            return fail("APPROVAL_REQUIRED");
        }
        if self.conn.execute(
            "UPDATE plans SET approved=1 WHERE id=? AND approved=0",
            [plan],
        )? != 1
        {
            return fail("PLAN_NOT_FOUND");
        }
        Ok(())
    }

    pub fn submit(&mut self, plan: i64, key: &str) -> Result<i64> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(i64, i64)> = tx
            .query_row(
                "SELECT id,plan_id FROM operations WHERE request_key=?",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, original)) = existing {
            if original != plan {
                return fail("IDEMPOTENCY_CONFLICT");
            }
            tx.commit()?;
            return Ok(id);
        }
        let approved: i64 = tx.query_row("SELECT approved FROM plans WHERE id=?", [plan], |r| {
            r.get(0)
        })?;
        if approved != 1 {
            return fail("APPROVAL_REQUIRED");
        }
        tx.execute(
            "INSERT INTO operations(plan_id,request_key) VALUES(?,?)",
            params![plan, key],
        )?;
        let id = tx.last_insert_rowid();
        let payload: String =
            tx.query_row("SELECT payload FROM plans WHERE id=?", [plan], |r| r.get(0))?;
        let inputs: Vec<Input> = serde_json::from_str(&payload)?;
        for step in 0..inputs.len() {
            tx.execute(
                "INSERT INTO items(operation_id,step) VALUES(?,?)",
                params![id, step as i64],
            )?;
        }
        tx.execute("UPDATE plans SET approved=2 WHERE id=?", [plan])?;
        tx.commit()?;
        Ok(id)
    }

    pub(crate) fn inputs(&self, operation: i64) -> Result<Vec<Input>> {
        let payload: String = self.conn.query_row("SELECT payload FROM plans JOIN operations ON plans.id=operations.plan_id WHERE operations.id=?", [operation], |r| r.get(0))?;
        Ok(serde_json::from_str(&payload)?)
    }

    pub(crate) fn state(&self, operation: i64, step: usize) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT state FROM items WHERE operation_id=? AND step=?",
            params![operation, step as i64],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn staged(&self, operation: i64, step: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET state='staged' WHERE operation_id=? AND step=?",
            params![operation, step as i64],
        )?;
        Ok(())
    }

    pub(crate) fn register(&mut self, operation: i64, step: usize, digest: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO assets(operation_id,step,digest) VALUES(?,?,?)",
            params![operation, step as i64, digest],
        )?;
        tx.execute(
            "UPDATE items SET state='registered' WHERE operation_id=? AND step=?",
            params![operation, step as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn cancel(&self, operation: i64) -> Result<()> {
        self.set_cancel(operation, true)
    }
    pub fn continue_after_review(&self, operation: i64) -> Result<()> {
        self.set_cancel(operation, false)
    }
    fn set_cancel(&self, operation: i64, value: bool) -> Result<()> {
        if self.conn.execute(
            "UPDATE operations SET cancelled=? WHERE id=?",
            params![value, operation],
        )? != 1
        {
            return fail("OPERATION_NOT_FOUND");
        }
        Ok(())
    }
    pub(crate) fn cancelled(&self, operation: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT cancelled FROM operations WHERE id=?",
            [operation],
            |r| r.get(0),
        )?)
    }
    pub fn batch(&self, operation: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO batches(operation_id) VALUES(?) ON CONFLICT(operation_id) DO NOTHING",
            [operation],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM batches WHERE operation_id=?",
            [operation],
            |r| r.get(0),
        )?)
    }
    pub fn counts(&self) -> Result<(i64, i64, i64)> {
        Ok((
            self.conn
                .query_row("SELECT count(*) FROM operations", [], |r| r.get(0))?,
            self.conn
                .query_row("SELECT count(*) FROM assets", [], |r| r.get(0))?,
            self.conn
                .query_row("SELECT count(*) FROM batches", [], |r| r.get(0))?,
        ))
    }

    pub(crate) fn completed_count(&self, operation: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM assets WHERE operation_id=?",
            [operation],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn begin_external_once(&self, operation: i64) -> Result<bool> {
        Ok(self.conn.execute("INSERT INTO external_attempts(operation_id,state) VALUES(?,'outcome_unknown') ON CONFLICT(operation_id) DO NOTHING",[operation])?==1)
    }
    pub(crate) fn confirm_external(&self, operation: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE external_attempts SET state='confirmed' WHERE operation_id=?",
            [operation],
        )?;
        Ok(())
    }
    pub fn external_state(&self, operation: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT state FROM external_attempts WHERE operation_id=?",
                [operation],
                |r| r.get(0),
            )
            .optional()?)
    }
}
