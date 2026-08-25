use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use qnc_service_contracts::{MediaResolveRequest, MediaResolveResponse, ServiceError};

use crate::app_state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/media/resolve", post(api_media_resolve))
}

async fn api_media_resolve(
    State(app): State<AppState>,
    Json(mut request): Json<MediaResolveRequest>,
) -> Result<Json<MediaResolveResponse>, (StatusCode, String)> {
    if request.project_id.trim().is_empty() {
        request.project_id = app.project.active_project_id()?;
    }
    app.media_gateway
        .resolve_sync(request)
        .map(Json)
        .map_err(map_service_error)
}

fn map_service_error(error: ServiceError) -> (StatusCode, String) {
    let status = match error.code.as_str() {
        "media_resolve_invalid_request" => StatusCode::BAD_REQUEST,
        "media_fallback_not_local" => StatusCode::BAD_REQUEST,
        "enterprise_media_route_missing" => StatusCode::BAD_REQUEST,
        "media_resolve_failed" if error.message.contains("_missing:") => StatusCode::NOT_FOUND,
        "filmstrip_media_missing" | "poster_media_missing" | "waveform_media_missing" => {
            StatusCode::NOT_FOUND
        }
        _ => StatusCode::BAD_REQUEST,
    };
    (status, error.message)
}
