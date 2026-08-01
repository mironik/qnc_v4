//! Read-only filesystem listing for in-app folder browser (shell service).

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FsListResponse {
    pub path: String,
    pub parent: Option<String>,
    pub roots: bool,
    pub entries: Vec<FsEntry>,
}

pub fn list_roots() -> Vec<FsEntry> {
    #[cfg(windows)]
    {
        let mut out = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let p = PathBuf::from(&root);
            if p.is_dir() {
                out.push(FsEntry {
                    name: root.clone(),
                    path: root,
                });
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        vec![FsEntry {
            name: "/".to_string(),
            path: "/".to_string(),
        }]
    }
}

fn normalize_list_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty path".into());
    }
    if trimmed.contains('\0') {
        return Err("invalid path".into());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    Ok(path)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn parent_path(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    #[cfg(windows)]
    {
        let p = display_path(parent);
        if p.len() == 2 && p.as_bytes()[1] == b':' {
            return Some(format!("{}\\", p));
        }
    }
    Some(display_path(parent))
}

fn is_hidden_or_system(name: &str, meta: &fs::Metadata) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        let attrs = meta.file_attributes();
        attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

pub fn list_directory(raw_path: &str) -> Result<FsListResponse, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Ok(FsListResponse {
            path: String::new(),
            parent: None,
            roots: true,
            entries: list_roots(),
        });
    }

    let path = normalize_list_path(trimmed)?;
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("path not accessible: {e}"))?;
    if !canonical.is_dir() {
        return Err("not a directory".into());
    }

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(&canonical).map_err(|e| format!("read_dir failed: {e}"))?;
    for entry in read_dir {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if is_hidden_or_system(&name, &meta) || !meta.is_dir() {
            continue;
        }
        let child = entry.path();
        entries.push(FsEntry {
            name,
            path: display_path(&child),
        });
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(FsListResponse {
        path: display_path(&canonical),
        parent: parent_path(&canonical),
        roots: false,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_returns_roots() {
        let resp = list_directory("").expect("roots");
        assert!(resp.roots);
        assert!(!resp.entries.is_empty());
    }

    #[test]
    fn list_temp_dir() {
        let tmp = std::env::temp_dir();
        let resp = list_directory(&tmp.to_string_lossy()).expect("temp dir");
        assert!(!resp.roots);
        assert!(resp.path.len() > 0);
    }

    #[test]
    fn dot_hidden_dir_is_filtered() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "qnc_shell_fs_hidden_test_{}_{}",
            std::process::id(),
            stamp
        ));
        let visible = base.join("visible");
        let hidden = base.join(".hidden");
        fs::create_dir_all(&visible).expect("visible dir");
        fs::create_dir_all(&hidden).expect("hidden dir");

        let resp = list_directory(&base.to_string_lossy()).expect("list base");
        let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(!names.contains(&".hidden"));

        let _ = fs::remove_dir_all(base);
    }
}
