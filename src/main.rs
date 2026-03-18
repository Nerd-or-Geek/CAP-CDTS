use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    process::{Command, exit},
    sync::Arc,
    thread,
};
use tokio::signal;
use tracing::info;
use tower_http::cors::CorsLayer;

// --- Embedded HTML ---
const INDEX_HTML: &str = include_str!("../static/index.html");

// --- App State ---
#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    rfid: Arc<Mutex<RfidReader>>,
    version: String,
    repo: String,
}

// --- RFID Reader (simulated for now, replace with rc522 crate) ---
struct RfidReader {
    last_uid: Option<String>,
    last_text: Option<String>,
}

impl RfidReader {
    fn new() -> Self {
        Self {
            last_uid: None,
            last_text: None,
        }
    }

    fn read(&mut self) -> Result<(String, String), String> {
        // TODO: Replace with real RC522 read logic using rppal or rc522 crate
        // For now, simulate a read
        self.last_uid = Some("DEADBEEF".to_string());
        self.last_text = Some("Hello, Cyberdeck!".to_string());
        Ok((
            self.last_uid.clone().unwrap(),
            self.last_text.clone().unwrap(),
        ))
    }

    fn write(&mut self, text: &str) -> Result<String, String> {
        // TODO: Replace with real RC522 write logic
        self.last_text = Some(text.to_string());
        Ok("DEADBEEF".to_string())
    }
}

// --- API Types ---
#[derive(Serialize)]
struct ReadResponse {
    uid: String,
    text: String,
    success: bool,
}

#[derive(Deserialize)]
struct WriteRequest {
    text: String,
}

#[derive(Serialize)]
struct StatusResponse {
    version: String,
    repo: String,
    status: String,
}

#[derive(Serialize)]
struct Card {
    uid: String,
    label: String,
    text: String,
}

#[derive(Deserialize)]
struct LabelRequest {
    uid: String,
    label: String,
}

// --- API Handlers ---
async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn api_read(State(state): State<AppState>) -> impl IntoResponse {
    let mut rfid = state.rfid.lock();
    match rfid.read() {
        Ok((uid, text)) => Json(ReadResponse {
            uid,
            text,
            success: true,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ReadResponse {
                uid: String::new(),
                text: e,
                success: false,
            }),
        )
            .into_response(),
    }
}

async fn api_write(
    State(state): State<AppState>,
    Json(payload): Json<WriteRequest>,
) -> impl IntoResponse {
    let mut rfid = state.rfid.lock();
    match rfid.write(&payload.text) {
        Ok(uid) => {
            let db = state.db.lock();
            let _ = db.execute(
                "INSERT OR IGNORE INTO cards (uid, label, text) VALUES (?1, '', ?2)",
                params![&uid, &payload.text],
            );
            Json(ReadResponse {
                uid,
                text: payload.text,
                success: true,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ReadResponse {
                uid: String::new(),
                text: e,
                success: false,
            }),
        )
            .into_response(),
    }
}

async fn api_cards(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.lock();
    let mut cards = Vec::new();
    
    if let Ok(mut stmt) = db.prepare("SELECT uid, label, text FROM cards") {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok(Card {
                uid: row.get(0)?,
                label: row.get(1)?,
                text: row.get(2)?,
            })
        }) {
            cards = rows.filter_map(Result::ok).collect();
        }
    }
    
    Json(cards)
}

async fn api_label(
    State(state): State<AppState>,
    Json(payload): Json<LabelRequest>,
) -> impl IntoResponse {
    let db = state.db.lock();
    let _ = db.execute(
        "UPDATE cards SET label = ?1 WHERE uid = ?2",
        params![payload.label, payload.uid],
    );
    StatusCode::OK
}

async fn api_status(State(state): State<AppState>) -> impl IntoResponse {
    Json(StatusResponse {
        version: state.version.clone(),
        repo: state.repo.clone(),
        status: "ok".to_string(),
    })
}

async fn api_update(State(state): State<AppState>) -> impl IntoResponse {
    let repo = state.repo.clone();
    thread::spawn(move || {
        let _ = do_self_update(&repo);
    });
    StatusCode::ACCEPTED
}

// --- Self-Updater ---
fn do_self_update(repo: &str) -> Result<(), String> {
    use self_update::backends::github::Update;
    use self_update::cargo_crate_version;

    let parts: Vec<&str> = repo.split('/').collect();
    let owner = parts.get(0).copied().unwrap_or("yourusername");
    let name = parts.get(1).copied().unwrap_or("rfid-cyberdeck-rust");

    info!("Checking for update from {}/{}", owner, name);

    let status = Update::configure()
        .repo_owner(owner)
        .repo_name(name)
        .bin_name("rfid-cyberdeck-rust")
        .show_download_progress(true)
        .current_version(cargo_crate_version!())
        .target(self_update::get_target())
        .build()
        .map_err(|e| e.to_string())?
        .update()
        .map_err(|e| e.to_string())?;

    if status.updated() {
        info!("Updated! Restarting...");
        let exe = std::env::current_exe().unwrap();
        let _ = Command::new(exe)
            .args(std::env::args().skip(1))
            .spawn();
        exit(0);
    }

    Ok(())
}

// --- DB Setup ---
fn init_db() -> Connection {
    let conn = Connection::open("cards.db").expect("Failed to open DB");
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS cards (
            uid TEXT PRIMARY KEY,
            label TEXT,
            text TEXT
        )",
        [],
    );
    conn
}

// --- Main ---
#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder().finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let version = env!("CARGO_PKG_VERSION").to_string();
    let repo = option_env!("RFID_CYBERDECK_REPO")
        .unwrap_or("yourusername/rfid-cyberdeck-rust")
        .to_string();

    let state = AppState {
        db: Arc::new(Mutex::new(init_db())),
        rfid: Arc::new(Mutex::new(RfidReader::new())),
        version,
        repo,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/read", get(api_read))
        .route("/api/write", post(api_write))
        .route("/api/cards", get(api_cards))
        .route("/api/label", post(api_label))
        .route("/api/status", get(api_status))
        .route("/api/update", post(api_update))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind");

    let server = axum::serve(listener, app);

    tokio::select! {
        _ = server => {},
        _ = signal::ctrl_c() => {
            info!("Shutting down gracefully");
        }
    }
}
