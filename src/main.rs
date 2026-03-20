use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
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
mod update;
mod auth;

use crate::models::{
    normalize_level, ApiStatus, CreateReportRequest, CreateUserRequest, GpioConfig, LoginRequest,
    LoginResponse, MeResponse, ReportRecord, UpdateReportRequest, UpdateUserRequest, UserPublic,
    LEVEL_ADVANCED_USER, LEVEL_ADMIN, LEVEL_BASIC, LEVEL_JUNIOR_ADMIN,
};
use crate::store::{JsonStore, StoreError, StoreErrorKind};
use crate::update::{StartUpdateError, UpdateConfig, Updater, UpdateStatusResponse};
use crate::auth::{AuthErrorKind, AuthManager, AuthUser};

#[derive(Clone)]
struct AppState {
    store: Arc<JsonStore>,
    updater: Updater,
    admin_token: Option<String>,
    auth: AuthManager,
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

    let admin_token = std::env::var("CAP_CDTS_ADMIN_TOKEN")
        .ok()
        .and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        });

    let update_enabled = std::env::var("CAP_CDTS_UPDATE_ENABLED")
        .ok()
        .map(|v| v.trim() != "0")
        .unwrap_or(true);

    let repo_dir = std::env::var("CAP_CDTS_REPO_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let state_path = std::env::var("CAP_CDTS_UPDATE_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/update_state.json"));

    let new_bin_default = format!(
        "bin/cap-cdts-backend.new{}",
        std::env::consts::EXE_SUFFIX
    );

    let new_bin = std::env::var("CAP_CDTS_UPDATE_NEW_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(new_bin_default));

    let live_bin = std::env::var("CAP_CDTS_UPDATE_LIVE_BIN")
        .ok()
        .and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(PathBuf::from(t))
            }
        });

    let auto_restart = std::env::var("CAP_CDTS_UPDATE_AUTO_RESTART")
        .ok()
        .map(|v| v.trim() != "0")
        .unwrap_or(true);

    let max_log_lines = std::env::var("CAP_CDTS_UPDATE_LOG_LINES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(400);

    let updater = Updater::open(UpdateConfig {
        enabled: admin_token.is_some() && update_enabled,
        repo_dir,
        state_path,
        new_bin,
        live_bin,
        auto_restart,
        max_log_lines,
    })
    .await?;

    let auth_ttl_secs = std::env::var("CAP_CDTS_AUTH_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(8 * 60 * 60);

    let auth = AuthManager::new(auth_ttl_secs);

    let state = AppState {
        store,
        updater,
        admin_token,
        auth,
    };

    let app = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/me", get(auth_me))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/:username", axum::routing::patch(update_user))
        .route("/api/reports", get(list_reports).post(create_report))
        .route(
            "/api/reports/:num",
            get(get_report)
                .delete(delete_report)
                .patch(update_report),
        )
        .route("/api/gpio/config", get(get_gpio_config).post(set_gpio_config))
        .route("/api/update/status", get(get_update_status))
        .route("/api/update/start", post(start_update))
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

    let addr = std::env::var("CAP_CDTS_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate());

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = async {
                if let Ok(mut s) = sigterm {
                    let _ = s.recv().await;
                }
            } => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }

    tracing::info!("shutdown signal received");
}

async fn api_status() -> Json<ApiStatus> {
    Json(ApiStatus { status: "ok" })
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();

    let t = v.strip_prefix("Bearer ")?.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, ApiError> {
    let token = bearer_token(headers).ok_or_else(|| {
        ApiError::new(StatusCode::UNAUTHORIZED, "missing Authorization: Bearer token")
    })?;

    state
        .auth
        .authenticate(&token)
        .await
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "invalid or expired session"))
}

fn can_manage_users(level: i32) -> bool {
    let lvl = normalize_level(level);
    lvl == LEVEL_ADMIN || lvl == LEVEL_JUNIOR_ADMIN
}

fn can_manage_reports(level: i32) -> bool {
    let lvl = normalize_level(level);
    lvl == LEVEL_ADMIN || lvl == LEVEL_JUNIOR_ADMIN
}

fn can_use_app(level: i32) -> bool {
    // Any authenticated user except basic.
    normalize_level(level) != LEVEL_BASIC
}

fn is_advanced_user(level: i32) -> bool {
    normalize_level(level) == LEVEL_ADVANCED_USER
}

async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    match state
        .auth
        .login(&state.store, &req.username, &req.passcode)
        .await
    {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            let status = match e.kind {
                AuthErrorKind::BadRequest => StatusCode::BAD_REQUEST,
                AuthErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
                AuthErrorKind::Forbidden => StatusCode::FORBIDDEN,
                AuthErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err(ApiError::new(status, e.message))
        }
    }
}

async fn auth_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<MeResponse>> {
    let user = require_user(&state, &headers).await?;
    match state.store.get_user_record(&user.username).await {
        Some(rec) => Ok(Json(MeResponse {
            user: UserPublic::from(&rec),
        })),
        None => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "session user no longer exists",
        )),
    }
}

async fn auth_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let token = bearer_token(&headers).ok_or_else(|| {
        ApiError::new(StatusCode::UNAUTHORIZED, "missing Authorization: Bearer token")
    })?;
    let _ = state.auth.logout(&token).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<UserPublic>>> {
    let user = require_user(&state, &headers).await?;
    if !can_use_app(user.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not permitted"));
    }
    Ok(Json(state.store.list_users().await))
}

async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<Json<UserPublic>> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_users(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }

    // Junior admin cannot create admin or junior admin users.
    if normalize_level(actor.level) == LEVEL_JUNIOR_ADMIN {
        let target = normalize_level(req.level);
        if target == LEVEL_ADMIN || target == LEVEL_JUNIOR_ADMIN {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "junior admin cannot create admin or junior admin users",
            ));
        }
    }

    let created = state.store.create_user(req).await?;
    Ok(Json(created))
}

async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> ApiResult<Json<UserPublic>> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_users(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }

    // Load current user to enforce junior-admin restrictions.
    if normalize_level(actor.level) == LEVEL_JUNIOR_ADMIN {
        if let Some(existing) = state.store.get_user_record(&username).await {
            let existing_level = normalize_level(existing.level);
            if existing_level == LEVEL_ADMIN || existing_level == LEVEL_JUNIOR_ADMIN {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "junior admin cannot modify admin or junior admin accounts",
                ));
            }
        }

        if let Some(new_level) = req.level {
            let target = normalize_level(new_level);
            if target == LEVEL_ADMIN || target == LEVEL_JUNIOR_ADMIN {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "junior admin cannot assign admin or junior admin roles",
                ));
            }
        }
    }

    match state.store.update_user(&username, req).await? {
        Some(updated) => Ok(Json(updated)),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "user not found")),
    }
}

async fn list_reports(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<ReportRecord>>> {
    let actor = require_user(&state, &headers).await?;
    if !can_use_app(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not permitted"));
    }

    let mut reports = state.store.list_reports().await;
    if is_advanced_user(actor.level) {
        reports.retain(|r| !r.person.eq_ignore_ascii_case(&actor.username));
    }

    Ok(Json(reports))
}

async fn create_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateReportRequest>,
) -> ApiResult<Json<ReportRecord>> {
    let actor = require_user(&state, &headers).await?;
    if !can_use_app(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not permitted"));
    }

    let created = state
        .store
        .create_report(&actor.username, actor.level, req)
        .await?;
    Ok(Json(created))
}

async fn get_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(num): Path<u32>,
) -> ApiResult<Json<ReportRecord>> {
    let actor = require_user(&state, &headers).await?;
    if !can_use_app(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "not permitted"));
    }

    match state.store.get_report(num).await {
        Some(r) => {
            if is_advanced_user(actor.level) && r.person.eq_ignore_ascii_case(&actor.username) {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "advanced users cannot view reports about themselves",
                ));
            }
            Ok(Json(r))
        }
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "report not found")),
    }
}

async fn update_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(num): Path<u32>,
    Json(req): Json<UpdateReportRequest>,
) -> ApiResult<Json<ReportRecord>> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_reports(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }

    match state.store.update_report(num, req, &actor.username).await? {
        Some(updated) => Ok(Json(updated)),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "report not found")),
    }
}

async fn delete_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(num): Path<u32>,
) -> ApiResult<StatusCode> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_reports(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }

    let deleted = state.store.delete_report(num).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "report not found"))
    }
}

async fn get_gpio_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<GpioConfig>> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_users(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }
    Ok(Json(state.store.get_gpio_config().await))
}

async fn set_gpio_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(cfg): Json<GpioConfig>,
) -> ApiResult<Json<GpioConfig>> {
    let actor = require_user(&state, &headers).await?;
    if !can_manage_users(actor.level) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "insufficient role"));
    }
    let saved = state.store.set_gpio_config(cfg).await?;
    Ok(Json(saved))
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let required = match state.admin_token.as_deref() {
        Some(t) => t,
        None => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admin token not configured; set CAP_CDTS_ADMIN_TOKEN to enable update endpoints",
            ))
        }
    };

    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();

    if provided.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing X-Admin-Token header",
        ));
    }

    if provided != required {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid admin token",
        ));
    }

    Ok(())
}

async fn get_update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<UpdateStatusResponse>> {
    require_admin(&state, &headers)?;
    Ok(Json(state.updater.status().await))
}

async fn start_update(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<UpdateStatusResponse>> {
    require_admin(&state, &headers)?;

    match state.updater.start().await {
        Ok(()) => Ok(Json(state.updater.status().await)),
        Err(StartUpdateError::Disabled) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "update feature is disabled (check CAP_CDTS_UPDATE_ENABLED and CAP_CDTS_ADMIN_TOKEN)",
        )),
        Err(StartUpdateError::AlreadyRunning) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "an update is already running",
        )),
    }
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
