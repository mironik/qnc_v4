//! Neutral editorial-shot domain (root + derived virtual shots).
//!
//! Single owner of the `virtual_shots` model. Ingest, QStory and Media Pool all
//! consume this module; it must not be owned by any of them.

pub(crate) mod db;

pub(crate) use db::{
    add_virtual_shot, cover_path_for_shot, derive_virtual_shot,
    ensure_reserved_root_shots_for_project, ensure_root_virtual_shots, list_virtual_shots,
    sync_root_virtual_shots_with_selection, update_virtual_shot, virtual_shot_frames,
};
