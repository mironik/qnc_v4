//! Workspace paths owned by the host (no web `app/` / `plugins/` tree).

use std::path::{Path, PathBuf};

pub fn seed_dir(root: &Path) -> PathBuf {
    root.join("seed")
}

pub fn system_seed(root: &Path) -> PathBuf {
    seed_dir(root).join("system_seed.json")
}

pub fn keyboard_shortcuts(root: &Path) -> PathBuf {
    seed_dir(root).join("keyboard-shortcuts.json")
}

pub fn tabs_dir(root: &Path) -> PathBuf {
    seed_dir(root).join("tabs")
}

pub fn components_dir(root: &Path) -> PathBuf {
    seed_dir(root).join("components")
}

pub fn design_tokens(root: &Path) -> PathBuf {
    seed_dir(root).join("design").join("tokens.json")
}

/// True if this directory looks like the QNC workspace root.
pub fn looks_like_root(path: &Path) -> bool {
    path.join("seed").join("system_seed.json").is_file()
        || path.join("qnc-host").join("Cargo.toml").is_file()
        || path.join("Cargo.toml").is_file() && path.join("qnc-app").is_dir()
}
