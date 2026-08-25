//! Shared media catalog helpers (clip FPS, proxy paths, enriched lists).
//! Not the Media Pool plugin form — that plugin is out of the product path.
//! HTTP `/api/media-pool/*` is not mounted.

mod db;
mod ingest_db;
mod store;
#[allow(dead_code)]
mod transcripts;

pub(crate) use ingest_db::{
    backfill_all_imported_metadata, proxy_path_for_clip, read_imported_clips,
    resolve_stored_clip_fps, resolve_stored_clip_timebase,
};
pub(crate) use store::{list_clips_enriched, mark_filmstrip_building};
