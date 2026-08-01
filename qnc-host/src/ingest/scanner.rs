//! Sken medija na disku (ekvivalent Jetson `ingest_scanner.py`).
//! Samo čitanje filesystema + grupiranje — upis u bazu je u `store`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::media::{is_card_thumb_file, is_media_file, resolve_card_media_root};

pub const DEFAULT_SCAN_DEPTH: u32 = 8;

pub struct ScanInventory {
    pub browse_root: PathBuf,
    pub card_root: PathBuf,
    pub media_files: Vec<PathBuf>,
    pub thumb_files: Vec<PathBuf>,
}

/// Skenira browse mapu (MXF/proxy) i THM/JPG na card rootu (npr. FX9 Thmbnl).
pub fn scan_inventory(browse_root: &Path) -> ScanInventory {
    let card_root = resolve_card_media_root(browse_root);
    let thumb_files = scan_card_thumb_files(&card_root, DEFAULT_SCAN_DEPTH);
    ScanInventory {
        browse_root: browse_root.to_path_buf(),
        card_root,
        media_files: scan_media_files(browse_root, DEFAULT_SCAN_DEPTH),
        thumb_files,
    }
}

pub fn scan_media_files(root: &Path, max_depth: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![(root.to_path_buf(), 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_media_file(&path) {
                out.push(path);
            } else if path.is_dir() && depth < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    out.sort();
    out
}

pub fn scan_card_thumb_files(root: &Path, max_depth: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![(root.to_path_buf(), 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_card_thumb_file(&path) {
                out.push(path);
            } else if path.is_dir() && depth < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    out.sort();
    out
}
