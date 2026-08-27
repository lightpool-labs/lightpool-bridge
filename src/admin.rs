// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::net::SocketAddr;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::config::{BridgeConfigError, BridgeLinkConfig};
use crate::events::{BridgeEventKind, BridgeEventRecord, EventsPage};
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
    #[serde(default = "default_event_page")]
    page: u32,
    #[serde(default = "default_event_page_size")]
    page_size: u32,
}

fn default_event_page() -> u32 {
    1
}

fn default_event_page_size() -> u32 {
    50
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventsWsMessage {
    Event {
        event: BridgeEventRecord,
    },
    Status {
        status: BridgeStatusResponse,
    },
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
        .route("/api/ws/events", get(events_ws))
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
) -> Json<EventsPage> {
    Json(
        state
            .bridge
            .events_page(
                q.route_id.as_deref(),
                q.token.as_deref(),
                q.page,
                q.page_size,
            )
            .await,
    )
}

async fn events_ws(
    ws: WebSocketUpgrade,
    Query(q): Query<EventsQuery>,
    State(state): State<AdminState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| events_ws_loop(socket, state, q))
}

async fn events_ws_loop(mut socket: WebSocket, state: AdminState, q: EventsQuery) {
    let route_id = q.route_id.filter(|id| !id.is_empty());
    let token = q.token.filter(|t| !t.is_empty());

    let status = state.bridge.status().await;
    if send_ws_json(&mut socket, &EventsWsMessage::Status { status })
        .await
        .is_err()
    {
        return;
    }

    let mut rx = state.bridge.subscribe_events();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            received = rx.recv() => {
                match received {
                    Ok(event) => {
                        if route_id.as_ref().is_some_and(|r| event.route_id != *r) {
                            continue;
                        }
                        if token.as_ref().is_some_and(|t| event.token.as_deref() != Some(t.as_str())) {
                            continue;
                        }
                        if send_ws_json(&mut socket, &EventsWsMessage::Event { event: event.clone() }).await.is_err() {
                            break;
                        }
                        if matches!(event.kind, BridgeEventKind::ConfigUpdated | BridgeEventKind::RouteReloaded) {
                            let status = state.bridge.status().await;
                            if send_ws_json(&mut socket, &EventsWsMessage::Status { status }).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_ws_json(socket: &mut WebSocket, message: &EventsWsMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket.send(Message::Text(text)).await.map_err(|_| ())
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
