//! 库内分库目标的枚举（A4-02 的「目标分库」列表）。
//!
//! 分库不是独立实体，它只是总仓内的一个真实相对目录；引用由总仓代次派生
//! （`db::collections::collection_ref_for`）。所以「有哪些可选目标」这件事必须
//! 由磁盘现状决定，而不是由数据库里历史上出现过什么决定。
//!
//! 这段逻辑曾经写在命令层（`commands/move_cmd.rs`）。目录遍历、保留目录过滤、
//! 深度与数量上限、引用登记都是业务规则，留在命令层会让「命令只做参数校验与
//! 状态编排」这条约定失效，也让第二个需要同一份列表的入口只能复制一遍。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::db::collections;
use crate::db::libraries::LibrarySnapshot;
use crate::error::AppResult;
use crate::services::managed_library;
use crate::state::Database;

/// 目录枚举上限：分库是给人选的，不是文件浏览器。
const TARGET_LIMIT: usize = 200;
const TARGET_MAX_DEPTH: usize = 4;

/// 一个可选的分库目标。分库只是总仓内的相对目录，引用由总仓代次派生。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionTarget {
    pub collection_ref: String,
    pub relative_path: String,
    pub label: String,
}

/// 列出当前总仓里可以作为移动目标的目录，并为每个目录登记分库引用。
///
/// 顺序：先按名字排序的深度优先（根目录永远第一个），再逐个 `collections::ensure`
/// —— 登记是幂等的，重复列出不会产生新分库。
pub fn list(
    db: &Arc<Database>,
    library: &LibrarySnapshot,
    now: i64,
) -> AppResult<Vec<CollectionTarget>> {
    let root = managed_library::verified_root_path(library)?;
    let mut directories = Vec::new();
    walk_directories(&root, Path::new(""), 0, &mut directories);

    let conn = db
        .lock()
        .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
    let mut targets = Vec::with_capacity(directories.len());
    for relative_path in directories {
        let stored = collections::ensure(
            &conn,
            &library.library_id,
            &library.library_epoch,
            &relative_path,
            now,
        )?;
        targets.push(CollectionTarget {
            collection_ref: stored.collection_ref,
            label: if relative_path.is_empty() {
                "总仓根目录".to_string()
            } else {
                relative_path.clone()
            },
            relative_path,
        });
    }
    Ok(targets)
}

fn walk_directories(root: &Path, relative: &Path, depth: usize, out: &mut Vec<String>) {
    if depth == 0 {
        out.push(relative_path_text(relative));
    } else if depth > TARGET_MAX_DEPTH || out.len() >= TARGET_LIMIT {
        return;
    }
    let directory = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    // 读不到就当作没有子目录：一个刚被删掉的目录不该让整列表失败。
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 链接可能指向总仓之外，而「分库」的定义是总仓内的真实目录。
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // 保留目录（缩略图、代理视频等）不是分库：它们由应用维护，用户不该把素材搬进去。
        if depth == 0 && managed_library::is_reserved_managed_path(Path::new(&name)) {
            continue;
        }
        names.push(name);
    }
    names.sort();
    for name in names {
        let child = if relative.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            relative.join(&name)
        };
        if out.len() >= TARGET_LIMIT {
            return;
        }
        out.push(relative_path_text(&child));
        walk_directories(root, &child, depth + 1, out);
    }
}

/// 相对路径统一用 `/` 分隔存储：它在数据库里是跨平台的字面量，不是本地路径。
fn relative_path_text(relative: &Path) -> String {
    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_memory, libraries};
    use std::fs;
    use tempfile::TempDir;

    fn library_and_db() -> (TempDir, Arc<Database>) {
        let directory = tempfile::tempdir().unwrap();
        let conn = init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, directory.path()).unwrap();
        let db = Arc::new(Database::new(conn));
        (directory, db)
    }

    fn current(db: &Arc<Database>) -> LibrarySnapshot {
        let conn = db.lock().unwrap();
        libraries::current(&conn).unwrap().unwrap()
    }

    #[test]
    fn targets_follow_the_real_disk_root_first_and_skip_reserved_directories() {
        let (directory, db) = library_and_db();
        fs::create_dir_all(directory.path().join("旅行/云南")).unwrap();
        fs::create_dir_all(directory.path().join("茶馆")).unwrap();
        // 保留目录不该出现在候选里：它由应用维护，不是用户的分类。
        let reserved_name = ".migration-stage";
        fs::create_dir_all(directory.path().join(reserved_name)).unwrap();

        let library = current(&db);
        let targets = list(&db, &library, 1).unwrap();
        let paths: Vec<&str> = targets.iter().map(|t| t.relative_path.as_str()).collect();

        assert_eq!(paths.first(), Some(&""), "根目录必须第一个");
        assert_eq!(targets[0].label, "总仓根目录");
        assert!(paths.contains(&"旅行"));
        assert!(paths.contains(&"旅行/云南"), "相对路径统一用 / 分隔");
        assert!(paths.contains(&"茶馆"));
        assert!(
            !paths.iter().any(|path| path.starts_with(reserved_name)),
            "保留目录被当成可选分库了：{paths:?}"
        );
        // 引用由总仓代次派生，且登记是幂等的。
        let again = list(&db, &library, 2).unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|t| t.collection_ref.clone())
                .collect::<Vec<_>>(),
            again
                .iter()
                .map(|t| t.collection_ref.clone())
                .collect::<Vec<_>>()
        );
        assert!(targets
            .iter()
            .all(|target| target.collection_ref.starts_with("collection_")));
    }

    #[test]
    fn a_missing_root_is_reported_as_a_library_problem_not_an_empty_list() {
        let (directory, db) = library_and_db();
        let library = current(&db);
        // 总仓被搬走：必须明确报「根目录不可用」，而不是返回空列表让用户以为
        // 自己的分类全都消失了。
        fs::remove_dir_all(directory.path()).unwrap();
        let error = match list(&db, &library, 1) {
            Ok(targets) => panic!("根目录不可用时不该成功，实际返回 {targets:?}"),
            Err(error) => error,
        };
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("InvalidLibraryRoot"),
            "根目录不可用时应报总仓根无效，实际：{rendered}"
        );
    }
}
