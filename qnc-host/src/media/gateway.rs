use std::path::PathBuf;

use async_trait::async_trait;
use qnc_service_contracts::{
    IntegrationGatewayKind, IntegrationGatewayRoutes, MediaAccessKind, MediaGateway, MediaLocator,
    MediaRef, MediaResolveRequest, MediaResolveResponse, ServiceError, ServiceResult,
};
use serde_json::json;

use crate::project::db::ProjectPaths;

use super::{
    resolve_filmstrip_media, resolve_original_media, resolve_play_media, resolve_poster_media,
    resolve_waveform_media, PlayMediaKind,
};

#[derive(Clone)]
pub struct ProjectMediaGateway {
    paths: ProjectPaths,
    kind: IntegrationGatewayKind,
    endpoint: Option<String>,
    routes: IntegrationGatewayRoutes,
    read_only: bool,
}

impl ProjectMediaGateway {
    #[cfg(test)]
    pub fn new(paths: ProjectPaths, kind: IntegrationGatewayKind, read_only: bool) -> Self {
        Self::with_routes(
            paths,
            kind,
            read_only,
            None,
            IntegrationGatewayRoutes::default(),
        )
    }

    pub fn with_routes(
        paths: ProjectPaths,
        kind: IntegrationGatewayKind,
        read_only: bool,
        endpoint: Option<String>,
        routes: IntegrationGatewayRoutes,
    ) -> Self {
        Self {
            paths,
            kind,
            endpoint,
            routes,
            read_only,
        }
    }

    pub fn resolve_sync(
        &self,
        request: MediaResolveRequest,
    ) -> ServiceResult<MediaResolveResponse> {
        let project_id = request.project_id.trim();
        let clip_id = request.clip_id.trim();
        if project_id.is_empty() || clip_id.is_empty() {
            return Err(ServiceError::new(
                "media_resolve_invalid_request",
                "project_id and clip_id are required.",
            ));
        }

        let (path, resolved_kind, mut metadata) = match request.access {
            MediaAccessKind::PlaybackProxy => {
                let media =
                    resolve_play_media(&self.paths, project_id, clip_id).map_err(resolve_error)?;
                let resolved_kind = match media.kind {
                    PlayMediaKind::Proxy => "playback_proxy",
                    PlayMediaKind::Original => "playback_original_fallback",
                };
                let metadata = playback_metadata_json(
                    &media,
                    json!({
                        "field_order": media.field_order,
                        "interlaced": media.interlaced,
                        "source_class": media.source_class,
                        "proxy_recipe": media.proxy_recipe,
                    }),
                );
                (media.path, resolved_kind, metadata)
            }
            MediaAccessKind::OriginalMaster => {
                let media = resolve_original_media(&self.paths, project_id, clip_id)
                    .map_err(resolve_error)?;
                let metadata = playback_metadata_json(
                    &media,
                    json!({
                        "field_order": media.field_order,
                        "interlaced": media.interlaced,
                        "source_class": media.source_class,
                        "proxy_recipe": media.proxy_recipe,
                    }),
                );
                (media.path, "original_master", metadata)
            }
            MediaAccessKind::FilmstripSource => {
                let fallback = local_fallback_path(request.fallback.as_ref())?;
                let media =
                    resolve_filmstrip_media(&self.paths, project_id, clip_id, fallback.as_deref())
                        .ok_or_else(|| {
                            ServiceError::new(
                                "filmstrip_media_missing",
                                format!("Filmstrip media for clip '{clip_id}' was not found."),
                            )
                        })?;
                (media, "filmstrip_source", json!({}))
            }
            MediaAccessKind::PosterSource => {
                let media =
                    resolve_poster_media(&self.paths, project_id, clip_id).ok_or_else(|| {
                        ServiceError::new(
                            "poster_media_missing",
                            format!("Poster source media for clip '{clip_id}' was not found."),
                        )
                    })?;
                (media, "poster_source", json!({}))
            }
            MediaAccessKind::WaveformSource => {
                let media =
                    resolve_waveform_media(&self.paths, project_id, clip_id).ok_or_else(|| {
                        ServiceError::new(
                            "waveform_media_missing",
                            format!("Waveform source media for clip '{clip_id}' was not found."),
                        )
                    })?;
                (media, "waveform_source", json!({}))
            }
        };
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert("resolver".into(), json!("project_media_gateway"));
            obj.insert("resolved_kind".into(), json!(resolved_kind));
        }

        Ok(MediaResolveResponse {
            media: MediaRef {
                clip_id: clip_id.to_string(),
                locator: self.locator_for_gateway(
                    request.access,
                    project_id,
                    clip_id,
                    resolved_kind,
                    path,
                )?,
            },
            access: request.access,
            gateway_kind: self.kind,
            read_only: self.read_only,
            metadata,
        })
    }

    fn locator_for_gateway(
        &self,
        access: MediaAccessKind,
        project_id: &str,
        clip_id: &str,
        resolved_kind: &str,
        path: PathBuf,
    ) -> ServiceResult<MediaLocator> {
        match self.kind {
            IntegrationGatewayKind::LocalFs | IntegrationGatewayKind::SharedFs => {
                Ok(MediaLocator::LocalPath { path })
            }
            IntegrationGatewayKind::EnterpriseProxy => {
                let template = self.routes.route_for(access).ok_or_else(|| {
                    ServiceError::new(
                        "enterprise_media_route_missing",
                        format!("Enterprise gateway route nije konfiguriran za '{resolved_kind}'."),
                    )
                })?;
                Ok(MediaLocator::IntranetPath {
                    uri: expand_route_template(
                        self.endpoint.as_deref(),
                        template,
                        project_id,
                        clip_id,
                        resolved_kind,
                    ),
                })
            }
        }
    }
}

#[async_trait]
impl MediaGateway for ProjectMediaGateway {
    async fn resolve(&self, request: MediaResolveRequest) -> ServiceResult<MediaResolveResponse> {
        self.resolve_sync(request)
    }
}

fn playback_metadata_json(
    media: &super::play::PlayMedia,
    mut metadata: serde_json::Value,
) -> serde_json::Value {
    if let Some(obj) = metadata.as_object_mut() {
        if let (Some(fps_num), Some(fps_den)) = (media.fps_num, media.fps_den) {
            obj.insert(
                "source_timebase".into(),
                json!({
                    "fps_num": fps_num,
                    "fps_den": fps_den,
                }),
            );
            obj.insert("fps".into(), json!(f64::from(fps_num) / f64::from(fps_den)));
        }
        if let Some(duration_sec) = media.duration_sec {
            obj.insert("duration_sec".into(), json!(duration_sec));
        }
        if let Some(duration_frames) = media.duration_frames {
            obj.insert("duration_frames".into(), json!(duration_frames));
        }
        if let Some(has_audio) = media.has_audio {
            obj.insert("has_audio".into(), json!(has_audio));
        }
        if let Some(audio_channels) = media.audio_channels {
            obj.insert("audio_channels".into(), json!(audio_channels));
        }
    }
    metadata
}

fn resolve_error(error: String) -> ServiceError {
    ServiceError::new("media_resolve_failed", error)
}

fn local_fallback_path(
    locator: Option<&MediaLocator>,
) -> ServiceResult<Option<std::path::PathBuf>> {
    match locator {
        Some(MediaLocator::LocalPath { path }) => Ok(Some(path.clone())),
        Some(MediaLocator::IntranetPath { .. }) | Some(MediaLocator::ManagedAsset { .. }) => {
            Err(ServiceError::new(
                "media_fallback_not_local",
                "Fallback media locator is not directly available to the local resolver.",
            ))
        }
        None => Ok(None),
    }
}

fn expand_route_template(
    endpoint: Option<&str>,
    template: &str,
    project_id: &str,
    clip_id: &str,
    resolved_kind: &str,
) -> String {
    let expanded = template
        .trim()
        .replace("{project_id}", &route_token(project_id))
        .replace("{clip_id}", &route_token(clip_id))
        .replace("{access}", &route_token(resolved_kind))
        .replace("{resolved_kind}", &route_token(resolved_kind));
    if expanded.contains("://") || expanded.starts_with("\\\\") || endpoint.is_none() {
        return expanded;
    }
    let endpoint = endpoint.unwrap_or_default().trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return expanded;
    }
    let route = expanded.trim_start_matches('/');
    format!("{endpoint}/{route}")
}

fn route_token(value: &str) -> String {
    value
        .trim()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qnc_service_contracts::{MediaAccessKind, MediaGateway, MediaResolveRequest};
    use rusqlite::params;

    use super::*;
    use crate::project::db::{open_global, open_project};

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: PathBuf::from("nonexistent"),
        }
    }

    fn setup_project(paths: &ProjectPaths, project_id: &str) -> std::path::PathBuf {
        let _ = open_global(paths);
        let project_dir = paths.projects_root.join(project_id);
        std::fs::create_dir_all(project_dir.join("proxy")).unwrap();
        std::fs::create_dir_all(project_dir.join("original")).unwrap();
        let _ = open_project(paths, project_id).unwrap();
        project_dir
    }

    fn unique_base(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "qnc_gateway_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[tokio::test]
    async fn project_gateway_resolves_playback_proxy() {
        let base = unique_base("proxy");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_proxy";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_a";
        let proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::write(&proxy, b"proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, 'mp4')",
            params![clip_id, proxy.to_string_lossy().to_string()],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::LocalFs, true);
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::PlaybackProxy,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(resolved.access, MediaAccessKind::PlaybackProxy);
        assert_eq!(resolved.gateway_kind, IntegrationGatewayKind::LocalFs);
        assert!(resolved.read_only);
        assert_eq!(
            resolved.media.locator,
            MediaLocator::LocalPath {
                path: proxy.clone()
            }
        );
        assert_eq!(
            resolved
                .metadata
                .get("source_timebase")
                .and_then(|value| value.get("fps_num"))
                .and_then(|value| value.as_u64()),
            Some(50)
        );
        assert_eq!(
            resolved
                .metadata
                .get("duration_frames")
                .and_then(|value| value.as_i64()),
            Some(50)
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn project_gateway_resolves_playback_original_fallback_when_proxy_missing() {
        let base = unique_base("play_original_fallback");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_play_original";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_original";
        let original = project_dir.join("card").join(format!("{clip_id}.mxf"));
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"original").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, source_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'detected',
                     0, ?2, 'mxf')",
            params![clip_id, original.to_string_lossy().to_string()],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::LocalFs, true);
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::PlaybackProxy,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(resolved.access, MediaAccessKind::PlaybackProxy);
        assert_eq!(
            resolved
                .metadata
                .get("resolved_kind")
                .and_then(|value| value.as_str()),
            Some("playback_original_fallback")
        );
        assert_eq!(
            resolved.media.locator,
            MediaLocator::LocalPath {
                path: original.clone()
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn project_gateway_resolves_original_master() {
        let base = unique_base("original");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_original";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_b";
        let proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        let original = project_dir.join("original").join(format!("{clip_id}.mxf"));
        std::fs::write(&proxy, b"proxy").unwrap();
        std::fs::write(&original, b"original").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, original_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, ?3, 'mxf')",
            params![
                clip_id,
                proxy.to_string_lossy().to_string(),
                original.to_string_lossy().to_string()
            ],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::LocalFs, true);
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::OriginalMaster,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(resolved.access, MediaAccessKind::OriginalMaster);
        assert_eq!(
            resolved.media.locator,
            MediaLocator::LocalPath {
                path: original.clone()
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn project_gateway_resolves_poster_source_from_proxy_columns() {
        let base = unique_base("poster");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_poster";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_c";
        let card_proxy = project_dir.join("card").join(format!("{clip_id}.mp4"));
        let project_proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::create_dir_all(card_proxy.parent().unwrap()).unwrap();
        std::fs::write(&card_proxy, b"card-proxy").unwrap();
        std::fs::write(&project_proxy, b"project-proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?3, 'mp4')",
            params![
                clip_id,
                card_proxy.to_string_lossy().to_string(),
                project_proxy.to_string_lossy().to_string()
            ],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::LocalFs, true);
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::PosterSource,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(resolved.access, MediaAccessKind::PosterSource);
        assert_eq!(
            resolved.media.locator,
            MediaLocator::LocalPath {
                path: card_proxy.clone()
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn project_gateway_resolves_waveform_source_from_imported_media() {
        let base = unique_base("waveform");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_waveform";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_d";
        let project_proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::write(&project_proxy, b"project-proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, 'mp4')",
            params![clip_id, project_proxy.to_string_lossy().to_string()],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::LocalFs, true);
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::WaveformSource,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(resolved.access, MediaAccessKind::WaveformSource);
        assert_eq!(
            resolved.media.locator,
            MediaLocator::LocalPath {
                path: project_proxy.clone()
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn enterprise_gateway_requires_configured_media_route() {
        let base = unique_base("enterprise");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway_enterprise";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip_e";
        let proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::write(&proxy, b"proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, 'mp4')",
            params![clip_id, proxy.to_string_lossy().to_string()],
        )
        .unwrap();

        let gateway =
            ProjectMediaGateway::new(paths.clone(), IntegrationGatewayKind::EnterpriseProxy, true);
        let error = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::PlaybackProxy,
                fallback: None,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code, "enterprise_media_route_missing");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn enterprise_gateway_uses_configured_media_route() {
        let base = unique_base("enterprise_route");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "gateway enterprise";
        let project_dir = setup_project(&paths, project_id);
        let clip_id = "clip e";
        let proxy = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::write(&proxy, b"proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, 'mp4')",
            params![clip_id, proxy.to_string_lossy().to_string()],
        )
        .unwrap();

        let gateway = ProjectMediaGateway::with_routes(
            paths.clone(),
            IntegrationGatewayKind::EnterpriseProxy,
            true,
            Some("http://mam-gateway.local/qnc".into()),
            IntegrationGatewayRoutes {
                playback_proxy: Some("/media/{access}/{project_id}/{clip_id}".into()),
                ..IntegrationGatewayRoutes::default()
            },
        );
        let resolved = gateway
            .resolve(MediaResolveRequest {
                project_id: project_id.into(),
                clip_id: clip_id.into(),
                access: MediaAccessKind::PlaybackProxy,
                fallback: None,
            })
            .await
            .unwrap();

        assert_eq!(
            resolved.gateway_kind,
            IntegrationGatewayKind::EnterpriseProxy
        );
        assert_eq!(
            resolved.media.locator,
            MediaLocator::IntranetPath {
                uri: "http://mam-gateway.local/qnc/media/playback_proxy/gateway%20enterprise/clip%20e"
                    .into()
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
