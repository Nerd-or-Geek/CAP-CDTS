use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use parking_lot::{Mutex, RwLock};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    process::{Command, exit},
    sync::Arc,
    thread,
    time::Duration,
};
use tokio::signal;
use tracing::{info, warn};
use tower_http::cors::CorsLayer;

#[cfg(target_os = "linux")]
use rppal::gpio::{Gpio, Level};

#[cfg(target_os = "linux")]
use std::time::Instant;

// --- Embedded HTML ---
const INDEX_HTML: &str = include_str!("../static/index.html");

const BIN_NAME: &str = env!("CARGO_PKG_NAME");

// Mode switch wiring (BCM GPIO numbers):
// - GPIO27 (Pin 13) pulled to GND => READ mode
// - GPIO22 (Pin 15) pulled to GND => WRITE mode
// - neither pulled to GND => OFF
#[cfg(target_os = "linux")]
const MODE_READ_GPIO: u8 = 27;
#[cfg(target_os = "linux")]
const MODE_WRITE_GPIO: u8 = 22;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DeviceMode {
    Off,
    Read,
    Write,
}

// --- App State ---
#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    rfid: Arc<Mutex<RfidReader>>,
    version: String,
    repo: String,
    mode: Arc<RwLock<DeviceMode>>,
}

impl AppState {
    fn mode(&self) -> DeviceMode {
        *self.mode.read()
    }

    fn set_mode(&self, mode: DeviceMode) {
        *self.mode.write() = mode;
    }
}

#[cfg(target_os = "linux")]
struct GpioModeSwitch {
    read_pin: rppal::gpio::InputPin,
    write_pin: rppal::gpio::InputPin,
}

#[cfg(target_os = "linux")]
impl GpioModeSwitch {
    fn new() -> Result<Self, String> {
        let gpio = Gpio::new().map_err(|e| format!("Failed to access GPIO: {e}"))?;

        let read_pin = gpio
            .get(MODE_READ_GPIO)
            .map_err(|e| format!("Failed to access GPIO{MODE_READ_GPIO}: {e}"))?
            .into_input_pullup();

        let write_pin = gpio
            .get(MODE_WRITE_GPIO)
            .map_err(|e| format!("Failed to access GPIO{MODE_WRITE_GPIO}: {e}"))?
            .into_input_pullup();

        Ok(Self { read_pin, write_pin })
    }

    fn sample_mode(&self) -> DeviceMode {
        let read_active = self.read_pin.read() == Level::Low;
        let write_active = self.write_pin.read() == Level::Low;

        match (read_active, write_active) {
            (true, false) => DeviceMode::Read,
            (false, true) => DeviceMode::Write,
            (false, false) => DeviceMode::Off,
            // Should never happen with correct wiring; fail safe.
            (true, true) => DeviceMode::Off,
        }
    }
}

fn spawn_mode_switch_monitor(state: AppState) {
    thread::spawn(move || {
        #[cfg(target_os = "linux")]
        {
            let mode_switch = match GpioModeSwitch::new() {
                Ok(s) => s,
                Err(e) => {
                    warn!("GPIO mode switch disabled: {e}");
                    state.set_mode(DeviceMode::Off);
                    return;
                }
            };

            // Basic debounce: require a stable reading for MODE_DEBOUNCE_MS before committing.
            const MODE_DEBOUNCE_MS: u64 = 75;
            const POLL_MS: u64 = 20;

            let mut last_raw = mode_switch.sample_mode();
            let mut last_stable = last_raw;
            let mut last_change = Instant::now();

            state.set_mode(last_stable);
            info!("Initial mode: {:?}", last_stable);

            loop {
                let raw = mode_switch.sample_mode();
                if raw != last_raw {
                    last_raw = raw;
                    last_change = Instant::now();
                }

                if raw != last_stable && last_change.elapsed() >= Duration::from_millis(MODE_DEBOUNCE_MS) {
                    let prev = state.mode();
                    last_stable = raw;
                    state.set_mode(last_stable);
                    if prev != last_stable {
                        info!("Mode switch: {:?} -> {:?}", prev, last_stable);
                    }
                }

                thread::sleep(Duration::from_millis(POLL_MS));
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            // No physical mode switch outside Linux/Raspberry Pi.
            // Default to WRITE so the dev UI is usable.
            state.set_mode(DeviceMode::Write);
        }
    });
}

fn spawn_continuous_reader(state: AppState) {
    thread::spawn(move || {
        const READ_POLL_MS: u64 = 150;
        let mut last_seen: Option<(String, String)> = None;

        loop {
            if state.mode() == DeviceMode::Read {
                let read_result = {
                    let mut rfid = state.rfid.lock();
                    rfid.read()
                };

                match read_result {
                    Ok((uid, text)) => {
                        let changed = match &last_seen {
                            Some((last_uid, last_text)) => last_uid != &uid || last_text != &text,
                            None => true,
                        };

                        if changed {
                            last_seen = Some((uid.clone(), text.clone()));

                            // Keep DB in sync with latest read while preserving any custom label.
                            let db = state.db.lock();
                            let updated = db
                                .execute(
                                    "UPDATE cards SET text = ?2 WHERE uid = ?1",
                                    params![&uid, &text],
                                )
                                .unwrap_or(0);

                            if updated == 0 {
                                let _ = db.execute(
                                    "INSERT OR IGNORE INTO cards (uid, label, text) VALUES (?1, '', ?2)",
                                    params![&uid, &text],
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!("RFID read error: {e}");
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            } else {
                last_seen = None;
            }

            thread::sleep(Duration::from_millis(READ_POLL_MS));
        }
    });
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

    fn last(&self) -> Option<(String, String)> {
        match (&self.last_uid, &self.last_text) {
            (Some(uid), Some(text)) => Some((uid.clone(), text.clone())),
            _ => None,
        }
    }

    fn write(&mut self, text: &str) -> Result<String, String> {
        // TODO: Replace with real RC522 write logic
        let uid = "DEADBEEF".to_string();
        self.last_uid = Some(uid.clone());
        self.last_text = Some(text.to_string());
        Ok(uid)
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
    mode: DeviceMode,
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
    match state.mode() {
        DeviceMode::Off => (
            StatusCode::CONFLICT,
            Json(ReadResponse {
                uid: String::new(),
                text: "Device is OFF".to_string(),
                success: false,
            }),
        )
            .into_response(),
        DeviceMode::Write => (
            StatusCode::CONFLICT,
            Json(ReadResponse {
                uid: String::new(),
                text: "Device is in WRITE mode".to_string(),
                success: false,
            }),
        )
            .into_response(),
        DeviceMode::Read => {
            let rfid = state.rfid.lock();
            match rfid.last() {
                Some((uid, text)) => Json(ReadResponse {
                    uid,
                    text,
                    success: true,
                })
                .into_response(),
                None => Json(ReadResponse {
                    uid: String::new(),
                    text: "Waiting for tag...".to_string(),
                    success: false,
                })
                .into_response(),
            }
        }
    }
}

async fn api_write(
    State(state): State<AppState>,
    Json(payload): Json<WriteRequest>,
) -> impl IntoResponse {
    if state.mode() != DeviceMode::Write {
        return (
            StatusCode::CONFLICT,
            Json(ReadResponse {
                uid: String::new(),
                text: "Writes are only allowed in WRITE mode".to_string(),
                success: false,
            }),
        )
            .into_response();
    }

    let mut rfid = state.rfid.lock();
    match rfid.write(&payload.text) {
        Ok(uid) => {
            let db = state.db.lock();
            let _ = db.execute(
                "INSERT OR IGNORE INTO cards (uid, label, text) VALUES (?1, '', ?2)",
                params![&uid, &payload.text],
            );

            // Keep DB text updated while preserving any label.
            let _ = db.execute(
                "UPDATE cards SET text = ?2 WHERE uid = ?1",
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
        mode: state.mode(),
        status: "ok".to_string(),
    })
}

async fn api_update(State(state): State<AppState>) -> impl IntoResponse {
    let repo = state.repo.clone();
    thread::spawn(move || {
        if let Err(e) = do_self_update(&repo) {
            eprintln!("Update failed: {e}");
        }
    });
    StatusCode::ACCEPTED
}

// --- Self-Updater ---
fn do_self_update(repo: &str) -> Result<(), String> {
    let parts: Vec<&str> = repo.split('/').collect();
    let owner = parts.get(0).copied().unwrap_or("Nerd-or-Geek");
    let name = parts.get(1).copied().unwrap_or("CAP-CDTS");

    info!("Starting background update for {}/{}", owner, name);

    // Fast path on Raspberry Pi: download the prebuilt release binary if it exists.
    // This avoids compiling 200+ crates on a Pi Zero.
    #[cfg(target_os = "linux")]
    {
        match try_update_from_github_release(owner, name) {
            Ok(()) => return Ok(()),
            Err(e) => warn!("Release update unavailable, falling back to source build: {e}"),
        }
    }

    let repo_url = format!("https://github.com/{}/{}.git", owner, name);
    perform_background_update(&repo_url)
}

#[cfg(target_os = "linux")]
fn try_update_from_github_release(owner: &str, repo: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current executable: {e}"))?;

    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "Failed to determine executable directory".to_string())
        ?
        .to_path_buf();

    let url = format!(
        "https://github.com/{}/{}/releases/latest/download/{}",
        owner, repo, BIN_NAME
    );

    let download_tmp = std::env::temp_dir().join(format!("{BIN_NAME}.download"));
    info!("Attempting release download: {url}");

    download_file(&url, &download_tmp)?;

    // Ensure executable bit is set.
    let mut perms = fs::metadata(&download_tmp)
        .map_err(|e| format!("Failed to stat downloaded binary: {e}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&download_tmp, perms)
        .map_err(|e| format!("Failed to set permissions on downloaded binary: {e}"))?;

    // Stage next to the current binary, then atomically rename into place.
    let staged = exe_dir.join(format!("{BIN_NAME}.new"));
    fs::copy(&download_tmp, &staged)
        .map_err(|e| format!("Failed to stage new binary: {e}"))?;

    fs::rename(&staged, &current_exe)
        .map_err(|e| format!("Failed to replace running binary: {e}"))?;

    info!("Updated from GitHub Release. Restarting...");
    let _ = Command::new(&current_exe)
        .args(std::env::args().skip(1))
        .spawn();
    exit(0);
}

#[cfg(target_os = "linux")]
fn download_file(url: &str, dest: &PathBuf) -> Result<(), String> {
    // Prefer curl, fall back to wget.
    let dest_str = dest.to_string_lossy().to_string();
    match Command::new("curl")
        .args(["-L", "-f", "-sS", "-o", &dest_str, url])
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let out = Command::new("wget")
                .args(["-q", "-O", &dest_str, url])
                .output()
                .map_err(|e| format!("Failed to run wget: {e}"))?;

            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "wget failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        }
        Err(e) => Err(format!("Failed to run curl: {e}")),
    }
}

fn updater_root_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CAP_CDTS_UPDATER_DIR") {
        return PathBuf::from(dir);
    }

    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("cap-cdts-updater");
    }

    std::env::temp_dir().join("cap-cdts-updater")
}

fn python_rebuild_script_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("tools").join("rebuild.py"));
        candidates.push(cwd.join("rebuild.py"));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // Common layouts:
            // - repo_root/target/release/<bin>
            // - repo_root/<bin>
            candidates.push(exe_dir.join("tools").join("rebuild.py"));
            candidates.push(exe_dir.join("rebuild.py"));

            // Try to walk up a few levels (target/release -> repo root)
            let mut p = exe_dir.to_path_buf();
            for _ in 0..4 {
                if let Some(parent) = p.parent() {
                    candidates.push(parent.join("tools").join("rebuild.py"));
                    p = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }
    }

    candidates.into_iter().find(|p| p.exists())
}

fn try_python_source_build(repo_url: &str, root: &PathBuf, built_exe_name: &str) -> Result<PathBuf, String> {
    let script = python_rebuild_script_path()
        .ok_or_else(|| "Python rebuild script not found (expected tools/rebuild.py)".to_string())?;

    let root_str = root.to_string_lossy().to_string();

    // Try python3 then python.
    for py in ["python3", "python"] {
        let out = Command::new(py)
            .arg(&script)
            .arg("--repo-url")
            .arg(repo_url)
            .arg("--updater-root")
            .arg(&root_str)
            .arg("--bin-name")
            .arg(built_exe_name)
            .output();

        match out {
            Ok(out) => {
                if !out.status.success() {
                    return Err(format!(
                        "Python rebuild failed ({py}): {}",
                        String::from_utf8_lossy(&out.stderr)
                    ));
                }

                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if path.is_empty() {
                    return Err("Python rebuild produced no artifact path".to_string());
                }

                let artifact = PathBuf::from(path);
                if !artifact.exists() {
                    return Err(format!("Python rebuild artifact does not exist: {artifact:?}"));
                }

                return Ok(artifact);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Failed to launch {py}: {e}")),
        }
    }

    Err("Python executable not found (install python3)".to_string())
}

fn build_from_source_rust(repo_url: &str, root: &PathBuf, built_exe_name: &str) -> Result<PathBuf, String> {
    let clone_dir = root.join("src");
    let target_dir = root.join("cargo-target");

    // Fresh clone each time, but keep the Cargo target dir cached so builds are fast.
    let _ = fs::remove_dir_all(&clone_dir);

    info!("Cloning repository to {:?}", clone_dir);

    let clone_output = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch")
        .arg(repo_url)
        .arg(&clone_dir)
        .output()
        .map_err(|e| format!("Failed to run git clone: {e}"))?;

    if !clone_output.status.success() {
        return Err(format!(
            "Git clone failed: {}",
            String::from_utf8_lossy(&clone_output.stderr)
        ));
    }

    info!("Building release binary (cached target at {:?})...", target_dir);

    let has_lock = clone_dir.join("Cargo.lock").exists();

    let mut build_cmd = Command::new("cargo");
    build_cmd
        .env("CARGO_TARGET_DIR", &target_dir)
        .arg("build")
        .arg("--release")
        .current_dir(&clone_dir);

    if has_lock {
        build_cmd.arg("--locked");
    }

    let build_output = build_cmd
        .output()
        .map_err(|e| format!("Failed to run cargo build: {e}"))?;

    if !build_output.status.success() {
        return Err(format!(
            "Cargo build failed: {}",
            String::from_utf8_lossy(&build_output.stderr)
        ));
    }

    let new_binary = target_dir.join("release").join(built_exe_name);
    if !new_binary.exists() {
        return Err(format!("Built binary not found at {:?}", new_binary));
    }

    Ok(new_binary)
}

fn perform_background_update(repo_url: &str) -> Result<(), String> {
    let root = updater_root_dir();
    fs::create_dir_all(&root)
        .map_err(|e| format!("Failed to create updater dir {root:?}: {e}"))?;

    info!("Preparing update build...");

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current executable: {e}"))?;

    let built_exe_name = if cfg!(windows) {
        format!("{BIN_NAME}.exe")
    } else {
        BIN_NAME.to_string()
    };

    // Prefer Python rebuild on Linux to build in a fresh folder, but fall back to internal build.
    let new_binary = if cfg!(target_os = "linux") {
        match try_python_source_build(repo_url, &root, &built_exe_name) {
            Ok(p) => p,
            Err(e) => {
                warn!("Python rebuild unavailable, falling back to internal build: {e}");
                build_from_source_rust(repo_url, &root, &built_exe_name)?
            }
        }
    } else {
        build_from_source_rust(repo_url, &root, &built_exe_name)?
    };

    info!("Build completed successfully. Preparing to switch binaries...");

    info!("New binary ready at {:?}", new_binary);
    info!("Swapping binaries and restarting...");

    if cfg!(windows) {
        // Windows: Create a batch script to replace the binary after this process exits.
        let batch_path = std::env::temp_dir().join("cap_cdts_update.bat");
        let batch_content = format!(
            "@echo off\r\n\
REM Wait for process to exit\r\n\
timeout /t 2 /nobreak >NUL\r\n\
REM Replace binary\r\n\
copy /Y \"{}\" \"{}\" >NUL\r\n\
REM Restart\r\n\
start \"\" \"{}\"\r\n",
            new_binary.display(),
            current_exe.display(),
            current_exe.display()
        );

        fs::write(&batch_path, batch_content)
            .map_err(|e| format!("Failed to create update script: {e}"))?;

        let batch_path_str = batch_path.to_string_lossy().to_string();
        let _ = Command::new("cmd")
            .args(["/C", "start", "", &batch_path_str])
            .spawn();

        exit(0);
    } else {
        // Unix: stage the new binary next to the current binary and atomically rename it in.
        let exe_dir = current_exe
            .parent()
            .ok_or_else(|| "Failed to determine executable directory".to_string())?
            .to_path_buf();

        let current_file = current_exe
            .file_name()
            .ok_or_else(|| "Failed to determine executable filename".to_string())?
            .to_string_lossy()
            .to_string();

        let staged = exe_dir.join(format!("{current_file}.new"));
        fs::copy(&new_binary, &staged)
            .map_err(|e| format!("Failed to stage new binary: {e}"))?;

        fs::rename(&staged, &current_exe)
            .map_err(|e| format!("Failed to replace running binary: {e}"))?;

        // Restart (systemd will also restart us, but spawning is convenient for non-systemd runs).
        let _ = Command::new(&current_exe)
            .args(std::env::args().skip(1))
            .spawn();

        exit(0);
    }
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
        .unwrap_or("Nerd-or-Geek/CAP-CDTS")
        .to_string();

    let state = AppState {
        db: Arc::new(Mutex::new(init_db())),
        rfid: Arc::new(Mutex::new(RfidReader::new())),
        version,
        repo,
        mode: Arc::new(RwLock::new(DeviceMode::Off)),
    };

    // Background hardware tasks (mode switch + continuous read).
    spawn_mode_switch_monitor(state.clone());
    spawn_continuous_reader(state.clone());

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
