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
use crate::media::ProjectMediaGateway;
use crate::modules::ModuleStore;
use crate::project::{ProjectDbBroker, ProjectState};
use crate::waveform::WaveformWorker;
use qnc_service_contracts::ExportEngine;

#[derive(Clone)]
pub struct AppState {
    pub root: PathBuf,
    pub config: AppConfig,
    pub background_work: BackgroundWorkGate,
    pub modules: Arc<RwLock<ModuleStore>>,
    pub export: Arc<dyn ExportEngine>,
    pub media_gateway: ProjectMediaGateway,
    pub project: ProjectState,
    pub project_db: ProjectDbBroker,
    pub ingest_card_thumbs: Arc<CardThumbWorker>,
    pub ingest_durations: Arc<DurationWorker>,
    pub ingest_posters: Arc<PosterWorker>,
    pub ingest_proxy: Arc<ProxyGenerateWorker>,
    pub ingest_import: Arc<ImportWorker>,
    pub ingest_audio_wrap: Arc<AudioWrapWorker>,
    pub filmstrip: Arc<FilmstripWorker>,
    pub waveform: Arc<WaveformWorker>,
}
