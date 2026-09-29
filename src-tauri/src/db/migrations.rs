//! 建表迁移：按 PRAGMA user_version 版本推进
//! v1 = 架构 v1.3 §1.4 全量 schema（含 fts_content 中间表 + 9 个触发器）

use rusqlite::{Connection, OptionalExtension};

use crate::error::AppResult;

const SCHEMA_V1: &str = r#"
-- 素材表
CREATE TABLE assets (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  file_path     TEXT    NOT NULL UNIQUE,
  file_name     TEXT    NOT NULL,
  file_ext      TEXT    NOT NULL,
  file_size     INTEGER NOT NULL,
  mime_type     TEXT    NOT NULL,
  width         INTEGER,
  height        INTEGER,
  duration_ms   INTEGER,
  video_codec   TEXT,
  audio_codec   TEXT,
  taken_at      INTEGER,
  created_at    INTEGER NOT NULL,
  modified_at   INTEGER NOT NULL,
  hash          TEXT,
  placeholder_path TEXT,
  hd_thumbnail_path TEXT
);
CREATE INDEX idx_assets_mime    ON assets(mime_type);
CREATE INDEX idx_assets_created ON assets(created_at);

-- 标签表（父子层级，方案B）
CREATE TABLE tags (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT    NOT NULL,
  parent_id   INTEGER REFERENCES tags(id) ON DELETE CASCADE,
  is_preset   INTEGER NOT NULL DEFAULT 0,
  sort_order  INTEGER NOT NULL DEFAULT 0,
  UNIQUE(parent_id, name)
);
CREATE INDEX idx_tags_parent ON tags(parent_id);

-- 素材-标签关联
CREATE TABLE asset_tags (
  asset_id   INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  tag_id     INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
  source     TEXT    NOT NULL DEFAULT 'manual',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (asset_id, tag_id)
);
CREATE INDEX idx_asset_tags_tag   ON asset_tags(tag_id);
CREATE INDEX idx_asset_tags_asset ON asset_tags(asset_id);

-- FTS5 方案2：独立 fts_content 中间表（外部内容表）
CREATE TABLE fts_content (
  asset_id   INTEGER PRIMARY KEY,
  file_name  TEXT NOT NULL,
  tag_names  TEXT
);
CREATE VIRTUAL TABLE assets_fts USING fts5(
  file_name,
  tag_names,
  content='fts_content',
  content_rowid='asset_id',
  tokenize='unicode61'
);

-- 第一层触发器：FTS 索引维护（挂 fts_content，官方 old/new 三段式；
-- delete 必须提供原始插入值，否则旧 token 残留产生幻影命中——已实测）
CREATE TRIGGER trg_fc_ai AFTER INSERT ON fts_content BEGIN
  INSERT INTO assets_fts(rowid, file_name, tag_names)
    VALUES (new.asset_id, new.file_name, new.tag_names);
END;
CREATE TRIGGER trg_fc_ad AFTER DELETE ON fts_content BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, file_name, tag_names)
    VALUES ('delete', old.asset_id, old.file_name, old.tag_names);
END;
CREATE TRIGGER trg_fc_au AFTER UPDATE ON fts_content BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, file_name, tag_names)
    VALUES ('delete', old.asset_id, old.file_name, old.tag_names);
  INSERT INTO assets_fts(rowid, file_name, tag_names)
    VALUES (new.asset_id, new.file_name, new.tag_names);
END;

-- 第二层触发器：业务表只维护 fts_content
CREATE TRIGGER trg_assets_ai AFTER INSERT ON assets BEGIN
  INSERT INTO fts_content(asset_id, file_name, tag_names)
    VALUES (new.id, cjk_bigram(new.file_name), '');
END;
CREATE TRIGGER trg_assets_ad AFTER DELETE ON assets BEGIN
  DELETE FROM fts_content WHERE asset_id = old.id;
END;
CREATE TRIGGER trg_assets_au AFTER UPDATE OF file_name ON assets BEGIN
  UPDATE fts_content SET file_name = cjk_bigram(new.file_name) WHERE asset_id = new.id;
END;
-- COALESCE 必须包在 cjk_bigram 参数内（无标签时 group_concat 为 NULL，直传报错——已实测）
CREATE TRIGGER trg_at_ai AFTER INSERT ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(t.name, ' '), ''))
      FROM asset_tags at JOIN tags t ON t.id = at.tag_id
     WHERE at.asset_id = new.asset_id
  ) WHERE asset_id = new.asset_id;
END;
CREATE TRIGGER trg_at_ad AFTER DELETE ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(t.name, ' '), ''))
      FROM asset_tags at JOIN tags t ON t.id = at.tag_id
     WHERE at.asset_id = old.asset_id
  ) WHERE asset_id = old.asset_id;
END;
CREATE TRIGGER trg_tags_au AFTER UPDATE OF name ON tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(t2.name, ' '), ''))
      FROM asset_tags at JOIN tags t2 ON t2.id = at.tag_id
     WHERE at.asset_id = fts_content.asset_id
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.id);
END;

-- AI 打标批次
CREATE TABLE ai_batches (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  status     TEXT NOT NULL DEFAULT 'pending',
  mode       TEXT NOT NULL,
  total      INTEGER NOT NULL DEFAULT 0,
  processed  INTEGER NOT NULL DEFAULT 0,
  confirmed  INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

-- AI 建议（确认才写 asset_tags）
CREATE TABLE ai_suggestions (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  batch_id       INTEGER NOT NULL REFERENCES ai_batches(id) ON DELETE CASCADE,
  asset_id       INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  suggested_tags TEXT NOT NULL,
  status         TEXT NOT NULL DEFAULT 'pending',
  confirmed_tags TEXT,
  created_at     INTEGER NOT NULL
);
CREATE INDEX idx_ai_sugg_batch ON ai_suggestions(batch_id);

-- 设置（键值对）
CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 网盘账号绑定
CREATE TABLE cloud_accounts (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  provider      TEXT NOT NULL,
  name          TEXT,
  access_token  TEXT,
  refresh_token TEXT,
  cookie        TEXT,
  expires_at    INTEGER,
  created_at    INTEGER NOT NULL
);

-- 导出任务
CREATE TABLE export_tasks (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  target     TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending',
  total      INTEGER NOT NULL DEFAULT 0,
  done       INTEGER NOT NULL DEFAULT 0,
  dest_dir   TEXT,
  account_id INTEGER,
  share_url  TEXT,
  error      TEXT,
  created_at INTEGER NOT NULL
);
"#;

/// v2：EXIF 元信息列（PRD 5.5，打标工作台 2.0）
/// B37：逐列定义，配合 migrate_v2 容错（PRAGMA table_info 检查再 ALTER）
const SCHEMA_V2_COLUMNS: &[(&str, &str)] = &[
    ("camera", "TEXT"),
    ("lens", "TEXT"),
    ("iso", "INTEGER"),
    ("aperture", "REAL"),
    ("shutter", "TEXT"),
    ("focal", "REAL"),
];

/// B37：逐列检查再 ALTER，幂等可重入
/// SQLite ALTER TABLE ADD COLUMN 不支持 IF NOT EXISTS 语法，
/// 中途崩溃（部分列已加但 user_version 未提交）重启后重跑不会 panic。
fn migrate_v2(conn: &Connection) -> AppResult<()> {
    let existing: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(assets)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?; // col 1 = name
        rows.filter_map(|r| r.ok()).collect()
    };
    for (col, ty) in SCHEMA_V2_COLUMNS {
        if !existing.contains(*col) {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {col} {ty}"), [])?;
        }
    }
    Ok(())
}

/// v3：BUG-B 写入侧根治（cjk_bigram 在 CJK↔非CJK 边界插空格）+ BUG-D 写入侧加固
/// （tag 触发器 group_concat 加 ORDER BY t.sort_order, t.id）。
/// 旧 cjk_bigram 产物不可逆，必须 DELETE fts_content 后回源重算 + FTS rebuild。
/// 幂等可重入：先完成重建（DROP 触发器→回源重算→rebuild）再 set user_version=3；
/// 中途崩溃（user_version 未提交）重启后重跑无副作用——DELETE+回源重算+rebuild 天然幂等，
/// 且 'rebuild' 会先清空 assets_fts 索引再从 fts_content 重灌，确保与任意前序状态一致。
const SCHEMA_V3: &str = r#"
-- ① 重建 3 个 tag 触发器（group_concat 固定顺序，与 assets.rs fill_tags 展示排序一致）
DROP TRIGGER IF EXISTS trg_at_ai;
DROP TRIGGER IF EXISTS trg_at_ad;
DROP TRIGGER IF EXISTS trg_tags_au;

CREATE TRIGGER trg_at_ai AFTER INSERT ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.name, ' '), ''))
      FROM (SELECT t.name FROM asset_tags at JOIN tags t ON t.id = at.tag_id
             WHERE at.asset_id = new.asset_id
             ORDER BY t.sort_order, t.id) x
  ) WHERE asset_id = new.asset_id;
END;
CREATE TRIGGER trg_at_ad AFTER DELETE ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.name, ' '), ''))
      FROM (SELECT t.name FROM asset_tags at JOIN tags t ON t.id = at.tag_id
             WHERE at.asset_id = old.asset_id
             ORDER BY t.sort_order, t.id) x
  ) WHERE asset_id = old.asset_id;
END;
CREATE TRIGGER trg_tags_au AFTER UPDATE OF name ON tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.name, ' '), ''))
      FROM (SELECT t.name FROM asset_tags at JOIN tags t ON t.id = at.tag_id
             WHERE at.asset_id = fts_content.asset_id
             ORDER BY t.sort_order, t.id) x
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.id);
END;

-- ② 从源表全量回源重算 fts_content（旧 cjk_bigram 产物不可逆，必须 DELETE 后回源重算）
DELETE FROM fts_content;
INSERT INTO fts_content(asset_id, file_name, tag_names)
  SELECT a.id,
         cjk_bigram(a.file_name),
         (SELECT cjk_bigram(COALESCE(group_concat(x.name, ' '), ''))
            FROM (SELECT t.name FROM asset_tags at JOIN tags t ON t.id = at.tag_id
                   WHERE at.asset_id = a.id
                   ORDER BY t.sort_order, t.id) x)
  FROM assets a;

-- ③ 重建外部内容表 FTS 索引（确保 assets_fts 与 fts_content 完全一致）
INSERT INTO assets_fts(assets_fts) VALUES('rebuild');
"#;

/// v4：M3-02 去重扫描索引（hash GROUP BY 走索引，3 万素材红线）
const SCHEMA_V4: &str = r#"
CREATE INDEX IF NOT EXISTS idx_assets_hash ON assets(hash);
"#;

/// v5：S3 包——排序索引（R-21）+ 回收站 deleted_at（R-22）+ 打标流水 tag_ops（R-25）
const SCHEMA_V5: &str = r#"
CREATE INDEX IF NOT EXISTS idx_assets_taken_at ON assets(taken_at);
CREATE INDEX IF NOT EXISTS idx_assets_size     ON assets(file_size);
CREATE INDEX IF NOT EXISTS idx_assets_deleted  ON assets(deleted_at);

-- 打标操作流水（R-25）：确认/摘标签写入，撤销按 batch_id 反向操作
CREATE TABLE IF NOT EXISTS tag_ops (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  asset_id   INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  tag_id     INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
  op         TEXT    NOT NULL,             -- add | remove
  actor      TEXT    NOT NULL,             -- manual | ai_cloud | ai_local
  batch_id   INTEGER,                      -- AI 批次 id（手工操作为 NULL）
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tag_ops_batch   ON tag_ops(batch_id);
CREATE INDEX IF NOT EXISTS idx_tag_ops_created ON tag_ops(created_at);
"#;

/// B37 同款容错：检查表是否已有某列（SQLite ALTER ADD COLUMN 不支持 IF NOT EXISTS，
/// 逐列检查再 ALTER，幂等可重入——中途崩溃重启重跑不会 panic）
fn has_column(conn: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut names = rows.filter_map(|r| r.ok());
    Ok(names.any(|name| name == column))
}

/// B37 同款容错：先查列再 ALTER（deleted_at；SQLite ALTER 不支持 IF NOT EXISTS）
fn migrate_v5(conn: &Connection) -> AppResult<()> {
    if !has_column(conn, "assets", "deleted_at")? {
        conn.execute("ALTER TABLE assets ADD COLUMN deleted_at INTEGER", [])?;
    }
    conn.execute_batch(SCHEMA_V5)?;
    Ok(())
}

/// v6：本地打标错误详情——ai_suggestions 增加 last_error（单条失败原因落库供前端展示）
fn migrate_v6(conn: &Connection) -> AppResult<()> {
    if !has_column(conn, "ai_suggestions", "last_error")? {
        conn.execute("ALTER TABLE ai_suggestions ADD COLUMN last_error TEXT", [])?;
    }
    Ok(())
}

/// v7：P1-04 导出任务软提示——export_tasks 增加 warning 列（status=done 时的注意事项）
fn migrate_v7(conn: &Connection) -> AppResult<()> {
    if !has_column(conn, "export_tasks", "warning")? {
        conn.execute("ALTER TABLE export_tasks ADD COLUMN warning TEXT", [])?;
    }
    Ok(())
}

/// v8：标签系统地基——稳定分面、规范标签、别名、AI 候选明细与关联确认元数据。
/// 迁移仅增列/增表，保留现有 tag id 和 asset_tags 关联；所有步骤均可重入。
const SCHEMA_V8: &str = r#"
CREATE TABLE IF NOT EXISTS tag_facets (
  key            TEXT PRIMARY KEY,
  display_name   TEXT NOT NULL,
  description    TEXT NOT NULL DEFAULT '',
  selection_mode TEXT NOT NULL DEFAULT 'multi',
  max_items      INTEGER,
  sort_order     INTEGER NOT NULL DEFAULT 0,
  is_system      INTEGER NOT NULL DEFAULT 1,
  status         TEXT NOT NULL DEFAULT 'active',
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  CHECK(selection_mode IN ('single', 'multi')),
  CHECK(status IN ('active', 'deprecated'))
);

CREATE TABLE IF NOT EXISTS tag_aliases (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  tag_id           INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  alias            TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  locale           TEXT NOT NULL DEFAULT '',
  alias_type       TEXT NOT NULL DEFAULT 'synonym',
  is_searchable    INTEGER NOT NULL DEFAULT 1,
  created_at       INTEGER NOT NULL,
  UNIQUE(tag_id, normalized_alias, locale),
  CHECK(alias_type IN ('synonym', 'old_name', 'translation', 'typo'))
);
CREATE INDEX IF NOT EXISTS idx_tag_aliases_lookup
  ON tag_aliases(normalized_alias, locale);

CREATE TABLE IF NOT EXISTS ai_suggestion_items (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  suggestion_id   INTEGER NOT NULL REFERENCES ai_suggestions(id) ON DELETE CASCADE,
  facet_key       TEXT NOT NULL,
  raw_name        TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  tag_id          INTEGER REFERENCES tags(id) ON DELETE SET NULL,
  confidence      REAL,
  decision        TEXT NOT NULL DEFAULT 'pending',
  decision_reason TEXT,
  created_at      INTEGER NOT NULL,
  CHECK(confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
  CHECK(decision IN ('pending', 'accepted', 'modified', 'rejected'))
);
CREATE INDEX IF NOT EXISTS idx_ai_suggestion_items_suggestion
  ON ai_suggestion_items(suggestion_id);
CREATE INDEX IF NOT EXISTS idx_ai_suggestion_items_tag
  ON ai_suggestion_items(tag_id);

CREATE INDEX IF NOT EXISTS idx_tags_facet_status
  ON tags(facet_key, status, sort_order, id);
CREATE INDEX IF NOT EXISTS idx_tags_normalized
  ON tags(facet_key, normalized_name);
CREATE INDEX IF NOT EXISTS idx_asset_tags_confirmation
  ON asset_tags(confirmation, tag_id, asset_id);
"#;

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> AppResult<()> {
    if !has_column(conn, table, column)? {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn migrate_v8(conn: &Connection) -> AppResult<()> {
    add_column_if_missing(conn, "tags", "canonical_name", "TEXT")?;
    add_column_if_missing(conn, "tags", "normalized_name", "TEXT")?;
    add_column_if_missing(conn, "tags", "facet_key", "TEXT NOT NULL DEFAULT 'custom'")?;
    add_column_if_missing(conn, "tags", "status", "TEXT NOT NULL DEFAULT 'active'")?;
    add_column_if_missing(conn, "tags", "is_system", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "tags", "description", "TEXT NOT NULL DEFAULT ''")?;

    add_column_if_missing(conn, "asset_tags", "confidence", "REAL")?;
    add_column_if_missing(
        conn,
        "asset_tags",
        "confirmation",
        "TEXT NOT NULL DEFAULT 'confirmed'",
    )?;
    add_column_if_missing(conn, "asset_tags", "confirmed_at", "INTEGER")?;
    add_column_if_missing(conn, "asset_tags", "confirmed_by", "TEXT")?;
    add_column_if_missing(conn, "asset_tags", "source_batch_id", "INTEGER")?;

    conn.execute_batch(SCHEMA_V8)?;
    super::tag_facets::seed_system_facets(conn)?;

    conn.execute(
        "UPDATE tags SET canonical_name = name WHERE canonical_name IS NULL OR canonical_name = ''",
        [],
    )?;
    conn.execute(
        "UPDATE tags SET normalized_name = lower(trim(name)) WHERE normalized_name IS NULL OR normalized_name = ''",
        [],
    )?;
    conn.execute(
        "UPDATE asset_tags SET confirmed_by = 'migration', confirmed_at = created_at
          WHERE confirmed_by IS NULL",
        [],
    )?;

    // 旧分类根节点映射到稳定分面；未知根节点及其后代保留为 custom。
    const ROOT_MAPPINGS: &[(&str, &str)] = &[
        ("主体", "subject"),
        ("物体", "subject"),
        ("场景", "scene"),
        ("用途", "purpose"),
        ("风格", "style"),
        ("色彩风格", "style"),
        ("氛围情绪", "style"),
        ("色彩", "color"),
        ("构图视角", "composition"),
        ("构图/视角", "composition"),
        ("光线", "lighting"),
        ("光线/时间", "lighting"),
        ("人物", "people"),
        ("人物属性", "people"),
        ("技术", "technical"),
        ("可用性/技术特征", "technical"),
    ];
    for (root_name, facet_key) in ROOT_MAPPINGS {
        conn.execute(
            "WITH RECURSIVE sub(id) AS (
               SELECT id FROM tags WHERE parent_id IS NULL AND name = ?1
               UNION ALL SELECT t.id FROM tags t JOIN sub s ON t.parent_id = s.id
             )
             UPDATE tags SET facet_key = ?2,
                    is_system = CASE WHEN parent_id IS NULL THEN 1 ELSE is_system END
              WHERE id IN (SELECT id FROM sub)",
            rusqlite::params![root_name, facet_key],
        )?;
    }

    // FTS 文档包含规范标签名和可搜索别名；别名变化也会刷新相关素材。
    conn.execute_batch(
        r#"
DROP TRIGGER IF EXISTS trg_at_ai;
DROP TRIGGER IF EXISTS trg_at_ad;
DROP TRIGGER IF EXISTS trg_tags_au;
DROP TRIGGER IF EXISTS trg_tag_alias_ai;
DROP TRIGGER IF EXISTS trg_tag_alias_au;
DROP TRIGGER IF EXISTS trg_tag_alias_ad;

CREATE TRIGGER trg_at_ai AFTER INSERT ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = new.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = new.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id = new.asset_id;
END;
CREATE TRIGGER trg_at_ad AFTER DELETE ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = old.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = old.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id = old.asset_id;
END;
CREATE TRIGGER trg_tags_au AFTER UPDATE OF name, status ON tags BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.id);
END;
CREATE TRIGGER trg_tag_alias_ai AFTER INSERT ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.tag_id);
END;
CREATE TRIGGER trg_tag_alias_au AFTER UPDATE ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id IN (old.tag_id, new.tag_id));
END;
CREATE TRIGGER trg_tag_alias_ad AFTER DELETE ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = (
    SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
      SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      UNION ALL
      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
      ORDER BY ord, tid, kind, term
    ) x
  ) WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = old.tag_id);
END;

UPDATE fts_content SET tag_names = (
  SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM (
    SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
      FROM asset_tags at JOIN tags t ON t.id = at.tag_id
     WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
    UNION ALL
    SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
      FROM asset_tags at JOIN tags t ON t.id = at.tag_id
      JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
     WHERE at.asset_id = fts_content.asset_id AND t.status = 'active'
    ORDER BY ord, tid, kind, term
  ) x
);
INSERT INTO assets_fts(assets_fts) VALUES('rebuild');
"#,
    )?;
    Ok(())
}

/// v9：超级搜索查询索引（P1A）。只加索引，不改业务行数。
/// 评审 §三.3 取舍：只加 taken_at / camera / file_size / (width,height) 四个；
/// file_ext 基数极低不做；lens / duration_ms 等真出现慢查询再说；aspect_ratio 为派生表达式暂无索引承诺。
const SCHEMA_V9: &str = r#"
CREATE INDEX IF NOT EXISTS idx_assets_taken_at ON assets(taken_at);
CREATE INDEX IF NOT EXISTS idx_assets_camera   ON assets(camera);
CREATE INDEX IF NOT EXISTS idx_assets_file_size ON assets(file_size);
CREATE INDEX IF NOT EXISTS idx_assets_width_height ON assets(width, height);
"#;

/// v10：tagCategories（中文名机器协议）→ ai_facet_configs（稳定 facet_key）已在上方 migrate() 处理。
/// v11：独立 color 分面补齐（指导书 C-3/C-5）。
/// 老库 V8 已建 tag_facets，但默认 AI 配置曾把「色彩风格」归 style 而缺少独立 color；
/// 新库由 default_tag_categories 覆盖（含「色彩」→color）。本迁移幂等：
///  ① INSERT OR IGNORE 补齐 color tag_facets 行；
///  ② 若 ai_facet_configs 缺 color 配置则补默认（不覆盖用户已有 style hint/配置内容）。
fn migrate_v11(conn: &Connection) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT OR IGNORE INTO tag_facets
         (key, display_name, description, selection_mode, max_items, sort_order, is_system, status, created_at, updated_at)
         VALUES ('color', '色彩', '主色、色调与色彩关系', 'multi', 3, 50, 1, 'active', ?1, ?1)",
        rusqlite::params![now],
    )?;
    super::settings::ensure_color_facet_config(conn)?;
    Ok(())
}

/// v12：媒体元数据结构化字段 + 原始 JSON + 扫描状态（指导书 §7.4/§13.1）。
/// 只新增列，不改业务行；不在此迁移中扫描/回填视频（回填由可取消后台任务负责）。
/// 逐列 ALTER，幂等可重入（中途崩溃重启重跑不 panic）。
const V12_COLUMNS: &[(&str, &str)] = &[
    ("media_kind", "TEXT"), // image | video | unknown（后端探测事实源）
    ("container_format", "TEXT"),
    ("video_profile", "TEXT"),
    ("pixel_format", "TEXT"),
    ("bit_depth", "INTEGER"),
    ("frame_rate", "REAL"), // 简化：平均帧率小数；分子/分母原始值见 media_metadata_json
    ("video_bit_rate", "INTEGER"),
    ("color_range", "TEXT"),
    ("color_space", "TEXT"),
    ("color_transfer", "TEXT"),
    ("color_primaries", "TEXT"),
    ("audio_sample_rate", "INTEGER"),
    ("audio_channels", "INTEGER"),
    ("audio_layout", "TEXT"),
    ("rotation", "INTEGER"),
    ("media_metadata_json", "TEXT"),    // 原始 ffprobe JSON 留底
    ("metadata_version", "INTEGER"),    // 探测协议版本
    ("metadata_scanned_at", "INTEGER"), // 最近一次探测时间（ms）
    ("metadata_error", "TEXT"),         // 探测失败原因；NULL 表示未探测/成功
];

fn migrate_v12(conn: &Connection) -> AppResult<()> {
    for (col, ty) in V12_COLUMNS {
        add_column_if_missing(conn, "assets", col, ty)?;
    }
    Ok(())
}

/// v13：标签分面生命周期能力（指导书 §12.2/§13.1 仅补真实缺失能力）。
/// ① 给 tag_facets 增加 `applies_to`（all|image|video，默认 all）；
/// ② 状态拓宽为 active|inactive|deprecated（原 CHECK 只有 active|deprecated）。
/// SQLite 无法 ALTER CHECK，需重建表；无任何表以 FK 引用 tag_facets（tags.facet_key 只是普通 TEXT 列），
/// 重建安全。幂等：仅当缺 applies_to 列时执行。
const V13_FACETS: &str = r#"
CREATE TABLE tag_facets_new (
  key            TEXT PRIMARY KEY,
  display_name   TEXT NOT NULL,
  description    TEXT NOT NULL DEFAULT '',
  selection_mode TEXT NOT NULL DEFAULT 'multi',
  max_items      INTEGER,
  sort_order     INTEGER NOT NULL DEFAULT 0,
  is_system      INTEGER NOT NULL DEFAULT 1,
  status         TEXT NOT NULL DEFAULT 'active',
  applies_to     TEXT NOT NULL DEFAULT 'all',
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  CHECK(selection_mode IN ('single', 'multi')),
  CHECK(status IN ('active', 'inactive', 'deprecated')),
  CHECK(applies_to IN ('all', 'image', 'video'))
);
INSERT INTO tag_facets_new
  (key, display_name, description, selection_mode, max_items, sort_order, is_system, status, applies_to, created_at, updated_at)
SELECT key, display_name, description, selection_mode, max_items, sort_order, is_system,
       CASE WHEN status = 'deprecated' THEN 'inactive' ELSE status END,
       'all', created_at, updated_at
  FROM tag_facets;
DROP TABLE tag_facets;
ALTER TABLE tag_facets_new RENAME TO tag_facets;
"#;

fn migrate_v13(conn: &Connection) -> AppResult<()> {
    if !has_column(conn, "tag_facets", "applies_to")? {
        conn.execute_batch(V13_FACETS)?;
    }
    Ok(())
}

/// v14：视频兼容代理缓存（指导书 §8.3）。记录按素材 + 变体生成/查询状态，不替换原文件。
/// 状态机：queued|running|ready|failed|canceled。代理失败原因可展示；清理缓存不影响原文件。
const V14: &str = r#"
CREATE TABLE IF NOT EXISTS video_proxies (
  asset_id   INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  variant    TEXT    NOT NULL DEFAULT 'h264_mp4',
  status     TEXT    NOT NULL DEFAULT 'queued',
  path       TEXT,
  error      TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (asset_id, variant),
  CHECK(status IN ('queued', 'running', 'ready', 'failed', 'canceled'))
);
CREATE INDEX IF NOT EXISTS idx_video_proxies_asset ON video_proxies(asset_id);
"#;

fn migrate_v14(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(V14)?;
    Ok(())
}

/// v15：AI 连接档案 + 用途绑定（指导书 §6.3/§7.3）。
///  - 创建 ai_connections / ai_usage_bindings；
///  - 迁移旧 settings.app_settings 的每个 profile 到连接表；
///  - 按 kind 映射 deployment（local→local，其他→cloud）；
///  - 按 apiMode 映射 protocol（openai→openai_chat，anthropic→anthropic_messages，缺失→openai_chat + warning）；
///  - API Key 写入系统凭据（keyring），成功后 api_key_ref = connection_id；
///    写入失败则保留旧 JSON 里的明文 key，并返回迁移 warning（不丢 key）；
///  - 旧 active_profile 同时绑定到 super_search 和 tagging，用户可在 UI 分别修改；
///  - 幂等：以 ai_connections 表非空 / user_version 提交为准，重复执行无副作用。
const V15_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ai_connections (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  deployment TEXT NOT NULL CHECK (deployment IN ('cloud','local')),
  protocol TEXT NOT NULL CHECK (protocol IN ('openai_chat','anthropic_messages')),
  base_url TEXT NOT NULL,
  model TEXT NOT NULL,
  api_key_ref TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_usage_bindings (
  usage TEXT PRIMARY KEY CHECK (usage IN ('super_search','tagging')),
  connection_id TEXT NOT NULL REFERENCES ai_connections(id),
  updated_at INTEGER NOT NULL
);
"#;

/// §6.4 apiMode → protocol 固定映射（缺失/未知 → openai_chat + warning）。
fn map_protocol(api_mode: &str) -> &'static str {
    match api_mode {
        "anthropic" => "anthropic_messages",
        _ => "openai_chat",
    }
}

fn migrate_v15(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(V15_SCHEMA)?;
    // 幂等：已有连接档案（
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM ai_connections", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }

    // 读取旧 settings（app_settings），迁入连接表
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'app_settings'",
            [],
            |r| r.get(0),
        )
        .ok();
    let Some(raw) = raw else {
        return Ok(());
    };
    let s: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("v15 迁移：settings JSON 解析失败，跳过连接迁移: {e}");
            return Ok(());
        }
    };
    let ai = s.get("ai").cloned().unwrap_or_default();
    let profiles = ai
        .get("profiles")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let active_id = ai
        .get("activeProfile")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let now = chrono::Utc::now().timestamp_millis();

    for p in &profiles {
        let id = p
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            tracing::warn!("v15 迁移：跳过无 id 的 profile");
            continue;
        }
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("未命名")
            .to_string();
        let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let deployment = if kind == "local" { "local" } else { "cloud" };
        let api_mode = p.get("apiMode").and_then(|v| v.as_str()).unwrap_or("");
        let protocol = map_protocol(api_mode);
        if api_mode.is_empty() || (api_mode != "openai" && api_mode != "anthropic") {
            tracing::warn!(
                "v15 迁移：profile {name}({id}) 的 apiMode 缺失/未知({api_mode:?})，按 openai_chat 处理"
            );
        }
        let base_url = p
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let model = p
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let api_key = p
            .get("apiKey")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // API Key → keyring；成功才置 api_key_ref，失败保留旧 JSON（不丢 key + warning）
        let mut api_key_ref: Option<String> = None;
        if !api_key.is_empty() {
            match crate::services::credentials::save_api_key(&id, &api_key) {
                Ok(()) => api_key_ref = Some(id.clone()),
                Err(e) => {
                    tracing::warn!(
                        "v15 迁移：profile {name}({id}) 的 API Key 写入系统凭据失败，保留旧 JSON: {e}"
                    );
                }
            }
        }
        conn.execute(
            "INSERT OR IGNORE INTO ai_connections
               (id, name, deployment, protocol, base_url, model, api_key_ref, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)",
            rusqlite::params![id, name, deployment, protocol, base_url, model, api_key_ref, now],
        )?;
    }

    // 旧 active_profile 同时绑定 super_search + tagging（UI 可分别修改）
    if !active_id.is_empty() {
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_connections WHERE id = ?1)",
            [&active_id],
            |r| r.get(0),
        )?;
        if exists != 0 {
            for usage in ["super_search", "tagging"] {
                conn.execute(
                    "INSERT OR IGNORE INTO ai_usage_bindings (usage, connection_id, updated_at)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![usage, active_id, now],
                )?;
            }
        }
    }

    // 迁移成功后：原 settings JSON 移除明文 API Key（写备份副本；失败保留旧数据并 warning）
    if let Ok(mut s2) = serde_json::from_str::<serde_json::Value>(&raw) {
        // 备份副本
        let _ = conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('app_settings_backup_v15', ?1)",
            [&raw],
        );
        let profiles = s2
            .get_mut("ai")
            .and_then(|a| a.get_mut("profiles"))
            .and_then(|p| p.as_array_mut());
        if let Some(profiles) = profiles {
            let mut all_migrated = true;
            for p in profiles.iter_mut() {
                let has_key = p
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .map(|k| !k.is_empty())
                    .unwrap_or(false);
                if has_key {
                    let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    // 只有该连接成功写入凭据才清除明文；否则保留（不丢 key）
                    let cleared: bool = conn
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM ai_connections WHERE id = ?1 AND api_key_ref = ?1)",
                            [&id],
                            |r| r.get(0),
                        )
                        .unwrap_or(false);
                    if cleared {
                        if let Some(obj) = p.as_object_mut() {
                            obj.insert("apiKey".into(), serde_json::Value::String(String::new()));
                        }
                    } else {
                        all_migrated = false;
                    }
                }
            }
            if all_migrated {
                if let Ok(cleaned) = serde_json::to_string(&s2) {
                    let _ = conn.execute(
                        "UPDATE settings SET value = ?1 WHERE key = 'app_settings'",
                        [&cleaned],
                    );
                }
            } else {
                tracing::warn!("v15 迁移：部分 profile 未成功写入凭据，settings JSON 保留明细（备份见 app_settings_backup_v15）");
            }
        }
    }
    Ok(())
}

/// FB2-08（§14.8）V16：颜色从 AI 分面改为算法主色属性。
///
///  - `assets` 加 6 列（palette_json / palette_version / palette_scanned_at / dominant_hue|sat|lum）+ 3 索引；
///  - color 分面停用：`tag_facets.status = 'inactive'`、`ai_facet_configs` 里 color 的 `enabledForAi = false`、
///    `visibleInWorkbench = false`（存量 color 标签保留可搜索，只是不再由 AI 生成、工作台默认收起）。
///  - 幂等：add_column_if_missing + IF NOT EXISTS + UPDATE 无条件（重复执行无副作用）。
const V16_COLUMNS: &[(&str, &str)] = &[
    ("palette_json", "TEXT"),
    ("palette_version", "INTEGER"),
    ("palette_scanned_at", "INTEGER"),
    ("dominant_hue", "INTEGER"),
    ("dominant_sat", "INTEGER"),
    ("dominant_lum", "INTEGER"),
];

const V16_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_assets_dominant_hue ON assets(dominant_hue);
CREATE INDEX IF NOT EXISTS idx_assets_dominant_sat ON assets(dominant_sat);
CREATE INDEX IF NOT EXISTS idx_assets_dominant_lum ON assets(dominant_lum);
"#;

/// V16 迁移：加色板列 + 索引 + color 分面停用（AI 侧摘除，见 §14.3）。
fn migrate_v16(conn: &Connection) -> AppResult<()> {
    // 1. 只增列 + 索引（ALTER / CREATE INDEX IF NOT EXISTS 天然幂等，放事务外）
    for (col, ty) in V16_COLUMNS {
        add_column_if_missing(conn, "assets", col, ty)?;
    }
    conn.execute_batch(V16_INDEXES)?;

    let now = chrono::Utc::now().timestamp_millis();
    // 数据写操作包事务：否则中途失败会留下"做了一半、版本号未升"的库，
    // 每次启动都在同一句报错（FX-01 的生产表现）。
    let tx = conn.unchecked_transaction()?;

    // 2. color 分面停用（'deprecated' 语义已在 V13 收敛到 'inactive'）
    tx.execute(
        "UPDATE tag_facets SET status = 'inactive', updated_at = ?1 WHERE key = 'color'",
        rusqlite::params![now],
    )?;

    // 3. settings JSON（camelCase）一次读-改-写：
    //    - color: enabledForAi=false + visibleInWorkbench=false；
    //    - style: hint 追加「不包含颜色」——WHY: hint 是 settings JSON 的字段（settings.rs AiFacetConfig），
    //      tag_facets 表里从来没有这一列（V8 建表 / V13 重建均无），写表会 no such column（FX-01）。
    if let Some(raw) = tx
        .query_row(
            "SELECT value FROM settings WHERE key = 'app_settings'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
    {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) {
            let mut changed = false;
            if let Some(arr) = v.get_mut("aiFacetConfigs").and_then(|c| c.as_array_mut()) {
                for cfg in arr.iter_mut() {
                    let key = cfg.get("facetKey").and_then(|k| k.as_str()).unwrap_or("");
                    match key {
                        "color" => {
                            if let Some(o) = cfg.as_object_mut() {
                                o.insert("enabledForAi".into(), serde_json::Value::Bool(false));
                                o.insert(
                                    "visibleInWorkbench".into(),
                                    serde_json::Value::Bool(false),
                                );
                                changed = true;
                            }
                        }
                        "style" => {
                            let old = cfg.get("hint").and_then(|h| h.as_str()).unwrap_or("");
                            if !old.contains("不包含颜色") {
                                let next = if old.trim().is_empty() {
                                    "风格描述不包含颜色（颜色由算法主色呈现）".to_string()
                                } else {
                                    format!("{old}。风格描述不包含颜色（颜色由算法主色呈现）")
                                };
                                if let Some(o) = cfg.as_object_mut() {
                                    o.insert("hint".into(), serde_json::Value::String(next));
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            if changed {
                if let Ok(s) = serde_json::to_string(&v) {
                    tx.execute(
                        "UPDATE settings SET value = ?1 WHERE key = 'app_settings'",
                        [&s],
                    )?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// FB5-05（§7.2）V17：素材一句话描述（content_description）。
///
///  - 幂等加列：assets.content_description / ai_suggestions.suggested_description
///    + confirmed_description / fts_content.content_description；
///  - FTS5 虚拟表不能 ALTER 加索引列 → 重建 assets_fts 为三列
///    （file_name/tag_names/content_description），重建 trg_fc_*（delete 必须写全旧三列，防幽灵 token）
///    与 trg_assets_ai / trg_assets_au；
///  - trg_assets_au 改为 `AFTER UPDATE OF file_name, content_description`，两列都经 cjk_bigram；
///  - 保留 V8「规范标签 + 可搜索别名 + active 状态」的 tag_names 聚合语义
///    （trg_at_* / trg_tags_au / trg_tag_alias_* 不动，不退回 V1 简单 group_concat）；
///  - 从 assets 回填 fts_content.content_description（回填 UPDATE 经过新三列触发器）；
///  - INSERT INTO assets_fts(assets_fts) VALUES('rebuild')；
///  - 全部完成后再写 user_version=17：中途崩溃保留 version=16，下次启动重跑，
///    重建是 DROP/CREATE + 回源回填，不重复丢业务数据。
fn migrate_v17(conn: &Connection) -> AppResult<()> {
    // 1. 加列（幂等）
    add_column_if_missing(
        conn,
        "assets",
        "content_description",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "ai_suggestions",
        "suggested_description",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(conn, "ai_suggestions", "confirmed_description", "TEXT")?;
    add_column_if_missing(
        conn,
        "fts_content",
        "content_description",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    // 2-5. 重建 FTS 虚表 + 第一/第二层触发器（trg_at_*/trg_tags_au/trg_tag_alias_* 保留 V8 语义）
    conn.execute_batch(
        r#"
DROP TRIGGER IF EXISTS trg_fc_ai;
DROP TRIGGER IF EXISTS trg_fc_ad;
DROP TRIGGER IF EXISTS trg_fc_au;
DROP TRIGGER IF EXISTS trg_assets_ai;
DROP TRIGGER IF EXISTS trg_assets_au;

DROP TABLE IF EXISTS assets_fts;
CREATE VIRTUAL TABLE assets_fts USING fts5(
  file_name,
  tag_names,
  content_description,
  content='fts_content',
  content_rowid='asset_id',
  tokenize='unicode61'
);

-- 第一层：FTS 索引维护（delete 写全旧三列，否则旧 token 残留产生幻影命中）
CREATE TRIGGER trg_fc_ai AFTER INSERT ON fts_content BEGIN
  INSERT INTO assets_fts(rowid, file_name, tag_names, content_description)
    VALUES (new.asset_id, new.file_name, new.tag_names, new.content_description);
END;
CREATE TRIGGER trg_fc_ad AFTER DELETE ON fts_content BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, file_name, tag_names, content_description)
    VALUES ('delete', old.asset_id, old.file_name, old.tag_names, old.content_description);
END;
CREATE TRIGGER trg_fc_au AFTER UPDATE ON fts_content BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, file_name, tag_names, content_description)
    VALUES ('delete', old.asset_id, old.file_name, old.tag_names, old.content_description);
  INSERT INTO assets_fts(rowid, file_name, tag_names, content_description)
    VALUES (new.asset_id, new.file_name, new.tag_names, new.content_description);
END;

-- 第二层：业务表只维护 fts_content（file_name 与 content_description 都经 cjk_bigram）
CREATE TRIGGER trg_assets_ai AFTER INSERT ON assets BEGIN
  INSERT INTO fts_content(asset_id, file_name, tag_names, content_description)
    VALUES (new.id, cjk_bigram(new.file_name), '', cjk_bigram(new.content_description));
END;
CREATE TRIGGER trg_assets_au AFTER UPDATE OF file_name, content_description ON assets BEGIN
  UPDATE fts_content
     SET file_name = cjk_bigram(new.file_name),
         content_description = cjk_bigram(new.content_description)
   WHERE asset_id = new.id;
END;
"#,
    )?;

    // 5b. 预同步：重建后先 rebuild 一次，把既有 fts_content 行灌进新索引——
    //     否则回填 UPDATE 触发的 delete 命令会命中「索引中不存在」的行，
    //     FTS5 外部内容表对此报 SQLITE_CORRUPT_VTAB(267)（已实测）。
    //     不违反 §7.2 顺序：触发器仍在回填之前就位，最后仍有一次 rebuild 收尾。
    conn.execute_batch("INSERT INTO assets_fts(assets_fts) VALUES('rebuild');")?;

    // 7. 从 assets 回填 fts_content.content_description（此时新触发器已就位，回填 UPDATE 同步 FTS）
    conn.execute(
        "UPDATE fts_content SET content_description = (
           SELECT cjk_bigram(COALESCE(a.content_description, ''))
             FROM assets a WHERE a.id = fts_content.asset_id
         )",
        [],
    )?;

    // 8. 重建 FTS 索引（确保 assets_fts 与 fts_content 完全一致）
    conn.execute_batch("INSERT INTO assets_fts(assets_fts) VALUES('rebuild');")?;
    Ok(())
}

/// GPS 定位属性 V18：assets 加经纬度列。
///
///  - `latitude REAL` / `longitude REAL`：有符号十进制度（北纬东经为正），无定位为 NULL；
///  - 各自建索引（按经纬度区间检索 / 位置分面聚合走索引）；
///  - 幂等：add_column_if_missing + CREATE INDEX IF NOT EXISTS（重复执行无副作用）。
///  - 本期只存原始经纬度，不做城市反向地理编码（决策）。
fn migrate_v18(conn: &Connection) -> AppResult<()> {
    add_column_if_missing(conn, "assets", "latitude", "REAL")?;
    add_column_if_missing(conn, "assets", "longitude", "REAL")?;
    conn.execute_batch(
        r#"
CREATE INDEX IF NOT EXISTS idx_assets_latitude ON assets(latitude);
CREATE INDEX IF NOT EXISTS idx_assets_longitude ON assets(longitude);
"#,
    )?;
    Ok(())
}

/// V19：基础版能力补齐的数据列。全部幂等（add_column_if_missing + IF NOT EXISTS）。
fn migrate_v19(conn: &Connection) -> AppResult<()> {
    add_column_if_missing(conn, "assets", "favorite", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "assets", "rating", "INTEGER NOT NULL DEFAULT 0")?;
    // user_rotation：用户手动旋转（0/90/180/270）。
    // 严禁复用 assets.rotation —— 那是 V12 的 ffprobe 媒体元数据语义。
    add_column_if_missing(
        conn,
        "assets",
        "user_rotation",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "assets", "phash", "INTEGER")?;
    conn.execute_batch(
        r#"
CREATE INDEX IF NOT EXISTS idx_assets_favorite ON assets(favorite)
  WHERE favorite = 1 AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_assets_rating   ON assets(rating)
  WHERE rating > 0 AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_assets_phash    ON assets(phash)
  WHERE phash IS NOT NULL;
"#,
    )?;
    Ok(())
}

/// V20：分面单一事实源（低风险段）。settings.aiFacetConfigs 的语义搬进 tag_facets。
/// 幂等：add_column_if_missing + UPDATE 回填（可重跑）。
fn migrate_v20(conn: &Connection) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();

    // ① 加列。SQLite 的 ALTER ADD COLUMN 不能带 CHECK（tag_facets 已有 3 个 CHECK，
    //    只能建表时声明）→ input_mode 靠 Rust 层 validate 兜底。
    add_column_if_missing(
        conn,
        "tag_facets",
        "input_mode",
        "TEXT NOT NULL DEFAULT 'ai_and_manual'",
    )?;

    // ② 回填。实测库：6 个 enabledForAi=true → ai_and_manual；color(false) → manual_only。
    //    hint 并入 description（拼接不覆盖，instr 守卫防重跑重复拼）。
    let s = crate::db::settings::get_settings(conn)?;
    for cfg in &s.ai_facet_configs {
        let mode = if cfg.enabled_for_ai {
            "ai_and_manual"
        } else {
            "manual_only"
        };
        conn.execute(
            "UPDATE tag_facets SET
                input_mode = ?2,
                description = CASE
                    WHEN ?3 = ''                    THEN description
                    WHEN trim(description) = ''     THEN ?3
                    WHEN instr(description, ?3) > 0 THEN description
                    ELSE description || char(10) || ?3
                END,
                display_name = COALESCE(NULLIF(trim(?4), ''), display_name),
                updated_at = ?5
              WHERE key = ?1",
            rusqlite::params![
                cfg.facet_key,
                mode,
                cfg.hint.trim(),
                cfg.display_name.as_deref().unwrap_or(""),
                now
            ],
        )?;
    }

    // ③ 无 aiFacetConfigs 条目的分面（实测库：purpose / technical / custom 三个）
    //    保持列默认 ai_and_manual。但 purpose / technical 天然是手工类 → 修正。
    //    守卫：只改「还没有任何标签」的分面，不覆盖已在用的配置。
    conn.execute(
        "UPDATE tag_facets SET input_mode='manual_only', updated_at=?1
          WHERE key IN ('purpose','technical') AND is_system=1
            AND NOT EXISTS (SELECT 1 FROM tags WHERE facet_key = tag_facets.key)",
        rusqlite::params![now],
    )?;

    // ④ 清空 JSON 侧（此后 Settings.ai_facet_configs 为 skip_serializing）
    let mut s2 = crate::db::settings::get_settings(conn)?;
    s2.ai_facet_configs.clear();
    crate::db::settings::save_settings(conn, &s2)?;
    Ok(())
}

/// FTS 触发器重建。facets 条件由调用方传入（V21 用 status='active'；V22a 起用
/// cfg_searchable=1 —— 语义变更：停用分面的标签仍可搜）。
/// facet_refresh_col：分面表的哪一列变化需要刷新其下素材的 FTS 词（V21=status，
/// V22a=cfg_searchable —— 必须与 facet_cond 依赖的列一致，否则刷新时机不对）。
/// 铁律：①复制现有 SQL 再改（V8 语义逐字保留，只换 EXISTS 条件）②改完立即 rebuild
/// ③先在内存库跑通全部 FTS 测试再上真库。
fn rebuild_fts_triggers_with_cond(
    conn: &Connection,
    facet_cond: &str,
    facet_refresh_col: &str,
) -> AppResult<()> {
    // P0-4（铁律 9）：tag_unique_terms=1 后 tag_aliases 冻结（新别名只写 tag_terms），
    // FTS 词源必须跟着切到 tag_terms —— 否则 gate 开后新增的别名永远进不了 FTS。
    let gated = crate::db::schema_features::feature_enabled(conn, "tag_unique_terms")?;
    // 别名半段的词源：gate 关读旧表（V8 语义逐字保留），gate 开读 tag_terms 的非 canonical 行。
    let alias_source = if gated {
        r#"      SELECT tt.term AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_terms tt ON tt.tag_id = t.id
         AND tt.is_searchable = 1 AND tt.term_kind != 'canonical'
       WHERE at.asset_id = {ASSET_REF} AND t.status = 'active' {facet_cond}"#
    } else {
        r#"      SELECT ta.alias AS term, t.sort_order AS ord, t.id AS tid, 1 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
        JOIN tag_aliases ta ON ta.tag_id = t.id AND ta.is_searchable = 1
       WHERE at.asset_id = {ASSET_REF} AND t.status = 'active' {facet_cond}"#
    };
    // 聚合子查询模板：与 V8 完全一致的分母 + 分面条件（注入）+ 按 gate 选别名词源
    // 注意：alias_source 是独立的原始字符串，其中的 {facet_cond} 不会被外层 format! 处理，
    // 组装后再整体 replace 一次（主字面量里的 {facet_cond} 已在 format! 时替换）。
    let term_words = format!(
        r#"SELECT t.name AS term, t.sort_order AS ord, t.id AS tid, 0 AS kind
        FROM asset_tags at JOIN tags t ON t.id = at.tag_id
       WHERE at.asset_id = {{ASSET_REF}} AND t.status = 'active' {facet_cond}
      UNION ALL
{alias_source}
      ORDER BY ord, tid, kind, term"#,
        alias_source = alias_source
    )
    .replace("{facet_cond}", facet_cond);
    let agg = |asset_ref: &str| -> String {
        format!(
            "SELECT cjk_bigram(COALESCE(group_concat(x.term, ' '), '')) FROM ({}) x",
            term_words.replace("{ASSET_REF}", asset_ref)
        )
    };

    // 别名词源的维护触发器按 gate 分两组（旧表一组 / tag_terms 一组）。
    // 无论哪组都先 DROP 全部相关触发器（幂等重跑 + gate 切换都安全）。
    conn.execute_batch(
        r#"
DROP TRIGGER IF EXISTS trg_at_ai;
DROP TRIGGER IF EXISTS trg_at_ad;
DROP TRIGGER IF EXISTS trg_tags_au;
DROP TRIGGER IF EXISTS trg_tag_alias_ai;
DROP TRIGGER IF EXISTS trg_tag_alias_au;
DROP TRIGGER IF EXISTS trg_tag_alias_ad;
DROP TRIGGER IF EXISTS trg_terms_ai;
DROP TRIGGER IF EXISTS trg_terms_au;
DROP TRIGGER IF EXISTS trg_terms_ad;
DROP TRIGGER IF EXISTS trg_facet_status_au;
"#,
    )?;

    // ① asset_tags INSERT/DELETE：按 new/old.asset_id 刷新
    conn.execute_batch(&format!(
        r#"CREATE TRIGGER trg_at_ai AFTER INSERT ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = ({agg_new}) WHERE asset_id = new.asset_id;
END;
CREATE TRIGGER trg_at_ad AFTER DELETE ON asset_tags BEGIN
  UPDATE fts_content SET tag_names = ({agg_old}) WHERE asset_id = old.asset_id;
END;"#,
        agg_new = agg("new.asset_id"),
        agg_old = agg("old.asset_id"),
    ))?;

    // ② tags 名字/状态变化：刷新该标签关联的全部素材
    conn.execute_batch(&format!(
        r#"CREATE TRIGGER trg_tags_au AFTER UPDATE OF name, status ON tags BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.id);
END;"#,
        agg_fc = agg("fts_content.asset_id"),
    ))?;

    // ③ 别名词源维护触发器：gate 关 → 挂 tag_aliases（冻结前仍由它维护）；
    //    gate 开 → 挂 tag_terms（新别名/旧词迁移后的唯一事实源，维护时机对齐）。
    if gated {
        conn.execute_batch(&format!(
            r#"CREATE TRIGGER trg_terms_ai AFTER INSERT ON tag_terms BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.tag_id);
END;
CREATE TRIGGER trg_terms_au AFTER UPDATE ON tag_terms BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id IN (old.tag_id, new.tag_id));
END;
CREATE TRIGGER trg_terms_ad AFTER DELETE ON tag_terms BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = old.tag_id);
END;"#,
            agg_fc = agg("fts_content.asset_id"),
        ))?;
    } else {
        conn.execute_batch(&format!(
            r#"CREATE TRIGGER trg_tag_alias_ai AFTER INSERT ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = new.tag_id);
END;
CREATE TRIGGER trg_tag_alias_au AFTER UPDATE ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id IN (old.tag_id, new.tag_id));
END;
CREATE TRIGGER trg_tag_alias_ad AFTER DELETE ON tag_aliases BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (SELECT asset_id FROM asset_tags WHERE tag_id = old.tag_id);
END;"#,
            agg_fc = agg("fts_content.asset_id"),
        ))?;
    }

    // ④ 分面条件变化 → 刷新其下标签关联的全部素材（列由调用方指定）
    conn.execute_batch(&format!(
        r#"CREATE TRIGGER trg_facet_status_au AFTER UPDATE OF {facet_col} ON tag_facets BEGIN
  UPDATE fts_content SET tag_names = ({agg_fc})
   WHERE asset_id IN (
     SELECT at.asset_id FROM asset_tags at JOIN tags t ON t.id = at.tag_id
      WHERE t.facet_key = new.key
   );
END;"#,
        facet_col = facet_refresh_col,
        agg_fc = agg("fts_content.asset_id"),
    ))?;

    Ok(())
}

/// V21 入口：重建触发器 + 全量 rebuild（FTS 与内容表对齐）。
/// V21 在 V22a 之前运行（列尚不存在），必须用 status 条件；
/// V22a 迁移会把 FACET_COND 换成 cfg_searchable 后再次重建。
fn migrate_v21(conn: &Connection) -> AppResult<()> {
    rebuild_fts_triggers_with_cond(
        conn,
        "AND EXISTS (SELECT 1 FROM tag_facets f WHERE f.key = t.facet_key AND f.status = 'active')",
        "status",
    )?;
    conn.execute_batch("INSERT INTO assets_fts(assets_fts) VALUES('rebuild');")?;
    Ok(())
}

// ════════════════════════════════════════════════════════════════════
// V22a（F1）无条件迁移段：分面能力矩阵 + 环/深度触发器 + review_state
// + AI 溯源列 + schema_features。不依赖数据干净，任何库都能安全推进。
//  ⚠ 新列一律 ALTER 追加；FACET_COLS / from_row 的索引跟着追加（铁律 2）
// ════════════════════════════════════════════════════════════════════

/// V22a 入口。推进 user_version=22 前必须完整成功；失败重启可重跑（幂等）。
fn migrate_v22a(conn: &Connection) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();

    // ── F1-a：tag_facets 配置值列（记录用户意图，生命周期状态永不覆盖它们）──
    add_column_if_missing(
        conn,
        "tag_facets",
        "cfg_visible_in_navigation",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "tag_facets",
        "cfg_manual_assignable",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "tag_facets",
        "cfg_ai_assignable",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "tag_facets",
        "cfg_searchable",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "tag_facets",
        "facet_kind",
        "TEXT NOT NULL DEFAULT 'tag'",
    )?;

    // 回填：manual_only → 不参与 AI（active 或 inactive 都算，保留用户原配置）
    conn.execute(
        "UPDATE tag_facets SET cfg_ai_assignable = 0 WHERE input_mode = 'manual_only'",
        [],
    )?;

    // ── F1-c：环检测 + 深度上限 + 父子同分面触发器 ──
    conn.execute_batch(
        r#"
-- 环检测（UPDATE parent_id）
CREATE TRIGGER IF NOT EXISTS trg_tags_no_cycle
  BEFORE UPDATE OF parent_id ON tags
  WHEN new.parent_id IS NOT NULL AND EXISTS (
    WITH RECURSIVE anc(id, d) AS (
      SELECT new.parent_id, 0
      UNION ALL
      SELECT t.parent_id, a.d + 1 FROM tags t JOIN anc a ON t.id = a.id
       WHERE t.parent_id IS NOT NULL AND a.d < 12
    ) SELECT 1 FROM anc WHERE id = new.id
  )
BEGIN SELECT RAISE(ABORT, '不能把标签挂到自己的子标签下（会形成循环）'); END;

-- 深度检查（UPDATE）：新父深度 + 被移动子树高度（不能只查新父深度）
CREATE TRIGGER IF NOT EXISTS trg_tags_max_depth_au
  BEFORE UPDATE OF parent_id ON tags
  WHEN new.parent_id IS NOT NULL
   AND (
     (WITH RECURSIVE anc(id, d) AS (
        SELECT new.parent_id, 1
        UNION ALL SELECT t.parent_id, a.d + 1 FROM tags t JOIN anc a ON t.id = a.id
         WHERE t.parent_id IS NOT NULL AND a.d < 12
      ) SELECT MAX(d) FROM anc)
     +
     (WITH RECURSIVE des(id, d) AS (
        SELECT new.id, 0
        UNION ALL SELECT t.id, s.d + 1 FROM tags t JOIN des s ON t.parent_id = s.id
         WHERE s.d < 12
      ) SELECT MAX(d) FROM des)
   ) > 8
BEGIN SELECT RAISE(ABORT, '移动后标签层级会超过 8 层（含其下所有子标签）'); END;

-- 深度检查（INSERT）：新节点无子树，只查新父深度 + 1
CREATE TRIGGER IF NOT EXISTS trg_tags_max_depth_ai
  BEFORE INSERT ON tags
  WHEN new.parent_id IS NOT NULL
   AND (WITH RECURSIVE anc(id, d) AS (
          SELECT new.parent_id, 1
          UNION ALL SELECT t.parent_id, a.d + 1 FROM tags t JOIN anc a ON t.id = a.id
           WHERE t.parent_id IS NOT NULL AND a.d < 12
        ) SELECT MAX(d) FROM anc) >= 8
BEGIN SELECT RAISE(ABORT, '标签层级不能超过 8 层'); END;

-- 父子同分面（INSERT + UPDATE 双向）
CREATE TRIGGER IF NOT EXISTS trg_tags_parent_facet_ai
  BEFORE INSERT ON tags
  WHEN new.parent_id IS NOT NULL
   AND (SELECT facet_key FROM tags WHERE id = new.parent_id) != new.facet_key
BEGIN SELECT RAISE(ABORT, '子标签必须与父标签属于同一分面'); END;

CREATE TRIGGER IF NOT EXISTS trg_tags_parent_facet_au
  BEFORE UPDATE OF parent_id, facet_key ON tags
  WHEN new.parent_id IS NOT NULL
   AND (SELECT facet_key FROM tags WHERE id = new.parent_id) != new.facet_key
BEGIN SELECT RAISE(ABORT, '子标签必须与父标签属于同一分面'); END;
"#,
    )?;

    // ── F1-e：schema_features 能力表 ──
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS schema_features (
  feature     TEXT PRIMARY KEY,
  enabled     INTEGER NOT NULL,
  applied_at  INTEGER,
  blocked_by  TEXT
);
"#,
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO schema_features VALUES ('tag_cycle_guard', 1, ?1, NULL)",
        [now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_features VALUES ('tag_unique_terms', 0, NULL, 'pending')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_features VALUES ('tag_facet_fk', 0, NULL, 'pending')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_features VALUES ('tag_facet_restrict_delete', 0, NULL, 'pending')",
        [],
    )?;

    // ── F1-f：asset_tags.review_state 三态（保守回填：把已确认的当已确认）──
    add_column_if_missing(
        conn,
        "asset_tags",
        "review_state",
        "TEXT NOT NULL DEFAULT 'ai_unreviewed'",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS ix_at_review ON asset_tags(review_state, tag_id, asset_id);",
    )?;
    // ① 手工的（source='manual' 只在用户确认/手工打标时写，历史也如此）
    conn.execute(
        "UPDATE asset_tags SET review_state = 'manual' WHERE source = 'manual'",
        [],
    )?;
    // ② AI 的但有 accepted/modified 的 suggestion item → 用户确认过
    conn.execute(
        "UPDATE asset_tags SET review_state = 'ai_reviewed'
          WHERE source != 'manual' AND review_state = 'ai_unreviewed' AND EXISTS (
            SELECT 1 FROM ai_suggestion_items i
              JOIN ai_suggestions s ON s.id = i.suggestion_id
             WHERE s.asset_id = asset_tags.asset_id AND i.tag_id = asset_tags.tag_id
               AND i.decision IN ('accepted','modified'))",
        [],
    )?;

    // ── F1-g：AI 溯源列（数据层，A2 接线用）──
    add_column_if_missing(conn, "ai_batches", "model_id", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "ai_batches", "model_version", "TEXT")?;
    add_column_if_missing(conn, "ai_batches", "profile_id", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(
        conn,
        "ai_batches",
        "prompt_version",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "ai_batches",
        "request_config_hash",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(conn, "ai_batches", "request_config_json", "TEXT")?;
    add_column_if_missing(conn, "ai_suggestions", "raw_response", "TEXT")?;
    add_column_if_missing(conn, "ai_suggestions", "analysis_json", "TEXT")?;
    add_column_if_missing(
        conn,
        "ai_suggestions",
        "analysis_schema_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;

    // F4（与 F1 同文件，并入 V22a 执行）：FTS 触发器条件从 status 换成
    // cfg_searchable（停用分面的标签仍可搜 —— 语义变更），改完立即 rebuild（铁律 3）
    rebuild_fts_triggers_with_cond(
        conn,
        "AND EXISTS (SELECT 1 FROM tag_facets f WHERE f.key = t.facet_key AND f.cfg_searchable = 1)",
        "cfg_searchable",
    )?;
    conn.execute_batch("INSERT INTO assets_fts(assets_fts) VALUES('rebuild');")?;
    Ok(())
}

// ════════════════════════════════════════════════════════════════════
// V22b（F2）条件迁移段：tag_terms 统一词条表 + facet_key 四道防线 +
// facet_key 引用完整性 + 分面删除 RESTRICT。
//   ⚠ 不随 migrate() 自动执行 —— 依赖数据干净，由 apply_tag_constraints
//     命令在预检通过后调用；全部幂等可重入。
// ════════════════════════════════════════════════════════════════════

/// F2-b：tag_terms 建表 + 唯一约束 + 索引。
/// ⚠ normalized_term 必须是默认 BINARY 排序，不许加 COLLATE（next_prefix 依赖字节序）。
fn create_tag_terms_table(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS tag_terms (
  tag_id          INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  facet_key       TEXT    NOT NULL,
  normalized_term TEXT    NOT NULL,
  term            TEXT    NOT NULL,
  locale          TEXT    NOT NULL DEFAULT '',
  term_kind       TEXT    NOT NULL,
  is_searchable   INTEGER NOT NULL DEFAULT 1,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (tag_id, normalized_term, locale),
  CHECK (term_kind IN ('canonical','synonym','old_name','translation','typo'))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_terms ON tag_terms(facet_key, normalized_term, locale);
CREATE UNIQUE INDEX IF NOT EXISTS ux_terms_canonical
  ON tag_terms(tag_id, term_kind) WHERE term_kind = 'canonical';
CREATE INDEX IF NOT EXISTS ix_terms_lookup
  ON tag_terms(normalized_term, locale, facet_key);
"#,
    )?;
    Ok(())
}

/// F2-b：把现有 tags（canonical）与 tag_aliases 灌进 tag_terms。
/// 单事务。只在 tag_terms 为空（首次 apply）时执行 —— 已有数据说明之前成功过，
/// 重跑直接跳过（避免与「新写入的词条」混淆；也保证冲突时 INSERT 会真实报错，
/// 不因 OR IGNORE 静默吞掉 —— 冲突必须让预检拦住，而不是靠 IGNORE 掩盖）。
fn backfill_tag_terms(conn: &Connection) -> AppResult<()> {
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM tag_terms", [], |r| r.get(0))?;
    if existing > 0 {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO tag_terms
           (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         SELECT id, facet_key, COALESCE(normalized_name, lower(trim(name))),
                COALESCE(canonical_name, name), '', 'canonical', 1, ?1
           FROM tags WHERE status = 'active'",
        [now],
    )?;
    tx.execute(
        "INSERT INTO tag_terms
           (tag_id, facet_key, normalized_term, term, locale, term_kind, is_searchable, created_at)
         SELECT ta.tag_id, t.facet_key, ta.normalized_alias, ta.alias, ta.locale,
                ta.alias_type, ta.is_searchable, ta.created_at
           FROM tag_aliases ta JOIN tags t ON t.id = ta.tag_id
          WHERE t.status = 'active'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// F2-c：facet_key 一致性四道防线（触发器 ①②③；④ check 在 tags.rs 校验层）。
/// ⚠ 触发器 ① 与 ③ 的交互：① 是 AFTER UPDATE ON tags，它执行的 UPDATE tag_terms
///    会触发 ③ 的 BEFORE UPDATE —— 此时 tags.facet_key 已是新值，③ 校验通过，顺序安全。
///    若日后把 ① 改成 BEFORE 就会死锁式互斥（建表 SQL 上写明，防后人调整时机）。
fn create_terms_facet_defenses(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
-- ① tags.facet_key 改动时自动同步 tag_terms（AFTER）
CREATE TRIGGER IF NOT EXISTS trg_terms_sync_facet
  AFTER UPDATE OF facet_key ON tags
BEGIN UPDATE tag_terms SET facet_key = new.facet_key WHERE tag_id = new.id; END;

-- ② 插入时校验（BEFORE INSERT）
CREATE TRIGGER IF NOT EXISTS trg_terms_facet_match_ai
  BEFORE INSERT ON tag_terms
  WHEN new.facet_key != (SELECT facet_key FROM tags WHERE id = new.tag_id)
BEGIN SELECT RAISE(ABORT, 'tag_terms.facet_key 必须与 tags.facet_key 一致'); END;

-- ③ 直接改 tag_terms.facet_key 时校验（BEFORE UPDATE）
CREATE TRIGGER IF NOT EXISTS trg_terms_facet_match_au
  BEFORE UPDATE OF facet_key ON tag_terms
  WHEN new.facet_key != (SELECT facet_key FROM tags WHERE id = new.tag_id)
BEGIN SELECT RAISE(ABORT, 'tag_terms.facet_key 必须与 tags.facet_key 一致'); END;
"#,
    )?;
    Ok(())
}

/// F2-d：facet_key 引用完整性 + 分面删除 RESTRICT。
/// ⚠ delete_facet 的级联必须在同一事务内先删 tags 再删 tag_facets（现有顺序已正确），
///   否则会被 trg_facets_restrict_delete 拦住。
fn create_facet_fk_guards(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TRIGGER IF NOT EXISTS trg_tags_facet_fk_ai
  BEFORE INSERT ON tags
  WHEN NOT EXISTS (SELECT 1 FROM tag_facets WHERE key = new.facet_key)
BEGIN SELECT RAISE(ABORT, '标签的 facet_key 指向不存在的分面'); END;

CREATE TRIGGER IF NOT EXISTS trg_tags_facet_fk_au
  BEFORE UPDATE OF facet_key ON tags
  WHEN NOT EXISTS (SELECT 1 FROM tag_facets WHERE key = new.facet_key)
BEGIN SELECT RAISE(ABORT, '标签的 facet_key 指向不存在的分面'); END;

CREATE TRIGGER IF NOT EXISTS trg_facets_restrict_delete
  BEFORE DELETE ON tag_facets
  WHEN EXISTS (SELECT 1 FROM tags WHERE facet_key = old.key)
     OR EXISTS (SELECT 1 FROM asset_facet_numbers WHERE facet_key = old.key)
BEGIN SELECT RAISE(ABORT, '该分面下还有标签或数值，请用 delete_tag_facet 命令'); END;
"#,
    )?;
    Ok(())
}

/// V22b 全套（F2-b/c/d）。调用方必须先跑预检（tags::detect_tag_conflicts）确认干净。
/// 幂等：全部 IF NOT EXISTS + INSERT OR IGNORE，可安全重跑。
/// 注：各子步骤内部自带事务（backfill_tag_terms），此处不再包外层事务防嵌套。
pub fn apply_v22b_constraints(conn: &Connection) -> AppResult<()> {
    create_tag_terms_table(conn)?;
    backfill_tag_terms(conn)?;
    create_terms_facet_defenses(conn)?;
    create_facet_fk_guards(conn)?;
    Ok(())
}

/// P0-4：gate 开启后重建 FTS 触发器与索引 —— 别名词源从 tag_aliases 切到 tag_terms，
/// 并全量 rebuild 对齐内容表。调用方必须在 set_feature("tag_unique_terms", true) 之后调用
/// （rebuild_fts_triggers_with_cond 按 feature 开关决定词源）。幂等可重跑。
pub fn rebuild_fts_triggers_for_gated_terms(conn: &Connection) -> AppResult<()> {
    rebuild_fts_triggers_with_cond(
        conn,
        "AND EXISTS (SELECT 1 FROM tag_facets f WHERE f.key = t.facet_key AND f.cfg_searchable = 1)",
        "cfg_searchable",
    )?;
    conn.execute_batch("INSERT INTO assets_fts(assets_fts) VALUES('rebuild');")?;
    Ok(())
}

/// 测试辅助：只建 tag_terms 表（不灌数据、不加防御触发器）。
pub fn create_tag_terms_table_for_test(conn: &Connection) -> AppResult<()> {
    create_tag_terms_table(conn)
}

/// 测试辅助：只建 facet_key 防御触发器。
pub fn create_terms_facet_defenses_for_test(conn: &Connection) -> AppResult<()> {
    create_terms_facet_defenses(conn)
}

pub fn migrate(conn: &Connection) -> AppResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        // W0-8：V1 建库包事务。全裸 CREATE TABLE 非幂等，首装中途崩溃 → 重跑报
        // "table already exists" → 启动永久阻断。事务保证要么全建成功要么全无。
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.commit()?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        // B37：逐列容错 ALTER，幂等可重入
        migrate_v2(conn)?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        // 先完成重建再提交 user_version=3：中途崩溃重启能重跑（V3 天然幂等）
        conn.execute_batch(SCHEMA_V3)?;
        conn.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        conn.execute_batch(SCHEMA_V4)?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        migrate_v5(conn)?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    if version < 6 {
        migrate_v6(conn)?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    if version < 7 {
        migrate_v7(conn)?;
        conn.pragma_update(None, "user_version", 7)?;
    }
    if version < 8 {
        migrate_v8(conn)?;
        conn.pragma_update(None, "user_version", 8)?;
    }
    if version < 9 {
        conn.execute_batch(SCHEMA_V9)?;
        conn.pragma_update(None, "user_version", 9)?;
    }
    if version < 10 {
        // V10：旧 tagCategories（中文名机器协议）→ ai_facet_configs（稳定 facet_key）。
        // 设置在键值表存 JSON，这里读出来转换后写回；幂等（已转则不变）。
        super::settings::normalize_settings_persist(conn)?;
        conn.pragma_update(None, "user_version", 10)?;
    }
    if version < 11 {
        // V11：独立 color 分面补齐（新库/存量库都成立），幂等
        migrate_v11(conn)?;
        conn.pragma_update(None, "user_version", 11)?;
    }
    if version < 12 {
        // V12：媒体元数据结构化字段（只增列，不扫描/回填视频）
        migrate_v12(conn)?;
        conn.pragma_update(None, "user_version", 12)?;
    }
    if version < 13 {
        // V13：标签分面生命周期字段（applies_to + 状态放宽）
        migrate_v13(conn)?;
        conn.pragma_update(None, "user_version", 13)?;
    }
    if version < 14 {
        // V14：视频兼容代理缓存表（queued|running|ready|failed|canceled）
        migrate_v14(conn)?;
        conn.pragma_update(None, "user_version", 14)?;
    }
    if version < 15 {
        // V15：AI 连接档案 + 用途绑定（keyring 凭据迁移；§6.3/§7.3）
        migrate_v15(conn)?;
        conn.pragma_update(None, "user_version", 15)?;
    }
    if version < 16 {
        // FB2-08（§14.8）：颜色改为算法主色属性 + color 分面停用（AI 侧摘除）
        migrate_v16(conn)?;
        conn.pragma_update(None, "user_version", 16)?;
    }
    if version < 17 {
        // FB5-05（§7.2）：一句话描述列 + FTS 三列重建 + 触发器升级
        migrate_v17(conn)?;
        conn.pragma_update(None, "user_version", 17)?;
    }
    if version < 18 {
        // GPS 定位属性：latitude/longitude 列 + 索引（只增列，不回填；回填走 media_refill）
        migrate_v18(conn)?;
        conn.pragma_update(None, "user_version", 18)?;
    }
    if version < 19 {
        migrate_v19(conn)?;
        conn.pragma_update(None, "user_version", 19)?;
    }
    if version < 20 {
        // V20：分面合表（低风险段）。V21 FTS 触发器独立版本号，失败可单独延后。
        migrate_v20(conn)?;
        conn.pragma_update(None, "user_version", 20)?;
    }
    if version < 21 {
        // V21 = FTS 触发器（高风险）。独立版本号使其可单独延后。
        migrate_v21(conn)?;
        conn.pragma_update(None, "user_version", 21)?;
    }
    if version < 22 {
        // V22a（F1 无条件段）：能力列 + 环/深度触发器 + review_state + 溯源列。
        // V22b（F2 条件段，冲突跳过）也在 22 内，见 apply_tag_constraints。
        migrate_v22a(conn)?;
        conn.pragma_update(None, "user_version", 22)?;
    }
    // V23（C-1）：色板关系表 —— 无条件幂等段（CREATE IF NOT EXISTS），
    // 不推进 user_version（既有版本号不可改；存量库每次启动自愈补齐）。
    migrate_v23(conn)?;
    // V24（§6）：数值化分面 —— tag_facets num_* 五列 + asset_facet_numbers 载荷表
    // + ai_suggestion_items 数值两列。无条件幂等段（逐列容错 ALTER + IF NOT EXISTS），
    // 不推进 user_version（与 V23 同理；存量库每次启动自愈补齐）。
    migrate_v24(conn)?;
    // V25：在线连接限额字段。无版本幂等段，旧库启动时自动补齐。
    migrate_v25(conn)?;
    // V26：视频代理缓存指纹。无版本幂等追加；旧 ready 代理的 NULL 指纹会触发安全重建。
    migrate_v26_video_proxy_fingerprints(conn)?;
    // 产品默认修正：用途和可用性/技术特征需要人工判断，不再进入 AI 提示词。
    migrate_manual_only_system_facets(conn)?;
    // 风格/氛围无法由 AI 稳定判断，且历史结果几乎全是「未知」；彻底下线该系统分面。
    migrate_remove_style_facet(conn)?;
    // 用途/技术特征不再是产品预置分类；空默认行移除，有历史数据或自定义配置则保留为用户分类。
    migrate_remove_legacy_manual_system_facets(conn)?;
    Ok(())
}

/// V24（§6.3）：数值化分面。幂等：
/// ① tag_facets 追加五列（num_min/num_max/num_unit/num_decimals/num_step）—— 逐列探测再 ALTER；
/// ② asset_facet_numbers 载荷表（PK (asset_id, facet_key)，覆盖索引）；
/// ③ ai_suggestion_items 追加 item_kind/num_value 两列。
/// facet_kind 只做应用层校验，不加 DB CHECK（§10 决策 6：重建表不值得）。
fn migrate_v24(conn: &Connection) -> AppResult<()> {
    // ① tag_facets 五列（铁律 2：追加末尾，FACET_COLS 同步）
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(tag_facets)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for (ddl, col) in [
        ("ALTER TABLE tag_facets ADD COLUMN num_min REAL", "num_min"),
        ("ALTER TABLE tag_facets ADD COLUMN num_max REAL", "num_max"),
        (
            "ALTER TABLE tag_facets ADD COLUMN num_unit TEXT NOT NULL DEFAULT ''",
            "num_unit",
        ),
        (
            "ALTER TABLE tag_facets ADD COLUMN num_decimals INTEGER NOT NULL DEFAULT 0",
            "num_decimals",
        ),
        (
            "ALTER TABLE tag_facets ADD COLUMN num_step REAL NOT NULL DEFAULT 1",
            "num_step",
        ),
    ] {
        if !cols.iter().any(|c| c == col) {
            conn.execute_batch(ddl)?;
        }
    }
    // ② 数值载荷表（不做通用 EAV：单 REAL 窄表）
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS asset_facet_numbers (
  asset_id        INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  facet_key       TEXT    NOT NULL,
  value           REAL    NOT NULL,
  source          TEXT    NOT NULL DEFAULT 'manual',
  review_state    TEXT    NOT NULL DEFAULT 'ai_unreviewed',
  source_batch_id INTEGER,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (asset_id, facet_key)
);
CREATE INDEX IF NOT EXISTS ix_afn_value ON asset_facet_numbers(facet_key, value, asset_id);
"#,
    )?;
    // ③ 建议项数值两列
    let item_cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(ai_suggestion_items)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    if !item_cols.iter().any(|c| c == "item_kind") {
        conn.execute_batch(
            "ALTER TABLE ai_suggestion_items ADD COLUMN item_kind TEXT NOT NULL DEFAULT 'tag'",
        )?;
    }
    if !item_cols.iter().any(|c| c == "num_value") {
        conn.execute_batch("ALTER TABLE ai_suggestion_items ADD COLUMN num_value REAL")?;
    }
    Ok(())
}

/// V25：在线连接限流字段。无版本幂等段，兼容当前 V22 user_version 与 V23/V24
/// 的存量自愈迁移约定；只追加列，不改变既有连接的默认行为。
fn migrate_v25(conn: &Connection) -> AppResult<()> {
    for (column, sql) in [
        (
            "max_concurrency",
            "ALTER TABLE ai_connections ADD COLUMN max_concurrency INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "requests_per_minute",
            "ALTER TABLE ai_connections ADD COLUMN requests_per_minute INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "requests_per_hour",
            "ALTER TABLE ai_connections ADD COLUMN requests_per_hour INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !has_column(conn, "ai_connections", column)? {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

/// V26（视频代理缓存指纹）：只追加新列，不修改已发布的 V14 表迁移。
/// 旧 ready 记录保留 NULL 指纹，服务层因此将其判为过期并从源文件重建。
fn migrate_v26_video_proxy_fingerprints(conn: &Connection) -> AppResult<()> {
    for (column, sql) in [
        (
            "source_fingerprint",
            "ALTER TABLE video_proxies ADD COLUMN source_fingerprint TEXT",
        ),
        (
            "encoder_version",
            "ALTER TABLE video_proxies ADD COLUMN encoder_version INTEGER",
        ),
        (
            "tool_fingerprint",
            "ALTER TABLE video_proxies ADD COLUMN tool_fingerprint TEXT",
        ),
        (
            "source_path",
            "ALTER TABLE video_proxies ADD COLUMN source_path TEXT",
        ),
    ] {
        if !has_column(conn, "video_proxies", column)? {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

/// 用独立标记只修正一次存量库；之后用户若主动移回 AI 组，不会在重启时被覆盖。
fn migrate_manual_only_system_facets(conn: &Connection) -> AppResult<()> {
    let applied: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='manual_only_system_facets_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if applied.as_deref() == Some("1") {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "UPDATE tag_facets
            SET cfg_ai_assignable = 0, input_mode = 'manual_only', updated_at = ?1
          WHERE is_system = 1 AND key IN ('purpose', 'technical')",
        [now],
    )?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('manual_only_system_facets_v1', '1')
         ON CONFLICT(key) DO UPDATE SET value='1'",
        [],
    )?;
    Ok(())
}

/// 一次性移除 `style / 风格/氛围` 系统分面，并级联清理标签、素材关联、建议项和流水。
/// 标记保证迁移只执行一次；以后用户若主动创建同名自定义分面，不会被启动迁移再次删除。
fn migrate_remove_style_facet(conn: &Connection) -> AppResult<()> {
    let applied: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='style_facet_removed_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if applied.as_deref() == Some("1") {
        return Ok(());
    }

    let is_system_style: Option<i64> = conn
        .query_row(
            "SELECT is_system FROM tag_facets WHERE key='style'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if is_system_style == Some(1) {
        crate::db::tag_facets::delete_facet_cascade(conn, "style")?;
    }
    // 分面级联只清 ai_suggestion_items；历史建议 JSON 仍会被工作台读取，必须一并移除 style。
    conn.execute(
        "UPDATE ai_suggestions
            SET suggested_tags = json_remove(suggested_tags, '$.style'),
                confirmed_tags = CASE
                    WHEN confirmed_tags IS NULL THEN NULL
                    ELSE json_remove(confirmed_tags, '$.style')
                END
          WHERE json_type(suggested_tags, '$.style') IS NOT NULL
             OR (confirmed_tags IS NOT NULL AND json_type(confirmed_tags, '$.style') IS NOT NULL)",
        [],
    )?;

    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('style_facet_removed_v1', '1')
         ON CONFLICT(key) DO UPDATE SET value='1'",
        [],
    )?;
    Ok(())
}

/// 将旧版内置的 purpose/technical 分类移出默认系统集合。
/// 该修复通过 is_system 状态幂等：只处理仍标记为系统的旧行，不触碰用户后来创建的同 key 分类。
fn migrate_remove_legacy_manual_system_facets(conn: &Connection) -> AppResult<()> {
    for (key, display_name, description, max_items, sort_order) in [
        ("purpose", "用途", "稳定的发布或设计用途", 3, 30),
        (
            "technical",
            "可用性/技术特征",
            "透明背景、可裁切等非文件格式属性",
            4,
            90,
        ),
    ] {
        let Some(facet) = crate::db::tag_facets::list_all(conn)?
            .into_iter()
            .find(|facet| facet.key == key && facet.is_system)
        else {
            continue;
        };

        let has_data: bool = conn.query_row(
            "SELECT
                EXISTS(SELECT 1 FROM tags WHERE facet_key = ?1)
                OR EXISTS(SELECT 1 FROM asset_facet_numbers WHERE facet_key = ?1)
                OR EXISTS(SELECT 1 FROM ai_suggestion_items WHERE facet_key = ?1)
                OR EXISTS(SELECT 1 FROM ai_suggestions WHERE
                    CASE WHEN json_valid(suggested_tags)
                         THEN json_type(suggested_tags, '$.' || ?1) IS NOT NULL ELSE 0 END
                    OR CASE WHEN json_valid(confirmed_tags)
                            THEN json_type(confirmed_tags, '$.' || ?1) IS NOT NULL ELSE 0 END)",
            [key],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )?;
        let is_unchanged_default = facet.display_name == display_name
            && facet.description == description
            && facet.selection_mode == "multi"
            && facet.max_items == Some(max_items)
            && facet.sort_order == sort_order
            && facet.status == "active"
            && facet.applies_to == "all"
            && facet.input_mode == "manual_only"
            && facet.cfg_visible_in_navigation
            && facet.cfg_manual_assignable
            && !facet.cfg_ai_assignable
            && facet.cfg_searchable
            && facet.facet_kind == "tag"
            && facet.num_min.is_none()
            && facet.num_max.is_none()
            && facet.num_unit.is_empty()
            && facet.num_decimals == 0
            && facet.num_step == 1.0;

        if has_data || !is_unchanged_default {
            conn.execute(
                "UPDATE tag_facets SET is_system = 0, updated_at = ?1 WHERE key = ?2 AND is_system = 1",
                rusqlite::params![chrono::Utc::now().timestamp_millis(), key],
            )?;
            tracing::info!(
                facet_key = key,
                "旧系统分类包含用户数据或自定义配置，已保留为用户分类"
            );
        } else {
            crate::db::tag_facets::delete_facet_cascade(conn, key)?;
            tracing::info!(facet_key = key, "已移除未使用的旧系统默认分类");
        }
    }
    Ok(())
}

/// V23（C-1）：色板关系表 —— 色名分桶查询（rank/ratio 是字符串方案表达不了的维度）。
/// 幂等：CREATE TABLE IF NOT EXISTS；旧库首次启动从 palette_json 自动补齐。
fn migrate_v23(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS asset_palette_colors (
  asset_id     INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  rank         INTEGER NOT NULL,        -- 0 = 主色，按 ratio 降序
  color_bucket INTEGER NOT NULL,        -- 色名分桶，与 colorName.ts 分段一一对应（见 db/palette_bucket.rs）
  ratio        REAL    NOT NULL,        -- 该色占比 0..1
  PRIMARY KEY (asset_id, rank)
);
CREATE INDEX IF NOT EXISTS ix_apc_bucket ON asset_palette_colors(color_bucket, rank, asset_id);
"#,
    )?;
    // 旧库首次具备关系表时，从已有色板补齐；完成标记避免每次启动重复全量重建。
    let indexed: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='palette_colors_indexed_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if indexed.as_deref() != Some("1") {
        super::assets::rescan_palette_colors(conn)?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('palette_colors_indexed_v1', '1')
             ON CONFLICT(key) DO UPDATE SET value='1'",
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c
    }

    #[test]
    fn v25_adds_connection_rate_limit_columns_idempotently() {
        let c = mem();
        c.execute_batch(
            "CREATE TABLE ai_connections (
               id TEXT PRIMARY KEY, name TEXT NOT NULL,
               deployment TEXT NOT NULL, protocol TEXT NOT NULL,
               base_url TEXT NOT NULL, model TEXT NOT NULL,
               api_key_ref TEXT, enabled INTEGER NOT NULL DEFAULT 1,
               created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        migrate_v25(&c).unwrap();
        migrate_v25(&c).unwrap();
        for column in [
            "max_concurrency",
            "requests_per_minute",
            "requests_per_hour",
        ] {
            assert!(has_column(&c, "ai_connections", column).unwrap());
        }
        c.execute(
            "INSERT INTO ai_connections
             (id, name, deployment, protocol, base_url, model, created_at, updated_at)
             VALUES ('c', 'C', 'cloud', 'openai_chat', 'u', 'm', 1, 1)",
            [],
        )
        .unwrap();
        let limits: (i64, i64, i64) = c
            .query_row(
                "SELECT max_concurrency, requests_per_minute, requests_per_hour
                 FROM ai_connections WHERE id='c'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(limits, (0, 0, 0));
    }

    #[test]
    fn v26_adds_video_proxy_fingerprints_idempotently_and_leaves_old_rows_stale() -> AppResult<()> {
        let c = mem();
        c.execute_batch(
            "CREATE TABLE assets (id INTEGER PRIMARY KEY, file_path TEXT);
             CREATE TABLE video_proxies (
               asset_id INTEGER NOT NULL,
               variant TEXT NOT NULL DEFAULT 'h264_mp4',
               status TEXT NOT NULL DEFAULT 'queued',
               path TEXT,
               error TEXT,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               PRIMARY KEY (asset_id, variant));
             INSERT INTO assets (id, file_path) VALUES (1, '/v.mp4');
             INSERT INTO video_proxies
               (asset_id, variant, status, path, created_at, updated_at)
               VALUES (1, 'h264_mp4', 'ready', '/proxy/1.mp4', 1, 1);",
        )?;

        migrate_v26_video_proxy_fingerprints(&c)?;
        migrate_v26_video_proxy_fingerprints(&c)?;

        for column in [
            "source_fingerprint",
            "encoder_version",
            "tool_fingerprint",
            "source_path",
        ] {
            assert!(has_column(&c, "video_proxies", column)?);
        }
        let fingerprints: (Option<String>, Option<i64>, Option<String>, Option<String>) = c
            .query_row(
                "SELECT source_fingerprint, encoder_version, tool_fingerprint, source_path
                 FROM video_proxies WHERE asset_id=1 AND variant='h264_mp4'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(fingerprints, (None, None, None, None));
        Ok(())
    }

    /// 生成一份「已到 v14」的旧库 fixture：直接建必要表 + settings JSON + user_version=14。
    /// 凭据后端：不强制 mock（避免与 credentials 测试竞态全局 builder）——
    /// 迁移行为按「keyring 可用/不可用」两种情形分别断言（下同）。
    fn legacy_v14_fixture() -> rusqlite::Connection {
        let c = mem();
        // 只需 settings 表 + 旧 app_settings JSON；迁移只依赖这两者
        c.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE ai_connections (
               id TEXT PRIMARY KEY, name TEXT NOT NULL,
               deployment TEXT NOT NULL CHECK (deployment IN ('cloud','local')),
               protocol TEXT NOT NULL CHECK (protocol IN ('openai_chat','anthropic_messages')),
               base_url TEXT NOT NULL, model TEXT NOT NULL,
               api_key_ref TEXT, enabled INTEGER NOT NULL DEFAULT 1,
               created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE ai_usage_bindings (
               usage TEXT PRIMARY KEY CHECK (usage IN ('super_search','tagging')),
               connection_id TEXT NOT NULL REFERENCES ai_connections(id),
               updated_at INTEGER NOT NULL);",
        )
        .unwrap();
        c.pragma_update(None, "user_version", 14).unwrap();
        c
    }

    /// v15 迁移：每个 profile 迁入连接表、apiMode→protocol 映射正确、active 绑定两用途。
    /// keyring 行为分两种合法结果并分别断言：
    ///  - 可用：api_key_ref 有值，旧 JSON 明文被清除，备份副本保留；
    ///  - 不可用：api_key_ref 空，旧 JSON 明文保留（不丢 key）。
    #[test]
    fn v15_migrates_old_profiles_into_connections_and_preserves_keys() {
        let c = legacy_v14_fixture();
        let old_json = r#"{
          "ai": {
            "profiles": [
              {"id":"p1","name":"通义","apiMode":"openai","kind":"cloud","baseUrl":"https://a/v1","apiKey":"sk-a","model":"qwen-max"},
              {"id":"p2","name":"本地Ollama","apiMode":"openai","kind":"local","baseUrl":"http://localhost:11434/v1","apiKey":"","model":"llama3.2-vision"},
              {"id":"p3","name":"Claude","apiMode":"anthropic","kind":"cloud","baseUrl":"https://api.anthropic.com","apiKey":"sk-b","model":"claude-3"}
            ],
            "activeProfile": "p1"
          }
        }"#;
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app_settings', ?1)",
            [old_json],
        )
        .unwrap();

        migrate_v15(&c).unwrap();

        // p1: cloud + openai→openai_chat
        let (deploy, proto) = c
            .query_row(
                "SELECT deployment, protocol FROM ai_connections WHERE id='p1'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(deploy, "cloud");
        assert_eq!(proto, "openai_chat");
        // p2: local
        let deploy2: String = c
            .query_row(
                "SELECT deployment FROM ai_connections WHERE id='p2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deploy2, "local");
        // p3: anthropic→anthropic_messages
        let proto3: String = c
            .query_row(
                "SELECT protocol FROM ai_connections WHERE id='p3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proto3, "anthropic_messages");

        // active profile 绑定两个用途（与 keyring 可用性无关）
        let usages: Vec<String> = c
            .prepare("SELECT usage FROM ai_usage_bindings ORDER BY usage")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(usages, vec!["super_search", "tagging"]);
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM ai_connections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        // 分情形断言密钥迁移结果（二选一，均合法）
        let ref1: Option<String> = c
            .query_row(
                "SELECT api_key_ref FROM ai_connections WHERE id='p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let kept: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='app_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let backup: Option<String> = c
            .query_row(
                "SELECT value FROM settings WHERE key='app_settings_backup_v15'",
                [],
                |r| r.get(0),
            )
            .ok();
        if ref1.is_some() {
            // keyring 可用：明文清除 + 备份副本保留
            assert!(
                !kept.contains("sk-a"),
                "迁移成功后 settings JSON 不得再含明文 API Key"
            );
            assert!(!kept.contains("sk-b"));
            assert!(
                backup.is_some() && backup.unwrap().contains("sk-a"),
                "备份副本应保留原始明文"
            );
        } else {
            // keyring 不可用：旧明文保留（不丢 key），迁移仍完成
            assert!(kept.contains("sk-a"), "keyring 写失败时旧明文 key 必须保留");
            assert!(kept.contains("sk-b"));
        }
    }

    /// 幂等：重复执行无副作用（连接表非空即跳过）。
    #[test]
    fn v15_is_idempotent() {
        let c = legacy_v14_fixture();
        let old_json = r#"{"ai":{"profiles":[{"id":"p1","name":"A","apiMode":"openai","kind":"cloud","baseUrl":"u","model":"m"}],"activeProfile":"p1"}}"#;
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app_settings', ?1)",
            [old_json],
        )
        .unwrap();
        migrate_v15(&c).unwrap();
        migrate_v15(&c).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM ai_connections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "重复执行不得重复插入连接");
    }

    /// 空 settings（无 app_settings 行）不报错。
    #[test]
    fn v15_no_settings_is_noop() {
        let c = legacy_v14_fixture();
        migrate_v15(&c).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM ai_connections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    /// 迁移后 foreign_key_check 无错误（表结构约束合法）。
    #[test]
    fn v15_passes_foreign_key_check() {
        let c = legacy_v14_fixture();
        let old_json = r#"{"ai":{"profiles":[{"id":"p1","name":"A","apiMode":"openai","kind":"cloud","baseUrl":"u","model":"m"}],"activeProfile":"p1"}}"#;
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app_settings', ?1)",
            [old_json],
        )
        .unwrap();
        migrate_v15(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA foreign_key_check").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(rows.count(), 0, "foreign_key_check 应无错误");
    }

    /// FB2-08：V16 —— 6 列 + 3 索引、color 分面停用、settings 里 color 关 AI / style hint 追加；幂等。
    /// 用 init_memory()（跑完整 migrate 链）而不是手写 fixture：手写 fixture 与真实 schema 漂移，
    /// 正是 FX-01（migrate_v16 写不存在的 tag_facets.hint）逃过测试的原因。
    #[test]
    fn v16_adds_palette_columns_and_deactivates_color_facet() {
        let c = crate::db::init_memory().unwrap(); // 已跑到最新版本（V16 → V17 链上）
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 16, "全新库应至少迁移到 V16，实际 {v}");

        for col in [
            "palette_json",
            "palette_version",
            "palette_scanned_at",
            "dominant_hue",
            "dominant_sat",
            "dominant_lum",
        ] {
            assert!(has_column(&c, "assets", col).unwrap(), "列 {col} 应存在");
        }
        for idx in [
            "idx_assets_dominant_hue",
            "idx_assets_dominant_sat",
            "idx_assets_dominant_lum",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "索引 {idx} 应存在");
        }
        let st: String = c
            .query_row("SELECT status FROM tag_facets WHERE key='color'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(st, "inactive");

        // 幂等：重复执行不报错、不改变现状、hint 不重复追加
        migrate_v16(&c).unwrap();
        migrate_v16(&c).unwrap();
        let st2: String = c
            .query_row("SELECT status FROM tag_facets WHERE key='color'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(st2, "inactive");
    }

    /// FX-01 回归：存量库（V15 + 已有 aiFacetConfigs）跑 V16 后，
    /// settings 里 color 关 AI、style hint 追加一次且仅一次。
    #[test]
    fn v16_patches_settings_json_not_tag_facets_table() {
        let c = crate::db::init_memory().unwrap();
        let json = r#"{"aiFacetConfigs":[
            {"facetKey":"color","enabledForAi":true,"hint":"主色"},
            {"facetKey":"style","enabledForAi":true,"hint":"如胶片感"}]}"#;
        c.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('app_settings', ?1)",
            [json],
        )
        .unwrap();

        migrate_v16(&c).unwrap();
        migrate_v16(&c).unwrap(); // 跑两次验幂等

        let raw: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='app_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let arr = v["aiFacetConfigs"].as_array().unwrap();
        let color = arr.iter().find(|x| x["facetKey"] == "color").unwrap();
        assert_eq!(color["enabledForAi"], serde_json::Value::Bool(false));
        assert_eq!(color["visibleInWorkbench"], serde_json::Value::Bool(false));
        let style_hint = arr.iter().find(|x| x["facetKey"] == "style").unwrap()["hint"]
            .as_str()
            .unwrap();
        assert!(
            style_hint.contains("不包含颜色"),
            "style hint 应追加消歧说明，实际：{style_hint}"
        );
        assert_eq!(
            style_hint.matches("不包含颜色").count(),
            1,
            "重复执行不得重复追加"
        );
        // tag_facets 表不得因此长出 hint 列
        assert!(
            !has_column(&c, "tag_facets", "hint").unwrap(),
            "hint 属于 settings JSON，不是表列"
        );
    }

    /// FX-01 回归：无 app_settings 行（全新库尚未存过设置）时 V16 不报错。
    #[test]
    fn v16_no_settings_row_is_noop() {
        let c = crate::db::init_memory().unwrap();
        c.execute("DELETE FROM settings WHERE key='app_settings'", [])
            .unwrap();
        migrate_v16(&c).unwrap();
    }

    // ── FB5-05（§13.6）：V17 与一句话描述 ──

    /// V17：三实体列 + FTS 三列 + trg_assets_au 升级为 UPDATE OF file_name, content_description；
    /// 全新库迁移链直达 V17。
    #[test]
    fn v17_migrates_fresh_db_to_17() {
        let c = crate::db::init_memory().unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 17, "全新库应至少迁移到 V17，实际 {v}");

        assert!(has_column(&c, "assets", "content_description").unwrap());
        assert!(has_column(&c, "ai_suggestions", "suggested_description").unwrap());
        assert!(has_column(&c, "ai_suggestions", "confirmed_description").unwrap());
        assert!(has_column(&c, "fts_content", "content_description").unwrap());

        // FTS 虚表三列（+ rowid）
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('assets_fts')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(cols, vec!["file_name", "tag_names", "content_description"]);

        // trg_assets_au 明确包含 UPDATE OF file_name, content_description
        let sql: String = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='trg_assets_au'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql.contains("UPDATE OF file_name, content_description"),
            "trg_assets_au 应为两列触发：{sql}"
        );
    }

    /// V17：描述写入 assets → 触发器同步 fts_content（bigram）→ FTS 可命中；
    /// file_name 更新仍触发（回归）。
    #[test]
    fn v17_description_flows_into_fts() {
        let c = crate::db::init_memory().unwrap();
        // 插入素材（assets 触发器写 fts_content）
        c.execute(
            "INSERT INTO assets (file_path, file_name, content_description, file_ext, file_size, mime_type, created_at, modified_at) VALUES ('/a/1.jpg', 'IMG_1001.jpg', '夜晚树下多人合影', '.jpg', 100, 'image/jpeg', 1, 1)",
            [],
        )
        .unwrap();
        let fts_desc: String = c
            .query_row("SELECT content_description FROM fts_content", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            fts_desc, "夜 晚 树 下 多 人 合 影",
            "fts_content 应存 bigram 描述"
        );
        // FTS 命中
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM assets_fts WHERE assets_fts MATCH '\"夜 晚\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "描述词应可被 FTS 命中");

        // 更新描述 → 触发器刷新
        c.execute(
            "UPDATE assets SET content_description = '白天海边合影' WHERE file_name = 'IMG_1001.jpg'",
            [],
        )
        .unwrap();
        let fts_desc2: String = c
            .query_row("SELECT content_description FROM fts_content", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fts_desc2, "白 天 海 边 合 影");
        let n2: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM assets_fts WHERE assets_fts MATCH '\"海 边\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 1);

        // 描述为空 → 回退空串，不报错
        c.execute(
            "UPDATE assets SET content_description = '' WHERE file_name = 'IMG_1001.jpg'",
            [],
        )
        .unwrap();
        let fts_desc3: String = c
            .query_row("SELECT content_description FROM fts_content", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fts_desc3, "");
    }

    /// V17：重复跑迁移幂等；模拟「写完数据但未写 user_version=17」的中断 →
    /// 重跑 migrate() 后虚表/触发器恢复、业务数据不丢。
    #[test]
    fn v17_idempotent_and_crash_recoverable() {
        let c = crate::db::init_memory().unwrap();
        c.execute(
            "INSERT INTO assets (file_path, file_name, content_description, file_ext, file_size, mime_type, created_at, modified_at) VALUES ('/a/2.jpg', 'keep.jpg', '保留描述', '.jpg', 100, 'image/jpeg', 1, 1)",
            [],
        )
        .unwrap();

        // 幂等：直接再跑两次（FTS5 外部内容表：重建后先 rebuild 一次，保证回填 UPDATE
        // 的 delete 命令命中既有行，否则 SQLITE_CORRUPT_VTAB(267)）
        migrate_v17(&c).unwrap();
        migrate_v17(&c).unwrap();

        // 模拟中断：数据已完成但 user_version 未写 17 → 下次启动重跑整条 migrate 链
        c.pragma_update(None, "user_version", 16).unwrap();
        crate::db::migrations::migrate(&c).unwrap();

        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 17, "重跑后应至少补写到 V17，实际 {v}");
        // 业务数据不丢
        let (keep, desc): (String, String) = c
            .query_row(
                "SELECT file_name, content_description FROM assets WHERE file_name='keep.jpg'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(keep, "keep.jpg");
        assert_eq!(desc, "保留描述");
        // 虚表与触发器恢复（FTS 仍可命中）
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM assets_fts WHERE assets_fts MATCH '\"保 留\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "重跑后 FTS 应可命中描述");
    }

    // ── GPS 定位属性：V18 ──

    /// V18：全新库迁移链直达 V18，assets 有 latitude/longitude 两列与各自索引；
    /// 重复执行幂等。
    #[test]
    fn v18_adds_geo_columns_and_indexes() {
        let c = crate::db::init_memory().unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 18, "全新库应至少迁移到 V18（实际 {v}）");

        assert!(
            has_column(&c, "assets", "latitude").unwrap(),
            "latitude 列应存在"
        );
        assert!(
            has_column(&c, "assets", "longitude").unwrap(),
            "longitude 列应存在"
        );
        for idx in ["idx_assets_latitude", "idx_assets_longitude"] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "索引 {idx} 应存在");
        }

        // 幂等：重复执行不报错、不重复建索引
        migrate_v18(&c).unwrap();
        migrate_v18(&c).unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_assets_latitude'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "重复执行不得重复建索引");
    }

    /// V18：老素材无定位时两列为 NULL（向后兼容契约），可正常读写。
    #[test]
    fn v18_geo_columns_default_null_and_writable() {
        let c = crate::db::init_memory().unwrap();
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES ('/a/geo.jpg', 'geo.jpg', '.jpg', 100, 'image/jpeg', 1, 1)",
            [],
        )
        .unwrap();
        let (lat, lng): (Option<f64>, Option<f64>) = c
            .query_row(
                "SELECT latitude, longitude FROM assets WHERE file_name='geo.jpg'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(lat.is_none() && lng.is_none(), "老素材无定位时应为 NULL");
        c.execute(
            "UPDATE assets SET latitude = 30.25, longitude = 120.167 WHERE file_name='geo.jpg'",
            [],
        )
        .unwrap();
        let lat: f64 = c
            .query_row(
                "SELECT latitude FROM assets WHERE file_name='geo.jpg'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lat, 30.25);
    }

    /// W0-8：V1 建库包事务 —— 首装失败可重跑，不得报 "table already exists" 永久阻断。
    /// 模拟方式：先建一个只被 SCHEMA_V1 前段创建、且会与后续语句冲突的对象不可行（batch 非事务
    /// 时中途失败依赖具体失败点），这里验证两个必要属性：
    /// ① 全新库 migrate 成功且 user_version>=1；② SCHEMA_V1 整体以事务提交——
    /// 用故意截断的 batch 在同款连接上验证「失败不落任何表」。
    #[test]
    fn fresh_install_is_transactional() {
        // ① 全新库正常建库
        let c = crate::db::init_memory().unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 1);
        let tables: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tables > 1, "全新库应有完整表结构");

        // ② 半截 SQL 在事务里回滚后不留任何残留表（对照 V1 的建库方式）
        let c2 = mem();
        let tx = c2.unchecked_transaction().unwrap();
        let broken = "CREATE TABLE w0_t1 (id INTEGER PRIMARY KEY); CREATE TABLE w0_t1 (id INTEGER PRIMARY KEY);";
        assert!(tx.execute_batch(broken).is_err(), "重复建表必须报错");
        tx.rollback().unwrap_or(());
        let leftover: i64 = c2
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'w0_t%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0, "事务失败后不得残留半截建表结果");
    }

    /// W1-1（V19）：四列 + 三索引幂等；新列有默认值不破坏旧行。
    #[test]
    fn v19_adds_columns_idempotent() {
        let c = crate::db::init_memory().unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 19);
        // 幂等：重复执行不报错、不重复建列
        migrate_v19(&c).unwrap();
        migrate_v19(&c).unwrap();
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('assets')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for col in ["favorite", "rating", "user_rotation", "phash"] {
            assert!(cols.iter().any(|c| c == col), "缺少列 {col}");
        }
        // 索引存在
        let idx: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN
                 ('idx_assets_favorite','idx_assets_rating','idx_assets_phash')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 3);
    }

    /// W1-2（V20）：分面合表 —— input_mode 回填按 enabledForAi 映射、
    /// hint 并入 description、purpose/technical 0 标签修正为 manual_only、JSON 侧清空。幂等。
    #[test]
    fn v20_facet_merge_idempotent_and_maps_correctly() {
        let c = crate::db::init_memory().unwrap();
        // 当前建库默认不再播种这两个分类；在此显式造出 V20 前的旧系统行，
        // 继续覆盖旧库迁移行为，而不依赖新库带有已移除的默认分类。
        insert_legacy_manual_system_facet(&c, "purpose", "用途", "稳定的发布或设计用途", 3, 30);
        insert_legacy_manual_system_facet(
            &c,
            "technical",
            "可用性/技术特征",
            "透明背景、可裁切等非文件格式属性",
            4,
            90,
        );
        c.execute(
            "UPDATE tag_facets SET cfg_ai_assignable=1, input_mode='ai_and_manual'
              WHERE key IN ('purpose', 'technical')",
            [],
        )
        .unwrap();
        // init_memory 已跑到最新版本（V20 已应用）。构造旧 JSON 侧再手动重跑 migrate_v20 验证幂等。
        // 注意 get_settings 会自动充实默认 ai_facet_configs（normalize_ai_facet_defaults），
        // save_settings 已 skip_serializing 该字段 → 直接写原始 JSON 才能模拟老库。
        // color 的 description 在建库链路（ensure_color_facet_config）已含默认 hint，
        // 重置为短版以验证 migrate_v20 的 hint 拼接逻辑。
        c.execute(
            "UPDATE tag_facets SET description = '主色、色调与色彩关系' WHERE key = 'color'",
            [],
        )
        .unwrap();
        let old_json = r#"{"aiFacetConfigs":[{"facetKey":"color","hint":"主色由算法呈现","enabledForAi":false}]}"#;
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('app_settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [old_json],
        )
        .unwrap();
        migrate_v20(&c).unwrap();
        migrate_v20(&c).unwrap(); // 幂等：description 不重复拼接
                                  // color → manual_only；description 含 hint
        let (mode, desc): (String, String) = c
            .query_row(
                "SELECT input_mode, description FROM tag_facets WHERE key='color'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mode, "manual_only");
        assert!(
            desc.contains("主色由算法呈现"),
            "hint 应并入 description: {desc}"
        );
        let count = desc.matches("主色由算法呈现").count();
        assert_eq!(count, 1, "重跑不得重复拼接 hint");
        // purpose/technical（系统 + 0 标签）→ manual_only；custom 保持列默认 ai_and_manual
        for key in ["purpose", "technical"] {
            let m: String = c
                .query_row(
                    "SELECT input_mode FROM tag_facets WHERE key=?1",
                    [key],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(m, "manual_only", "{key} 应为 manual_only");
        }
        let custom: String = c
            .query_row(
                "SELECT input_mode FROM tag_facets WHERE key='custom'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            custom, "ai_and_manual",
            "custom 是 AI 未知词落脚点，保持默认"
        );
        // JSON 侧已清空（直接读原始 JSON：get_settings 会自动重建默认配置，不适合断言持久化状态）
        let raw: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='app_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !raw.contains("aiFacetConfigs") || raw.contains(r#""aiFacetConfigs":[]"#),
            "aiFacetConfigs 应回填后清空，实际 JSON: {raw}"
        );
    }

    // ═══════════════ F1：V22a 无条件迁移段 ═══════════════

    /// V22a 跑两遍结果一致（幂等可重入）。
    #[test]
    fn v22a_adds_columns_idempotent() {
        let c = crate::db::init_memory().unwrap();
        // 首次已由 migrate() 完成；再跑一次迁移（幂等：列已存在、触发器 IF NOT EXISTS）
        migrate_v22a(&c).unwrap();
        // 五列齐备
        for col in [
            "cfg_visible_in_navigation",
            "cfg_manual_assignable",
            "cfg_ai_assignable",
            "cfg_searchable",
            "facet_kind",
        ] {
            assert!(has_column(&c, "tag_facets", col).unwrap(), "{col} 应存在");
        }
        // asset_tags.review_state + ai_batches/ai_suggestions 溯源列
        assert!(has_column(&c, "asset_tags", "review_state").unwrap());
        assert!(has_column(&c, "ai_batches", "model_id").unwrap());
        assert!(has_column(&c, "ai_suggestions", "raw_response").unwrap());
        // 触发器存在（环/深度）
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger'
                  AND name IN ('trg_tags_no_cycle','trg_tags_max_depth_au','trg_tags_max_depth_ai')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3, "三个约束触发器应存在");
        // schema_features 登记
        let guard: i64 = c
            .query_row(
                "SELECT enabled FROM schema_features WHERE feature='tag_cycle_guard'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(guard, 1, "tag_cycle_guard 应登记为启用");
    }

    /// V22a 回填：input_mode='manual_only' → cfg_ai_assignable=0（active/inactive 都算）。
    #[test]
    fn v22a_backfills_cfg_from_input_mode() {
        let c = crate::db::init_memory().unwrap();
        // 实测库语义：color 是 inactive + manual_only → cfg_ai_assignable=0
        let (mode, status, ai): (String, String, i64) = c
            .query_row(
                "SELECT input_mode, status, cfg_ai_assignable FROM tag_facets WHERE key='color'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(mode, "manual_only");
        assert_eq!(status, "inactive");
        assert_eq!(ai, 0, "manual_only 分面 cfg_ai_assignable 应为 0");
        // active + ai_and_manual 保持 1
        let custom_ai: i64 = c
            .query_row(
                "SELECT cfg_ai_assignable FROM tag_facets WHERE key='custom'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(custom_ai, 1);
    }

    /// F1-c：环创建被拒绝（UPDATE parent_id 到自己的后代）。
    #[test]
    fn cycle_creation_rejected() {
        let c = crate::db::init_memory().unwrap();
        let id_a: i64 = c
            .query_row(
                "INSERT INTO tags (name, facet_key) VALUES ('A','custom') RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let id_b: i64 = c
            .query_row("INSERT INTO tags (name, parent_id, facet_key) VALUES ('B',?1,'custom') RETURNING id", [id_a], |r| r.get(0))
            .unwrap();
        let id_c: i64 = c
            .query_row("INSERT INTO tags (name, parent_id, facet_key) VALUES ('C',?1,'custom') RETURNING id", [id_b], |r| r.get(0))
            .unwrap();
        // 把 A 挂到 C 下 → 环
        let err = c
            .execute(
                "UPDATE tags SET parent_id=?1 WHERE id=?2",
                rusqlite::params![id_c, id_a],
            )
            .unwrap_err();
        assert!(err.to_string().contains("循环"), "应报环错误：{err}");
    }

    /// F1-c：新父深度 6 + 被移动子树高度 4 = 10 层 → 拒绝（不能只查新父深度）。
    #[test]
    fn move_subtree_respects_max_depth() {
        let c = crate::db::init_memory().unwrap();
        // 深链 6 层（0..=5，anchor0 是根，anchor5 深度 5）作新父的锚
        let mut anchor: i64 = 0;
        for i in 0..6 {
            anchor = c
                .query_row(
                    "INSERT INTO tags (name, parent_id, facet_key) VALUES (?1, ?2, 'custom') RETURNING id",
                    rusqlite::params![format!("anchor{i}"), if i == 0 { Option::<i64>::None } else { Some(anchor) }],
                    |r| r.get(0),
                )
                .unwrap();
        }
        // 另起 4 层子树：sub_root（顶部）→ sub1 → sub2 → sub3
        let sub_root: i64 = c
            .query_row(
                "INSERT INTO tags (name, facet_key) VALUES ('sub_root','custom') RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut child = sub_root;
        for i in 1..4 {
            child = c
                .query_row(
                    "INSERT INTO tags (name, parent_id, facet_key) VALUES (?1, ?2, 'custom') RETURNING id",
                    rusqlite::params![format!("sub{i}"), child],
                    |r| r.get(0),
                )
                .unwrap();
        }
        // 把子树**顶部**（sub_root，高 4 含自身）挂到 anchor5（深 6）下
        // → anc(新父)=6 + des(sub_root 子树)=3 → 6+3=9 > 8 → 拒绝
        let err = c
            .execute(
                "UPDATE tags SET parent_id=?1 WHERE id=?2",
                rusqlite::params![anchor, sub_root],
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("层级") || err.to_string().contains("8 层"),
            "应报深度错误：{err}"
        );
    }

    /// F1-c：新节点插到深 8 的父下 → 拒绝。
    #[test]
    fn insert_at_max_depth_rejected() {
        let c = crate::db::init_memory().unwrap();
        let mut parent: i64 = 0;
        for i in 0..8 {
            parent = c
                .query_row(
                    "INSERT INTO tags (name, parent_id, facet_key) VALUES (?1, ?2, 'custom') RETURNING id",
                    rusqlite::params![format!("n{i}"), if i == 0 { Option::<i64>::None } else { Some(parent) }],
                    |r| r.get(0),
                )
                .unwrap();
        }
        // parent 深 8（0..=7 = 8 层，最深的那个 depth=7），子节点 depth=8 ≥ 8 → 拒绝
        let err = c
            .execute(
                "INSERT INTO tags (name, parent_id, facet_key) VALUES ('too_deep', ?1, 'custom')",
                [parent],
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("8 层") || err.to_string().contains("层级"),
            "应报深度错误：{err}"
        );
    }

    /// F1-c：跨分面挂父被拒绝（INSERT）。
    #[test]
    fn parent_facet_mismatch_rejected_on_insert() {
        let c = crate::db::init_memory().unwrap();
        let scene_id: i64 = c
            .query_row("SELECT id FROM tags WHERE facet_key='scene' AND name='scene' OR facet_key='scene' LIMIT 1", [], |r| r.get(0))
            .ok()
            .unwrap_or_else(|| {
                c.query_row(
                    "INSERT INTO tags (name, facet_key) VALUES ('场景根','scene') RETURNING id",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
            });
        // 用 people 分面的标签挂到 scene 下
        let err = c
            .execute(
                "INSERT INTO tags (name, parent_id, facet_key) VALUES ('错面', ?1, 'people')",
                [scene_id],
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("同一分面"),
            "应报同分面错误：{err}"
        );
    }

    /// F1-d 兜底：手工造环（绕过触发器）后 list_tree / total_count 不挂死。
    #[test]
    fn recursive_cte_terminates_on_existing_cycle() {
        let c = crate::db::init_memory().unwrap();
        let id_a: i64 = c
            .query_row(
                "INSERT INTO tags (name, facet_key) VALUES ('CA','custom') RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let id_b: i64 = c
            .query_row("INSERT INTO tags (name, parent_id, facet_key) VALUES ('CB',?1,'custom') RETURNING id", [id_a], |r| r.get(0))
            .unwrap();
        // 直接改 SQL 造环：B 的父已是 A，再把 A 的父改成 B（绕过触发器：先删触发器）
        c.execute_batch("DROP TRIGGER trg_tags_no_cycle; DROP TRIGGER trg_tags_max_depth_au;")
            .unwrap();
        c.execute(
            "UPDATE tags SET parent_id=?1 WHERE id=?2",
            rusqlite::params![id_b, id_a],
        )
        .unwrap();
        // 另加一个正常根，保证 list_tree 顶层有内容（环节点成对互为父子，无根）
        c.execute(
            "INSERT INTO tags (name, facet_key) VALUES ('正常根','custom')",
            [],
        )
        .unwrap();
        // list_tree 不应挂死（深度上限让递归终止）
        let tree = super::super::tags::list_tree(&c).unwrap();
        assert!(!tree.is_empty(), "至少应有正常根节点");
        // total_count 也不挂死
        let _ = super::super::tags::total_count(&c, id_a).unwrap();
    }

    /// V22a 回填守护：ai_suggestion_items decision in (accepted,modified) 的关联
    /// → review_state='ai_reviewed'（ReplaceAiOnly 不误删）。
    #[test]
    fn v22_backfill_marks_existing_confirmed_as_reviewed() {
        let c = crate::db::init_memory().unwrap();
        // 素材 + 标签
        let asset_id: i64 = c
            .query_row(
                "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at) VALUES ('/x/1.jpg','1.jpg','jpg',1,'image/jpeg',1,1) RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let tag_id: i64 = c
            .query_row("INSERT INTO tags (name, normalized_name, facet_key) VALUES ('海边','海边','scene') RETURNING id", [], |r| r.get(0))
            .unwrap();
        // 构造「已确认」的 AI 链路：batch + suggestion + item(accepted)
        let batch: i64 = c
            .query_row(
                "INSERT INTO ai_batches (status, mode, total, processed, confirmed, created_at) VALUES ('done','cloud',1,1,1,1) RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let sugg: i64 = c
            .query_row(
                "INSERT INTO ai_suggestions (batch_id, asset_id, suggested_tags, status, created_at) VALUES (?1,?2,'[]','confirmed',1) RETURNING id",
                rusqlite::params![batch, asset_id],
                |r| r.get(0),
            )
            .unwrap();
        c.execute(
            "INSERT INTO ai_suggestion_items (suggestion_id, facet_key, raw_name, normalized_name, tag_id, decision, created_at) VALUES (?1,'scene','海边','海边',?2,'accepted',1)",
            rusqlite::params![sugg, tag_id],
        )
        .unwrap();
        // AI 来源关联（review_state 默认 ai_unreviewed）
        c.execute(
            "INSERT INTO asset_tags (asset_id, tag_id, source, created_at, source_batch_id) VALUES (?1,?2,'ai_cloud',1,?3)",
            rusqlite::params![asset_id, tag_id, batch],
        )
        .unwrap();
        // 手工来源另一条 → manual
        let tag_manual: i64 = c
            .query_row("INSERT INTO tags (name, normalized_name, facet_key) VALUES ('人像','人像','subject') RETURNING id", [], |r| r.get(0))
            .unwrap();
        c.execute(
            "INSERT INTO asset_tags (asset_id, tag_id, source, created_at) VALUES (?1,?2,'manual',1)",
            rusqlite::params![asset_id, tag_manual],
        )
        .unwrap();
        // 再跑一次 V22a（幂等）触发回填 UPDATE
        migrate_v22a(&c).unwrap();
        let state: String = c
            .query_row(
                "SELECT review_state FROM asset_tags WHERE tag_id=?1",
                [tag_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "ai_reviewed", "已确认的 AI 标签应回填为 ai_reviewed");
        let manual_state: String = c
            .query_row(
                "SELECT review_state FROM asset_tags WHERE tag_id=?1",
                [tag_manual],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(manual_state, "manual");
    }

    #[test]
    fn legacy_manual_only_migration_remains_one_time_for_existing_rows() {
        let c = crate::db::init_memory().unwrap();
        for (key, name, description, max_items, sort_order) in [
            ("purpose", "用途", "稳定的发布或设计用途", 3, 30),
            (
                "technical",
                "可用性/技术特征",
                "透明背景、可裁切等非文件格式属性",
                4,
                90,
            ),
        ] {
            c.execute(
                "INSERT INTO tag_facets
                   (key, display_name, description, selection_mode, max_items, sort_order,
                    is_system, status, applies_to, created_at, updated_at, cfg_ai_assignable, input_mode)
                 VALUES (?1, ?2, ?3, 'multi', ?4, ?5, 1, 'active', 'all', 1, 1, 1, 'ai_and_manual')",
                rusqlite::params![key, name, description, max_items, sort_order],
            )
            .unwrap();
        }
        c.execute(
            "DELETE FROM settings WHERE key='manual_only_system_facets_v1'",
            [],
        )
        .unwrap();
        for key in ["purpose", "technical"] {
            assert_eq!(
                crate::db::tag_facets::get(&c, key).unwrap().input_mode,
                "ai_and_manual"
            );
        }
        migrate_manual_only_system_facets(&c).unwrap();
        for key in ["purpose", "technical"] {
            let ai: i64 = c
                .query_row(
                    "SELECT cfg_ai_assignable FROM tag_facets WHERE key=?1",
                    [key],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(ai, 0, "{key} 首次迁移后应为人工填写");
        }

        // 用户后来主动移回 AI 组；启动时重跑无条件迁移段不得覆盖用户选择。
        c.execute(
            "UPDATE tag_facets SET cfg_ai_assignable=1, input_mode='ai_and_manual' WHERE key='purpose'",
            [],
        )
        .unwrap();
        migrate_manual_only_system_facets(&c).unwrap();
        let ai: i64 = c
            .query_row(
                "SELECT cfg_ai_assignable FROM tag_facets WHERE key='purpose'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ai, 1, "一次性迁移不得在后续启动覆盖用户设置");
    }

    fn insert_legacy_manual_system_facet(
        conn: &Connection,
        key: &str,
        display_name: &str,
        description: &str,
        max_items: i64,
        sort_order: i64,
    ) {
        conn.execute(
            "INSERT INTO tag_facets
               (key, display_name, description, selection_mode, max_items, sort_order,
                is_system, status, applies_to, created_at, updated_at,
                cfg_visible_in_navigation, cfg_manual_assignable, cfg_ai_assignable,
                cfg_searchable, facet_kind, input_mode)
             VALUES (?1, ?2, ?3, 'multi', ?4, ?5, 1, 'active', 'all', 1, 1, 1, 1, 0, 1, 'tag', 'manual_only')",
            rusqlite::params![key, display_name, description, max_items, sort_order],
        )
        .unwrap();
    }

    #[test]
    fn legacy_manual_system_facets_are_removed_or_preserved_as_user_facets_idempotently() {
        let c = crate::db::init_memory().unwrap();
        insert_legacy_manual_system_facet(&c, "purpose", "用途", "稳定的发布或设计用途", 3, 30);
        insert_legacy_manual_system_facet(
            &c,
            "technical",
            "技术特征（已自定义）",
            "用户修改过的说明",
            4,
            90,
        );
        c.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('透明背景', '透明背景', '透明背景', 'technical', 0, 'active', 0)",
            [],
        )
        .unwrap();

        migrate_remove_legacy_manual_system_facets(&c).unwrap();
        assert!(crate::db::tag_facets::get(&c, "purpose").is_err());
        let technical = crate::db::tag_facets::get(&c, "technical").unwrap();
        assert!(!technical.is_system);
        assert_eq!(technical.display_name, "技术特征（已自定义）");
        assert_eq!(technical.description, "用户修改过的说明");
        let tag_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE facet_key='technical'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag_count, 1, "迁移不得丢失用户已有标签");

        // 相同 key 后续由用户新建后，重复启动迁移不能再影响它。
        let purpose = crate::db::tag_facets::create(
            &c,
            "purpose",
            "我的用途",
            "自建说明",
            "multi",
            None,
            "all",
        )
        .unwrap();
        migrate_remove_legacy_manual_system_facets(&c).unwrap();
        assert!(!crate::db::tag_facets::get(&c, "purpose").unwrap().is_system);
        assert!(
            !crate::db::tag_facets::get(&c, "technical")
                .unwrap()
                .is_system
        );
        assert_eq!(purpose.display_name, "我的用途");
    }

    #[test]
    fn style_system_facet_removed_once_but_user_facet_is_preserved() {
        let c = crate::db::init_memory().unwrap();
        c.execute(
            "INSERT INTO tag_facets
               (key, display_name, description, selection_mode, sort_order, is_system, status, created_at, updated_at)
             VALUES ('style', '风格/氛围', '视觉风格与整体情绪', 'multi', 40, 1, 'active', 1, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tags (name, normalized_name, canonical_name, facet_key, is_system, status, sort_order)
             VALUES ('未知', '未知', '未知', 'style', 0, 'active', 0)",
            [],
        )
        .unwrap();
        let asset_id: i64 = c
            .query_row(
                "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
                 VALUES ('/style.jpg', 'style.jpg', 'jpg', 1, 'image/jpeg', 1, 1) RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let batch_id: i64 = c
            .query_row(
                "INSERT INTO ai_batches (status, mode, total, created_at)
                 VALUES ('done', 'cloud', 1, 1) RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let suggestion_id: i64 = c
            .query_row(
                "INSERT INTO ai_suggestions
                   (batch_id, asset_id, suggested_tags, confirmed_tags, status, created_at)
                 VALUES (?1, ?2, '{\"style\":[\"未知\"],\"scene\":[\"海边\"]}',
                                      '{\"style\":[\"未知\"],\"scene\":[\"海边\"]}', 'confirmed', 1)
                 RETURNING id",
                rusqlite::params![batch_id, asset_id],
                |r| r.get(0),
            )
            .unwrap();
        // init_memory 已跑过全新库迁移；移除标记以模拟尚未执行本次清理的存量库。
        c.execute(
            "DELETE FROM settings WHERE key='style_facet_removed_v1'",
            [],
        )
        .unwrap();

        migrate_remove_style_facet(&c).unwrap();
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tag_facets WHERE key='style'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "历史系统 style 分面应被删除");
        let tag_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE facet_key='style'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tag_count, 0, "style 标签应随分面级联删除");
        let remaining: (Option<String>, Option<String>) = c
            .query_row(
                "SELECT json_extract(suggested_tags, '$.style'),
                        json_extract(confirmed_tags, '$.style')
                   FROM ai_suggestions WHERE id=?1",
                [suggestion_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(remaining, (None, None), "旧建议 JSON 中的 style 必须清理");
        let scene: String = c
            .query_row(
                "SELECT json_extract(suggested_tags, '$.scene[0]') FROM ai_suggestions WHERE id=?1",
                [suggestion_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scene, "海边", "清理 style 不得影响其他建议标签");

        // 迁移标记写入后，用户主动创建的同名自定义分面不得被后续启动再次删除。
        crate::db::tag_facets::create(&c, "style", "我的风格", "", "multi", None, "all").unwrap();
        migrate_remove_style_facet(&c).unwrap();
        let is_system: i64 = c
            .query_row(
                "SELECT is_system FROM tag_facets WHERE key='style'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(is_system, 0, "用户自定义 style 分面应保留");
    }
}
