use crate::{fail, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: i64,
    pub location: PathBuf,
    pub digest: String,
    pub trusted_receipt: bool,
    pub tags: String,
    pub rating: i64,
}

pub struct LibraryStore {
    conn: Connection,
    path: PathBuf,
}
#[derive(Debug)]
pub struct CleanupPlan {
    pub source: PathBuf,
    pub target: PathBuf,
    pub epoch: i64,
    pub records: Vec<AssetRecord>,
    pub state: String,
}
#[derive(Debug, PartialEq, Eq)]
pub struct MaintenanceHistory {
    pub epoch: i64,
    pub source: PathBuf,
    pub target: PathBuf,
    pub records: Vec<AssetRecord>,
    pub items: Vec<CleanupReceipt>,
    pub state: String,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct CleanupReceipt {
    pub asset: i64,
    pub state: String,
}
#[derive(Debug)]
pub struct ExternalUnlinkPlan {
    pub id: i64,
    pub library_id: String,
    pub root: PathBuf,
    pub epoch: i64,
    pub record: AssetRecord,
    pub state: String,
}
#[derive(Debug)]
pub struct ExternalReturnPlan {
    pub library_id: String,
    pub root: PathBuf,
    pub epoch: i64,
    pub record: AssetRecord,
    pub relative: PathBuf,
    pub identity: String,
    pub state: String,
}
impl LibraryStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS library(id TEXT PRIMARY KEY,root TEXT NOT NULL,epoch INTEGER NOT NULL);
            CREATE UNIQUE INDEX IF NOT EXISTS library_singleton ON library((1));
            CREATE TABLE IF NOT EXISTS records(id INTEGER PRIMARY KEY,payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS relocation(id INTEGER PRIMARY KEY CHECK(id=1),source TEXT NOT NULL,target TEXT NOT NULL,epoch INTEGER NOT NULL,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS cleanup(id INTEGER PRIMARY KEY CHECK(id=1),source TEXT NOT NULL,target TEXT NOT NULL,epoch INTEGER NOT NULL,records TEXT NOT NULL,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS cleanup_items(asset INTEGER PRIMARY KEY,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS maintenance_history(epoch INTEGER PRIMARY KEY,source TEXT NOT NULL,target TEXT NOT NULL,records TEXT NOT NULL,items TEXT NOT NULL,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS external_unlinks(id INTEGER PRIMARY KEY AUTOINCREMENT,asset INTEGER NOT NULL,library_id TEXT NOT NULL,root TEXT NOT NULL,epoch INTEGER NOT NULL,payload TEXT NOT NULL,state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS external_returns(id INTEGER PRIMARY KEY AUTOINCREMENT,asset INTEGER NOT NULL,library_id TEXT NOT NULL,root TEXT NOT NULL,epoch INTEGER NOT NULL,payload TEXT NOT NULL,relative TEXT NOT NULL,identity TEXT NOT NULL,state TEXT NOT NULL);")?;
        Ok(Self {
            conn,
            path: path.canonicalize()?,
        })
    }
    pub fn maintenance_guard_fixture(&self) -> Result<crate::maintenance_guard::MaintenanceGuard> {
        crate::maintenance_guard::acquire(&self.path)
    }
    pub fn initialize(&self, id: &str, root: &Path) -> Result<()> {
        let text = root.to_str().ok_or("NON_UTF8_PATH")?;
        self.conn.execute(
            "INSERT INTO library(id,root,epoch) VALUES(?,?,1)",
            params![id, text],
        )?;
        Ok(())
    }
    pub fn locator(&self) -> Result<(String, PathBuf, i64)> {
        Ok(self
            .conn
            .query_row("SELECT id,root,epoch FROM library", [], |r| {
                Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?))
            })?)
    }
    // Fixture-only: trusted_receipt is assigned by the host that created the test asset.
    pub fn seed(&self, record: &AssetRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO records(id,payload) VALUES(?,?)",
            params![record.id, serde_json::to_string(record)?],
        )?;
        Ok(())
    }
    pub fn records(&self) -> Result<Vec<AssetRecord>> {
        let mut statement = self
            .conn
            .prepare("SELECT payload FROM records ORDER BY id")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }
    pub(crate) fn start_copy(&self, source: &Path, target: &Path, epoch: i64) -> Result<()> {
        let source = source.to_str().ok_or("NON_UTF8_PATH")?;
        let target = target.to_str().ok_or("NON_UTF8_PATH")?;
        self.conn.execute("INSERT INTO relocation(id,source,target,epoch,state) VALUES(1,?,?,?,'copying') ON CONFLICT(id) DO NOTHING",params![source,target,epoch])?;
        let saved: (String, String, i64, String) = self.conn.query_row(
            "SELECT source,target,epoch,state FROM relocation WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if saved
            != (
                source.to_owned(),
                target.to_owned(),
                epoch,
                "copying".to_owned(),
            )
        {
            return fail("MAINTENANCE_CONFLICT");
        }
        Ok(())
    }
    pub(crate) fn has_copy(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM relocation WHERE id=1)",
            [],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn copy_switched_to(&self, target: &Path, epoch: i64) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM relocation WHERE id=1 AND target=? AND state='switched' AND epoch=?)",params![target.to_str().ok_or("NON_UTF8_PATH")?,epoch-1],|r|r.get(0))?)
    }
    pub(crate) fn switch(&mut self, target: &Path, epoch: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE library SET root=?,epoch=epoch+1 WHERE epoch=?",
            params![target.to_str().ok_or("NON_UTF8_PATH")?, epoch],
        )? != 1
        {
            return fail("LIBRARY_EPOCH_CHANGED");
        }
        tx.execute("UPDATE relocation SET state='switched' WHERE id=1", [])?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn switched_source(&self) -> Result<(PathBuf, PathBuf, i64)> {
        Ok(self.conn.query_row("SELECT r.source,r.target,l.epoch FROM relocation r JOIN library l ON l.root=r.target AND l.epoch=r.epoch+1 WHERE r.id=1 AND r.state='switched'", [], |r| Ok((PathBuf::from(r.get::<_,String>(0)?),PathBuf::from(r.get::<_,String>(1)?),r.get(2)?)))?)
    }
    pub(crate) fn create_cleanup(
        &self,
        source: &Path,
        target: &Path,
        epoch: i64,
        records: &[AssetRecord],
    ) -> Result<()> {
        if self.conn.execute("INSERT INTO cleanup(id,source,target,epoch,records,state) SELECT 1,?,?,?,?,'prepared' WHERE EXISTS(SELECT 1 FROM library WHERE root=? AND epoch=?)", params![source.to_str().ok_or("NON_UTF8_PATH")?,target.to_str().ok_or("NON_UTF8_PATH")?,epoch,serde_json::to_string(records)?,target.to_str().ok_or("NON_UTF8_PATH")?,epoch])? != 1 { return fail("LIBRARY_EPOCH_CHANGED"); }
        Ok(())
    }
    pub(crate) fn cleanup_plan(&self) -> Result<CleanupPlan> {
        let (source, target, epoch, records, state): (String, String, i64, String, String) =
            self.conn.query_row(
                "SELECT source,target,epoch,records,state FROM cleanup WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        Ok(CleanupPlan {
            source: source.into(),
            target: target.into(),
            epoch,
            records: serde_json::from_str(&records)?,
            state,
        })
    }
    // Only called by the trusted maintenance fixture after reviewing this fixed plan.
    pub fn approve_cleanup_fixture(&self, expected_epoch: i64) -> Result<()> {
        let _maintenance = self.maintenance_guard_fixture()?;
        if self.conn.execute("UPDATE cleanup SET state='approved' WHERE id=1 AND state='prepared' AND epoch=? AND EXISTS(SELECT 1 FROM library WHERE root=cleanup.target AND epoch=cleanup.epoch)",[expected_epoch])? != 1 { return fail("CLEANUP_REVIEW_REQUIRED"); }
        Ok(())
    }
    pub fn retain_source_fixture(&self, expected_epoch: i64) -> Result<()> {
        let _maintenance = self.maintenance_guard_fixture()?;
        if self.conn.execute("UPDATE cleanup SET state='retained' WHERE id=1 AND state IN ('prepared','approved') AND epoch=?
            AND EXISTS(SELECT 1 FROM library WHERE root=cleanup.target AND epoch=cleanup.epoch)
            AND NOT EXISTS(SELECT 1 FROM cleanup_items)",[expected_epoch])? != 1 {
            return fail("MAINTENANCE_UNFINISHED");
        }
        Ok(())
    }
    // Explicit trusted action, not automatic housekeeping during another request.
    // Archive and retire share a transaction, preserving unfinished work on error.
    pub fn archive_finished_fixture(&mut self, expected_epoch: i64) -> Result<()> {
        let _maintenance = self.maintenance_guard_fixture()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute("INSERT INTO maintenance_history(epoch,source,target,records,items,state)
            SELECT c.epoch,c.source,c.target,c.records,COALESCE((SELECT json_group_array(json_object('asset',asset,'state',state)) FROM cleanup_items),'[]'),c.state
            FROM cleanup c JOIN relocation r ON r.id=c.id AND r.source=c.source AND r.target=c.target AND r.epoch+1=c.epoch
            JOIN library l ON l.root=c.target AND l.epoch=c.epoch
            WHERE c.id=1 AND c.state IN ('complete','retained') AND r.state='switched' AND c.epoch=?
            AND NOT EXISTS(SELECT 1 FROM cleanup_items WHERE state!='complete')",[expected_epoch])?;
        if inserted != 1 {
            return fail("MAINTENANCE_UNFINISHED");
        }
        tx.execute("DELETE FROM cleanup_items", [])?;
        tx.execute("DELETE FROM cleanup WHERE id=1", [])?;
        tx.execute("DELETE FROM relocation WHERE id=1", [])?;
        tx.commit()?;
        Ok(())
    }
    pub fn history_fixture(&self) -> Result<Vec<MaintenanceHistory>> {
        let mut statement = self.conn.prepare(
            "SELECT epoch,source,target,records,items,state FROM maintenance_history ORDER BY epoch",
        )?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut history = Vec::new();
        for row in rows {
            let (epoch, source, target, records, items, state) = row?;
            history.push(MaintenanceHistory {
                epoch,
                source: source.into(),
                target: target.into(),
                records: serde_json::from_str(&records)?,
                items: serde_json::from_str(&items)?,
                state,
            });
        }
        Ok(history)
    }
    pub(crate) fn begin_cleanup(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("UPDATE cleanup SET state='running' WHERE id=1 AND state IN ('approved','running') AND EXISTS(SELECT 1 FROM library WHERE root=cleanup.target AND epoch=cleanup.epoch)",[])? != 1 { return fail("CLEANUP_REVIEW_REQUIRED"); }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn cleanup_item(&self, id: i64) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT COALESCE((SELECT state FROM cleanup_items WHERE asset=?),'pending')",
            [id],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn cleanup_item_state(&self, id: i64, state: &str) -> Result<()> {
        self.conn.execute("INSERT INTO cleanup_items(asset,state) VALUES(?,?) ON CONFLICT(asset) DO UPDATE SET state=excluded.state",params![id,state])?;
        Ok(())
    }
    pub(crate) fn finish_cleanup(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE cleanup SET state='complete' WHERE id=1 AND state='running'",
            [],
        )?;
        Ok(())
    }
    pub(crate) fn save_external_unlink(&self, record: &AssetRecord) -> Result<i64> {
        let payload = serde_json::to_string(record)?;
        if self.conn.execute(
            "INSERT INTO external_unlinks(asset,library_id,root,epoch,payload,state)
            SELECT r.id,l.id,l.root,l.epoch,r.payload,'prepared' FROM records r CROSS JOIN library l
            WHERE r.id=? AND r.payload=? AND NOT EXISTS(SELECT 1 FROM relocation)",
            params![record.id, payload],
        )? != 1
        {
            return fail("LEGACY_RECORD_CHANGED");
        }
        Ok(self.conn.last_insert_rowid())
    }
    pub fn external_unlink_fixture(&self, id: i64) -> Result<ExternalUnlinkPlan> {
        let (library_id, root, epoch, payload, state): (String, String, i64, String, String) =
            self.conn.query_row(
                "SELECT library_id,root,epoch,payload,state FROM external_unlinks WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        Ok(ExternalUnlinkPlan {
            id,
            library_id,
            root: root.into(),
            epoch,
            record: serde_json::from_str(&payload)?,
            state,
        })
    }
    pub fn approve_external_unlink_fixture(&self, id: i64, expected_epoch: i64) -> Result<()> {
        let _maintenance = self.maintenance_guard_fixture()?;
        if self.conn.execute("UPDATE external_unlinks SET state='approved' WHERE id=? AND epoch=? AND state='prepared'
            AND EXISTS(SELECT 1 FROM library l WHERE l.id=external_unlinks.library_id AND l.root=external_unlinks.root AND l.epoch=external_unlinks.epoch)
            AND EXISTS(SELECT 1 FROM records r WHERE r.id=external_unlinks.asset AND r.payload=external_unlinks.payload)
            AND NOT EXISTS(SELECT 1 FROM relocation)",params![id,expected_epoch])? != 1 {return fail("LEGACY_REVIEW_REQUIRED");}
        Ok(())
    }
    pub(crate) fn finish_external_unlink(&mut self, id: i64, expected_epoch: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let completed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM external_unlinks WHERE id=? AND epoch=? AND state='complete')",params![id,expected_epoch],|r|r.get(0))?;
        if completed {
            tx.commit()?;
            return Ok(());
        }
        if tx.execute("UPDATE external_unlinks SET state='complete' WHERE id=? AND epoch=? AND state='approved'
            AND EXISTS(SELECT 1 FROM library l WHERE l.id=external_unlinks.library_id AND l.root=external_unlinks.root AND l.epoch=external_unlinks.epoch)
            AND EXISTS(SELECT 1 FROM records r WHERE r.id=external_unlinks.asset AND r.payload=external_unlinks.payload)
            AND NOT EXISTS(SELECT 1 FROM relocation)",params![id,expected_epoch])? != 1 {return fail("LEGACY_REVIEW_REQUIRED");}
        if tx.execute("DELETE FROM records WHERE EXISTS(SELECT 1 FROM external_unlinks p WHERE p.id=? AND p.state='complete' AND p.asset=records.id AND p.payload=records.payload)",[id])? != 1 {return fail("LEGACY_RECORD_CHANGED");}
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn save_external_return(
        &self,
        record: &AssetRecord,
        relative: &Path,
        identity: &str,
    ) -> Result<i64> {
        let payload = serde_json::to_string(record)?;
        if self.conn.execute("INSERT INTO external_returns(asset,library_id,root,epoch,payload,relative,identity,state)
            SELECT r.id,l.id,l.root,l.epoch,r.payload,?,?,'prepared' FROM records r CROSS JOIN library l
            WHERE r.id=? AND r.payload=? AND NOT EXISTS(SELECT 1 FROM relocation)",params![relative.to_str().ok_or("NON_UTF8_PATH")?,identity,record.id,payload])?!=1 {return fail("LEGACY_RECORD_CHANGED");}
        Ok(self.conn.last_insert_rowid())
    }
    pub fn external_return_fixture(&self, id: i64) -> Result<ExternalReturnPlan> {
        let row=self.conn.query_row("SELECT library_id,root,epoch,payload,relative,identity,state FROM external_returns WHERE id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?)))?;
        Ok(ExternalReturnPlan {
            library_id: row.0,
            root: row.1.into(),
            epoch: row.2,
            record: serde_json::from_str(&row.3)?,
            relative: row.4.into(),
            identity: row.5,
            state: row.6,
        })
    }
    pub fn approve_external_return_fixture(&self, id: i64, epoch: i64) -> Result<()> {
        let _maintenance = self.maintenance_guard_fixture()?;
        if self.conn.execute("UPDATE external_returns SET state='approved' WHERE id=? AND epoch=? AND state='prepared'
            AND EXISTS(SELECT 1 FROM library l WHERE l.id=external_returns.library_id AND l.root=external_returns.root AND l.epoch=external_returns.epoch)
            AND EXISTS(SELECT 1 FROM records r WHERE r.id=external_returns.asset AND r.payload=external_returns.payload)
            AND NOT EXISTS(SELECT 1 FROM relocation)",params![id,epoch])?!=1 {return fail("LEGACY_REVIEW_REQUIRED");}
        Ok(())
    }
    pub(crate) fn begin_external_return(&self, id: i64, epoch: i64) -> Result<()> {
        if self.conn.execute("UPDATE external_returns SET state='copying' WHERE id=? AND epoch=? AND state IN ('approved','copying')
            AND EXISTS(SELECT 1 FROM library l WHERE l.id=external_returns.library_id AND l.root=external_returns.root AND l.epoch=external_returns.epoch)
            AND EXISTS(SELECT 1 FROM records r WHERE r.id=external_returns.asset AND r.payload=external_returns.payload)
            AND NOT EXISTS(SELECT 1 FROM relocation)",params![id,epoch])?!=1 {return fail("LEGACY_REVIEW_REQUIRED");}
        Ok(())
    }
    pub(crate) fn finish_external_return(&mut self, id: i64, epoch: i64) -> Result<()> {
        let plan = self.external_return_fixture(id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replacement = AssetRecord {
            location: plan.relative.clone(),
            trusted_receipt: true,
            ..plan.record.clone()
        };
        if tx.execute("UPDATE external_returns SET state='complete' WHERE id=? AND epoch=? AND state='copying' AND payload=? AND relative=? AND identity=?
            AND EXISTS(SELECT 1 FROM library l WHERE l.id=external_returns.library_id AND l.root=external_returns.root AND l.epoch=external_returns.epoch)
            AND EXISTS(SELECT 1 FROM records r WHERE r.id=external_returns.asset AND r.payload=external_returns.payload)
            AND NOT EXISTS(SELECT 1 FROM relocation)",params![id,epoch,serde_json::to_string(&plan.record)?,plan.relative.to_str().ok_or("NON_UTF8_PATH")?,plan.identity])?!=1 {return fail("LEGACY_REVIEW_REQUIRED");}
        if tx.execute(
            "UPDATE records SET payload=? WHERE id=? AND payload=?",
            params![
                serde_json::to_string(&replacement)?,
                plan.record.id,
                serde_json::to_string(&plan.record)?
            ],
        )? != 1
        {
            return fail("LEGACY_RECORD_CHANGED");
        }
        tx.commit()?;
        Ok(())
    }
}
