// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::net::SocketAddr;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::config::{BridgeConfigError, BridgeLinkConfig};
use crate::events::BridgeEventRecord;
use crate::handle::{BridgeHandle, BridgeStatusResponse};
use crate::route_config::BridgeRoute;

const INDEX_HTML: &str = include_str!("../admin/static/index.html");

#[derive(Clone)]
pub struct AdminState {
    pub bridge: BridgeHandle,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    config_path: String,
    route_count: usize,
    enabled_route_count: usize,
}

#[derive(Debug, Deserialize)]
struct PutConfigRequest {
    config: BridgeLinkConfig,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    route_id: Option<String>,
    token: Option<String>,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

fn default_event_limit() -> usize {
    100
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

pub async fn run_embedded(bridge: BridgeHandle, listen: SocketAddr) -> anyhow::Result<()> {
    let state = AdminState { bridge };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/routes", get(list_routes).post(create_route))
        .route("/api/routes/:id", put(update_route).delete(delete_route))
        .with_state(state);

    log::info!("Bridge admin UI at http://{}", listen);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(state): State<AdminState>) -> Json<HealthResponse> {
    let config = state.bridge.config().await;
    Json(HealthResponse {
        ok: true,
        config_path: state.bridge.config_path().display().to_string(),
        route_count: config.routes.len(),
        enabled_route_count: config.routes.iter().filter(|r| r.enabled).count(),
    })
}

async fn status(State(state): State<AdminState>) -> Json<BridgeStatusResponse> {
    Json(state.bridge.status().await)
}

async fn events(
    State(state): State<AdminState>,
    Query(q): Query<EventsQuery>,
) -> Json<Vec<BridgeEventRecord>> {
    Json(
        state
            .bridge
            .events(q.route_id.as_deref(), q.token.as_deref(), q.limit)
            .await,
    )
}

async fn get_config(State(state): State<AdminState>) -> Json<BridgeLinkConfig> {
    Json(state.bridge.config().await)
}

async fn put_config(
    State(state): State<AdminState>,
    Json(body): Json<PutConfigRequest>,
) -> Result<Json<BridgeLinkConfig>, AppError> {
    let config = state
        .bridge
        .update_config(body.config)
        .await
        .map_err(AppError::from_config)?;
    Ok(Json(config))
}

async fn list_routes(State(state): State<AdminState>) -> Json<Vec<BridgeRoute>> {
    Json(state.bridge.config().await.routes)
}

async fn create_route(
    State(state): State<AdminState>,
    Json(route): Json<BridgeRoute>,
) -> Result<Json<BridgeRoute>, AppError> {
    let mut config = state.bridge.config().await;
    if route.id.trim().is_empty() {
        return Err(AppError::bad_request("route id must not be empty"));
    }
    if config.routes.iter().any(|r| r.id == route.id) {
        return Err(AppError::bad_request(format!(
            "route id already exists: {}",
            route.id
        )));
    }
    config.routes.push(route.clone());
    state
        .bridge
        .update_config(config)
        .await
        .map_err(AppError::from_config)?;
    Ok(Json(route))
}

async fn update_route(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(route): Json<BridgeRoute>,
) -> Result<Json<BridgeRoute>, AppError> {
    if route.id != id {
        return Err(AppError::bad_request("route id in path and body must match"));
    }
    let mut config = state.bridge.config().await;
    let Some(index) = config.routes.iter().position(|r| r.id == id) else {
        return Err(AppError::not_found(format!("route not found: {}", id)));
    };
    config.routes[index] = route.clone();
    state
        .bridge
        .update_config(config)
        .await
        .map_err(AppError::from_config)?;
    Ok(Json(route))
}

async fn delete_route(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let mut config = state.bridge.config().await;
    let before = config.routes.len();
    config.routes.retain(|r| r.id != id);
    if config.routes.len() == before {
        return Err(AppError::not_found(format!("route not found: {}", id)));
    }
    state
        .bridge
        .update_config(config)
        .await
        .map_err(AppError::from_config)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn from_config(err: BridgeConfigError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CONTENT_TYPE, "application/json")],
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
