use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

mod models;
mod store;

use crate::models::{
    ApiStatus, CreateReportRequest, CreateUserRequest, GpioConfig, ReportRecord, UserPublic,
};
use crate::store::{JsonStore, StoreError, StoreErrorKind};

#[derive(Clone)]
struct AppState {
    store: Arc<JsonStore>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(value: StoreError) -> Self {
        let status = match value.kind {
            StoreErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            StoreErrorKind::Conflict => StatusCode::CONFLICT,
            StoreErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };

        ApiError::new(status, value.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct ErrBody {
            error: String,
        }

        let body = Json(ErrBody {
            error: self.message,
        });

        (self.status, body).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let store_path = std::env::var("CAP_CDTS_STORE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/store.json"));

    let store = Arc::new(JsonStore::open(store_path).await?);

    let state = AppState { store };

    let app = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/reports", get(list_reports).post(create_report))
        .route("/api/reports/:num", get(get_report).delete(delete_report))
        .route("/api/gpio/config", get(get_gpio_config).post(set_gpio_config))
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new("2.0").append_index_html_on_directories(true))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );

    let addr = "0.0.0.0:8080";
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

async fn api_status() -> Json<ApiStatus> {
    Json(ApiStatus { status: "ok" })
}

async fn list_users(State(state): State<AppState>) -> Json<Vec<UserPublic>> {
    Json(state.store.list_users().await)
}

async fn create_user(
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<Json<UserPublic>> {
    let created = state.store.create_user(req).await?;
    Ok(Json(created))
}

async fn list_reports(State(state): State<AppState>) -> Json<Vec<ReportRecord>> {
    Json(state.store.list_reports().await)
}

async fn create_report(
    State(state): State<AppState>,
    Json(req): Json<CreateReportRequest>,
) -> ApiResult<Json<ReportRecord>> {
    let created = state.store.create_report(req).await?;
    Ok(Json(created))
}

async fn get_report(
    State(state): State<AppState>,
    Path(num): Path<u32>,
) -> ApiResult<Json<ReportRecord>> {
    match state.store.get_report(num).await {
        Some(r) => Ok(Json(r)),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "report not found")),
    }
}

async fn delete_report(
    State(state): State<AppState>,
    Path(num): Path<u32>,
) -> ApiResult<StatusCode> {
    let deleted = state.store.delete_report(num).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "report not found"))
    }
}

async fn get_gpio_config(State(state): State<AppState>) -> Json<GpioConfig> {
    Json(state.store.get_gpio_config().await)
}

async fn set_gpio_config(
    State(state): State<AppState>,
    Json(cfg): Json<GpioConfig>,
) -> ApiResult<Json<GpioConfig>> {
    let saved = state.store.set_gpio_config(cfg).await?;
    Ok(Json(saved))
}

async fn ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_stream(state, socket))
}

async fn ws_stream(state: AppState, mut socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    let mut rx = state.store.subscribe_live();

    // Send an initial snapshot.
    let initial = rx.borrow().clone();
    if let Ok(text) = serde_json::to_string(&initial) {
        if socket.send(Message::Text(text)).await.is_err() {
            return;
        }
    }

    // Stream updates.
    loop {
        if rx.changed().await.is_err() {
            break;
        }

        let snapshot = rx.borrow().clone();
        let text = match serde_json::to_string(&snapshot) {
            Ok(t) => t,
            Err(_) => continue,
        };

        if socket.send(Message::Text(text)).await.is_err() {
            break;
        }
    }
}
