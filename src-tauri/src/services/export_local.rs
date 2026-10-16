//! 本地导出：复制/移动 + 进度回调 + 取消 + export_tasks 持久化（架构 §4.4）

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusqlite::Connection;
use serde::Serialize;

use crate::db::{assets, export};
use crate::error::{AppError, AppResult};
use crate::state::Database;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    pub task_id: i64,
    pub done: i64,
    pub total: i64,
}

/// R-26 子目录组织：flat（默认平铺）| by_tag（首个标签名，无标签入「未分组」）| by_date（YYYY-MM，taken_at 优先）
fn subdir_for(asset: &assets::Asset, layout: &str) -> String {
    match layout {
        "by_tag" => asset
            .tags
            .first()
            .map(|t| sanitize_dirname(&t.name))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "未分组".to_string()),
        "by_date" => {
            let ts = asset.taken_at.unwrap_or(asset.created_at);
            chrono::DateTime::from_timestamp_millis(ts)
                .map(|dt| dt.format("%Y-%m").to_string())
                .unwrap_or_else(|| "未知日期".to_string())
        }
        _ => String::new(),
    }
}

/// Windows 非法字符清洗（目录名）；全非法时回退「未分组」
fn sanitize_dirname(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        "未分组".to_string()
    } else {
        s
    }
}

/// 逐文件「短锁读库 → 锁外复制/移动 → 短锁写进度」，文件 IO 期间不持 DB 锁，
/// 避免大文件导出长时间阻塞全应用 DB 读写
/// R-26：layout 控制子目录组织（flat|by_tag|by_date）
/// too_many_arguments：8 参数为导出管线链路参数的稳定集合（db/任务/文件/模式/取消/进度），
/// 收进结构体会波及命令层与既有调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn export_local<F: Fn(ExportProgress)>(
    db: &Arc<Database>,
    task_id: i64,
    asset_ids: &[i64],
    dest_dir: &str,
    mode: &str,   // copy|move
    layout: &str, // flat|by_tag|by_date
    cancel: &Arc<AtomicBool>,
    progress: F,
) -> AppResult<()> {
    let lock = || db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
    let dest = PathBuf::from(dest_dir);
    fs::create_dir_all(&dest)?;
    let started = Instant::now();
    let total = asset_ids.len() as i64;
    tracing::info!(
        operation = "export_local",
        task_id,
        stage = "start",
        mode = %mode,
        layout = %layout,
        total,
        "开始本地导出"
    );
    {
        let conn = lock()?;
        export::update_progress(&conn, task_id, 0, "running")?;
    }

    let mut done = 0i64;
    // P1-04：跨盘降级 copy 后源文件删除失败的计数（B04 有意保留副本，但用户应知情）
    let mut stale_sources: i64 = 0;
    for &id in asset_ids {
        if cancel.load(Ordering::Relaxed) {
            let conn = lock()?;
            export::finish_task(&conn, task_id, "cancelled", None, None)?;
            tracing::warn!(
                operation = "export_local",
                task_id,
                stage = "cancelled",
                done,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出已取消"
            );
            return Ok(());
        }
        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id)?
        };
        let src = PathBuf::from(&asset.file_path);
        // R-26：按 layout 解析目标子目录（flat = 目标根目录）
        let sub = subdir_for(&asset, layout);
        let target_dir = if sub.is_empty() {
            dest.clone()
        } else {
            let d = dest.join(&sub);
            fs::create_dir_all(&d)?;
            d
        };
        // BUG-QA-1：unique_dest 失败不走 ?（会跳过 finish_task 导致任务卡 running），
        // 纳入 r 让下方 if let Err(e) = r 捕获后正常 finish_task("failed")
        let r = match unique_dest(&target_dir, &asset.file_name) {
            Ok((dst, norm)) if mode == "move" => {
                // 文件移动（可能耗时）：不持 DB 锁
                match move_file(&src, &dst) {
                    Ok(cleaned) => {
                        if !cleaned {
                            stale_sources += 1;
                            tracing::warn!(
                                operation = "export_local",
                                task_id,
                                asset_id = id,
                                stage = "stale_source",
                                "文件已移动但源文件未能删除"
                            );
                        }
                        // B04：move 成功后更新库记录指向新路径 + 新文件名
                        // （unique_dest 可能加了 (1) 后缀，file_name 需同步）
                        let new_name = dst
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&asset.file_name)
                            .to_string();
                        // UPDATE 失败不用 ?（会跳过下方 finish_task），而是作为 r=Err 让 finish_task 正常执行
                        match lock().and_then(|conn| {
                            assets::update_file_path_and_name(&conn, id, &norm, &new_name)
                        }) {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                tracing::error!(
                                    "move 后更新库记录失败（文件已移动到 {}）: {e}",
                                    dst.display()
                                );
                                Err(AppError::msg(format!(
                                    "文件已移动但库记录未更新，请重新入库: {e}"
                                )))
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Ok((dst, _)) => {
                // 文件复制（可能耗时）：不持 DB 锁
                fs::copy(&src, &dst).map(|_| ()).map_err(AppError::from)
            }
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            tracing::error!(
                operation = "export_local",
                task_id,
                asset_id = id,
                file_name = %asset.file_name,
                stage = "file_failed",
                error_code = e.code(),
                error = %e,
                done,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "导出单个素材失败"
            );
            let conn = lock()?;
            export::finish_task(
                &conn,
                task_id,
                "failed",
                None,
                Some(&format!("{}: {e}", asset.file_name)),
            )?;
            return Err(e);
        }
        done += 1;
        {
            let conn = lock()?;
            export::update_progress(&conn, task_id, done, "running")?;
        }
        progress(ExportProgress {
            task_id,
            done,
            total,
        });
    }
    {
        let conn = lock()?;
        if stale_sources > 0 {
            // P1-04：任务仍算成功（文件已在新位置、库已指向新路径），但附软提示：
            // 源位置残留副本，用户可手动清理——不再是「虚假报告成功」
            export::finish_task_with_warning(
                &conn,
                task_id,
                &format!(
                    "{stale_sources} 个源文件未能清理，原位置仍有副本（文件已完整移动，可手动删除）"
                ),
            )?;
            tracing::warn!(
                operation = "export_local",
                task_id,
                stage = "done_with_warning",
                mode = %mode,
                layout = %layout,
                done,
                total,
                stale_sources,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出完成但存在源文件残留"
            );
        } else {
            export::finish_task(&conn, task_id, "done", None, None)?;
            tracing::info!(
                operation = "export_local",
                task_id,
                stage = "done",
                mode = %mode,
                layout = %layout,
                done,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出完成"
            );
        }
    }
    Ok(())
}

/// 同名冲突自动加 (1)(2) 后缀；B06b：冲突超限(999)时报错而非回退覆盖
fn unique_dest(dir: &Path, name: &str) -> AppResult<(PathBuf, String)> {
    let candidate = dir.join(name);
    if !candidate.exists() {
        let path = crate::utils::path::encode_native_path(&candidate)?;
        let norm = crate::utils::path::normalize_path(&path);
        return Ok((candidate, norm));
    }
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let c = dir.join(format!("{stem}({i}){ext}"));
        if !c.exists() {
            let path = crate::utils::path::encode_native_path(&c)?;
            let norm = crate::utils::path::normalize_path(&path);
            return Ok((c, norm));
        }
    }
    Err(AppError::msg(format!("目标目录同名文件过多: {name}")))
}

/// 跨盘 rename 失败时降级 copy+remove；B04：remove 失败不阻塞（文件已在新位置，源残留记录日志）
/// P1-04：返回 bool 表示源文件是否已彻底清理（true=rename 或 copy+remove 完成；false=源残留）
fn move_file(src: &Path, dst: &Path) -> AppResult<bool> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(_) => {
            fs::copy(src, dst)?;
            // B04：remove 失败不阻塞——文件已在新位置，源残留记录日志
            match fs::remove_file(src) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        }
    }
}

/// R-26 CSV 清单导出：文件名/路径/标签/EXIF 摘要，UTF-8 BOM 保 Excel 兼容；
/// 返回清单文件路径。不存在的 id 跳过（部分成功，同 get_asset_urls 语义）
/// P1-03：与素材导出同一 unique_dest 策略（同名自动加 (1)(2)…），不再静默覆盖已有清单；
/// 先写临时文件再原子重命名，崩溃时序不留半截文件
pub fn write_csv_manifest(
    conn: &Connection,
    asset_ids: &[i64],
    dest_dir: &str,
) -> AppResult<PathBuf> {
    let dest = PathBuf::from(dest_dir);
    fs::create_dir_all(&dest)?;
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"\xef\xbb\xbf"); // UTF-8 BOM
    buf.extend_from_slice(
        "文件名,文件路径,标签,宽,高,大小字节,拍摄时间,相机,镜头,ISO,光圈,快门,焦距\r\n".as_bytes(),
    );
    for &id in asset_ids {
        let Ok(a) = assets::get_managed(conn, id) else {
            continue;
        };
        let tags = a
            .tags
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
            .join("; ");
        let taken = a
            .taken_at
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let row = [
            &a.file_name,
            &a.file_path,
            &tags,
            &a.width.map(|v| v.to_string()).unwrap_or_default(),
            &a.height.map(|v| v.to_string()).unwrap_or_default(),
            &a.file_size.to_string(),
            &taken,
            a.camera.as_deref().unwrap_or_default(),
            a.lens.as_deref().unwrap_or_default(),
            &a.iso.map(|v| v.to_string()).unwrap_or_default(),
            &a.aperture.map(|v| v.to_string()).unwrap_or_default(),
            a.shutter.as_deref().unwrap_or_default(),
            &a.focal.map(|v| v.to_string()).unwrap_or_default(),
        ]
        .iter()
        .map(|f| csv_escape(f))
        .collect::<Vec<_>>()
        .join(",");
        buf.extend_from_slice(row.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    // P1-03：不走固定文件名——已存在同名清单时自动加序号，绝不覆盖用户旧文件
    let (out, _) = unique_dest(&dest, "导出清单.csv")?;
    // 原子写：临时文件写完后 rename（同目录 rename 原子；Windows 下目标不存在即成功）
    let tmp = out.with_extension("csv.tmp");
    fs::write(&tmp, &buf)?;
    if let Err(e) = fs::rename(&tmp, &out) {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::from(e));
    }
    Ok(out)
}

/// CSV 字段转义：含逗号/引号/换行时加双引号包裹；
/// P2-09：以 = + - @ 开头的值（恶意文件名 / AI 建议标签 = 不可信输入）前置单引号，
/// 防 Excel 打开时被当作公式执行
fn csv_escape(s: &str) -> String {
    let formula_prefix = matches!(s.chars().next(), Some('=' | '+' | '-' | '@'));
    let s = if formula_prefix {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_formula_prefix_neutralized() {
        // P2-09：= + - @ 开头一律前置单引号，防 Excel 公式执行
        assert_eq!(csv_escape("=SUM(A1:A9)"), "'=SUM(A1:A9)");
        assert_eq!(csv_escape("+cmd|' /C calc"), "'+cmd|' /C calc");
        assert_eq!(csv_escape("-1"), "'-1");
        assert_eq!(csv_escape("@SUM"), "'@SUM");
        // 正常值不受影响
        assert_eq!(csv_escape("公园"), "公园");
        assert_eq!(csv_escape("1/250"), "1/250");
        // 前缀转义与引号包裹/逗号转义共存
        assert_eq!(csv_escape("=x,y"), "\"'=x,y\"");
        assert_eq!(csv_escape("正常,标签"), "\"正常,标签\"");
        assert_eq!(csv_escape("说\"好"), "\"说\"\"好\"");
        assert_eq!(csv_escape(""), "");
    }

    #[test]
    fn csv_manifest_never_overwrites_and_unique_names() {
        let conn = crate::db::init_memory().expect("内存库初始化失败");
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let dest = dir.path().join("out");
        let name = |p: &std::path::Path| p.file_name().unwrap().to_string_lossy().into_owned();

        // P1-03：首次固定名，二次自动加 (1)，绝不覆盖已有清单
        let p1 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("首次清单失败");
        assert_eq!(name(&p1), "导出清单.csv");
        let p2 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("二次清单失败");
        assert_eq!(name(&p2), "导出清单(1).csv");
        let p3 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("三次清单失败");
        assert_eq!(name(&p3), "导出清单(2).csv");

        // 第一份内容未被覆盖（三份逐字节一致：空资产 → 仅表头）
        let c1 = std::fs::read(&p1).unwrap();
        let c2 = std::fs::read(&p2).unwrap();
        assert_eq!(c1, c2);
        // 原子写不留 .tmp 残留
        assert!(!dir.path().join("out").join("导出清单.csv.tmp").exists());
    }
}
