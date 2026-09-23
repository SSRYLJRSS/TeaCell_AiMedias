//! ffmpeg/ffprobe 的统一解析入口。
//!
//! 发布构建只使用随包 sidecar；debug 构建才允许 PATH fallback。这样安装包在没有
//! 系统 ffmpeg 的机器上仍能完成视频五步闭环，同时避免不同模块各自探测产生漂移。

use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTool {
    Ffmpeg,
    Ffprobe,
}

impl MediaTool {
    fn base_name(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }

    fn file_name(self) -> String {
        if cfg!(target_os = "windows") {
            format!("{}.exe", self.base_name())
        } else {
            self.base_name().to_string()
        }
    }
}

fn probe(path: &std::path::Path) -> bool {
    Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn sidecar_candidate(tool: MediaTool) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(tool.file_name());
    candidate.is_file().then_some(candidate)
}

fn path_candidate(tool: MediaTool) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(tool.file_name()))
        .find(|candidate| candidate.is_file())
}

pub fn resolve(tool: MediaTool) -> Option<PathBuf> {
    if let Some(path) = sidecar_candidate(tool).filter(|path| probe(path)) {
        return Some(path);
    }
    if cfg!(debug_assertions) {
        return path_candidate(tool).filter(|path| probe(path));
    }
    None
}

pub fn ffmpeg_path() -> Option<PathBuf> {
    resolve(MediaTool::Ffmpeg)
}

pub fn ffprobe_path() -> Option<PathBuf> {
    resolve(MediaTool::Ffprobe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_target() {
        let name = MediaTool::Ffmpeg.file_name();
        assert_eq!(name.ends_with(".exe"), cfg!(target_os = "windows"));
    }
}
