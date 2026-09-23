//! 视频兼容代理缓存（指导书 §8.3）：按 素材 + 变体 记录生成/查询状态，不替换原文件。
//! 状态机：queued | running | ready | failed | canceled。代理失败原因可展示；清理缓存不影响原文件。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProxy {
    pub asset_id: i64,
    pub variant: String,
    pub status: String,
    pub path: Option<String>,
    pub error: Option<String>,
    pub updated_at: i64,
    // P1-05：缓存有效性指纹（V25 迁移列）。ready 命中必须三者全一致才复用。
    /// 源素材内容指纹（内容 hash 优先，回退 size+mtime）。
    pub source_fingerprint: Option<String>,
    /// 编码策略版本常量（codec/码率/像素格式/容器/兼容参数变化时递增）。
    pub encoder_version: Option<i64>,
    /// 已验证 ffmpeg 版本 + 二进制指纹。
    pub tool_fingerprint: Option<String>,
}

/// 读取某个素材 + 变体的代理记录；不存在返回 None。
pub fn get(conn: &Connection, asset_id: i64, variant: &str) -> AppResult<Option<VideoProxy>> {
    Ok(conn
        .query_row(
            "SELECT asset_id, variant, status, path, error, updated_at,
                    source_fingerprint, encoder_version, tool_fingerprint
               FROM video_proxies WHERE asset_id=?1 AND variant=?2",
            params![asset_id, variant],
            |r| {
                Ok(VideoProxy {
                    asset_id: r.get(0)?,
                    variant: r.get(1)?,
                    status: r.get(2)?,
                    path: r.get(3)?,
                    error: r.get(4)?,
                    updated_at: r.get(5)?,
                    source_fingerprint: r.get(6)?,
                    encoder_version: r.get(7)?,
                    tool_fingerprint: r.get(8)?,
                })
            },
        )
        .optional()?)
}

/// 更新某个素材 + 变体的代理状态（insert 或 replace）。只短锁调用。
/// running/failed/canceled 等非 ready 状态用此函数；**不触碰指纹列**（ON CONFLICT 未列出，
/// 保留既有值），避免中间状态清空 ready 时写入的指纹。
pub fn upsert(
    conn: &Connection,
    asset_id: i64,
    variant: &str,
    status: &str,
    path: Option<&str>,
    error: Option<&str>,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO video_proxies (asset_id, variant, status, path, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(asset_id, variant) DO UPDATE SET
           status=excluded.status, path=excluded.path, error=excluded.error, updated_at=excluded.updated_at",
        params![asset_id, variant, status, path, error, now],
    )?;
    Ok(())
}

/// P1-05：写入 ready 状态并**同时落三项指纹**（源内容 / 编码策略 / 工具版本）。
/// 只在代理成功生成或磁盘复用确认新鲜时调用；复用判定据此三列。
pub fn upsert_ready(
    conn: &Connection,
    asset_id: i64,
    variant: &str,
    path: &str,
    source_fingerprint: &str,
    encoder_version: i64,
    tool_fingerprint: &str,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO video_proxies
           (asset_id, variant, status, path, error, created_at, updated_at,
            source_fingerprint, encoder_version, tool_fingerprint)
         VALUES (?1, ?2, 'ready', ?3, NULL, ?4, ?4, ?5, ?6, ?7)
         ON CONFLICT(asset_id, variant) DO UPDATE SET
           status='ready', path=excluded.path, error=NULL, updated_at=excluded.updated_at,
           source_fingerprint=excluded.source_fingerprint,
           encoder_version=excluded.encoder_version,
           tool_fingerprint=excluded.tool_fingerprint",
        params![
            asset_id,
            variant,
            path,
            now,
            source_fingerprint,
            encoder_version,
            tool_fingerprint
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn upsert_and_get_roundtrip() {
        let c = init_memory().unwrap();
        // 需要一条 asset 记录（FK）
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES ('/v.mp4', 'v.mp4', 'mp4', 1, 'video/mp4', 1, 1)",
            [],
        )
        .unwrap();
        let asset_id = c
            .query_row("SELECT id FROM assets", [], |r| r.get(0))
            .unwrap();

        upsert(&c, asset_id, "h264_mp4", "running", None, None).unwrap();
        let p = get(&c, asset_id, "h264_mp4").unwrap().unwrap();
        assert_eq!(p.status, "running");
        assert_eq!(p.variant, "h264_mp4");
        assert_eq!(p.source_fingerprint, None);

        upsert(
            &c,
            asset_id,
            "h264_mp4",
            "ready",
            Some("/proxy/v.mp4"),
            None,
        )
        .unwrap();
        let p = get(&c, asset_id, "h264_mp4").unwrap().unwrap();
        assert_eq!(p.status, "ready");
        assert_eq!(p.path.as_deref(), Some("/proxy/v.mp4"));
    }

    /// P1-05：upsert_ready 落三项指纹；后续非 ready 的 upsert 不清空指纹（保留供诊断）。
    #[test]
    fn upsert_ready_persists_fingerprints() {
        let c = init_memory().unwrap();
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES ('/v.mp4', 'v.mp4', 'mp4', 1, 'video/mp4', 1, 1)",
            [],
        )
        .unwrap();
        let id: i64 = c
            .query_row("SELECT id FROM assets", [], |r| r.get(0))
            .unwrap();

        upsert_ready(&c, id, "h264_mp4", "/proxy/v.mp4", "srcfp-1", 3, "toolfp-1").unwrap();
        let p = get(&c, id, "h264_mp4").unwrap().unwrap();
        assert_eq!(p.status, "ready");
        assert_eq!(p.source_fingerprint.as_deref(), Some("srcfp-1"));
        assert_eq!(p.encoder_version, Some(3));
        assert_eq!(p.tool_fingerprint.as_deref(), Some("toolfp-1"));

        // 转 running：指纹不被清空（ON CONFLICT 不列出指纹列）。
        upsert(&c, id, "h264_mp4", "running", None, None).unwrap();
        let p2 = get(&c, id, "h264_mp4").unwrap().unwrap();
        assert_eq!(p2.status, "running");
        assert_eq!(p2.source_fingerprint.as_deref(), Some("srcfp-1"));
        assert_eq!(p2.encoder_version, Some(3));
    }
}
