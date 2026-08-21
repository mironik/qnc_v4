use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::background_work::BackgroundWorkGate;
use crate::config::AppConfig;
use crate::filmstrip::FilmstripWorker;
use crate::ingest_audio_wrap::AudioWrapWorker;
use crate::ingest_card_thumbs::CardThumbWorker;
use crate::ingest_durations::DurationWorker;
use crate::ingest_import::ImportWorker;
use crate::ingest_posters::PosterWorker;
use crate::ingest_proxy::ProxyGenerateWorker;
use crate::modules::ModuleStore;
use crate::project::ProjectState;
use crate::waveform::WaveformWorker;

#[derive(Clone)]
pub struct AppState {
    pub root: PathBuf,
    pub config: AppConfig,
    pub background_work: BackgroundWorkGate,
    pub modules: Arc<RwLock<ModuleStore>>,
    pub project: ProjectState,
    pub ingest_card_thumbs: Arc<CardThumbWorker>,
    pub ingest_durations: Arc<DurationWorker>,
    pub ingest_posters: Arc<PosterWorker>,
    pub ingest_proxy: Arc<ProxyGenerateWorker>,
    pub ingest_import: Arc<ImportWorker>,
    pub ingest_audio_wrap: Arc<AudioWrapWorker>,
    pub filmstrip: Arc<FilmstripWorker>,
    pub waveform: Arc<WaveformWorker>,
}
