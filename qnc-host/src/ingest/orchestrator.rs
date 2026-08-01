//! Tanki orkestrator — samo enqueuea potrebne ingest workere (bez poslovne logike).

use crate::app_state::AppState;

/// Nakon discover / browse / register — grid je u SQLite; thumb + duration u pozadini.
pub fn after_discover(app: &AppState, project_id: &str) {
    app.ingest_card_thumbs.enqueue(project_id);
    app.ingest_durations.enqueue(project_id);
}

/// Nakon POST import — copy/link → import worker; generate → proxy;
/// audio wrap (AV+TC) čeka fps iz SQLite kao waveform peaks.
pub fn after_import_queued(app: &AppState, project_id: &str) {
    app.ingest_import.enqueue(project_id);
    app.ingest_audio_wrap.enqueue(project_id);
}
