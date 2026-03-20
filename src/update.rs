use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};

#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: UpdateConfig,
    status: RwLock<UpdateStatus>,
    logs: Mutex<std::collections::VecDeque<String>>,
    running: Mutex<bool>,
}

#[derive(Clone, Debug)]
pub struct UpdateConfig {
    pub enabled: bool,
    pub repo_dir: PathBuf,

    /// Where to persist the last update status (optional but recommended for Pi deployments).
    pub state_path: PathBuf,

    /// Path to the staged binary produced by `make build`.
    pub new_bin: PathBuf,

    /// Path to the "live" binary. If not set, the updater will use `std::env::current_exe()`.
    pub live_bin: Option<PathBuf>,

    /// If true, the updater will exit the process after swapping binaries.
    /// Use this with a supervisor (systemd `Restart=always`) to restart into the new build.
    pub auto_restart: bool,

    /// How many log lines to keep in-memory and persist.
    pub max_log_lines: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Running,
    Success,
    Error,
    Restarting,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub enabled: bool,
    pub state: UpdateState,
    pub step: Option<String>,
    pub started_at_utc: Option<String>,
    pub finished_at_utc: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatusResponse {
    #[serde(flatten)]
    pub status: UpdateStatus,
    pub log_tail: Vec<String>,
    pub repo_dir: String,
    pub live_bin: String,
    pub new_bin: String,
}

#[derive(Debug)]
pub enum StartUpdateError {
    Disabled,
    AlreadyRunning,
}

impl std::fmt::Display for StartUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartUpdateError::Disabled => write!(f, "update feature is disabled"),
            StartUpdateError::AlreadyRunning => write!(f, "an update is already running"),
        }
    }
}

impl std::error::Error for StartUpdateError {}

impl Updater {
    pub async fn open(cfg: UpdateConfig) -> anyhow::Result<Self> {
        let mut status = UpdateStatus {
            enabled: cfg.enabled,
            state: UpdateState::Idle,
            step: None,
            started_at_utc: None,
            finished_at_utc: None,
            error: None,
        };

        // Best-effort load of prior status (helps after an auto-restart).
        if let Ok(raw) = tokio::fs::read_to_string(&cfg.state_path).await {
            if let Ok(prev) = serde_json::from_str::<UpdateStatus>(&raw) {
                status = prev;
                // Normalize state after a restart.
                // - If we were "running", the update was interrupted.
                // - If we were "restarting" and have a finished timestamp, treat it as success.
                match status.state {
                    UpdateState::Running => {
                        status.state = UpdateState::Error;
                        status.step = None;
                        status.finished_at_utc = Some(now_rfc3339());
                        status.error = Some(
                            "previous update was interrupted by a restart; current process is running"
                                .to_string(),
                        );
                    }
                    UpdateState::Restarting => {
                        if status.finished_at_utc.is_some() {
                            status.state = UpdateState::Success;
                            status.step = Some("restarted".to_string());
                            status.error = None;
                        } else {
                            status.state = UpdateState::Error;
                            status.step = None;
                            status.finished_at_utc = Some(now_rfc3339());
                            status.error = Some(
                                "previous update restarted before recording completion"
                                    .to_string(),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        let inner = Inner {
            cfg,
            status: RwLock::new(status),
            logs: Mutex::new(std::collections::VecDeque::new()),
            running: Mutex::new(false),
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub async fn status(&self) -> UpdateStatusResponse {
        let status = { self.inner.status.read().await.clone() };
        let log_tail = {
            let logs = self.inner.logs.lock().await;
            logs.iter().cloned().collect::<Vec<_>>()
        };

        let live_bin = match &self.inner.cfg.live_bin {
            Some(p) => p.clone(),
            None => std::env::current_exe().unwrap_or_else(|_| PathBuf::from("<unknown>")),
        };

        UpdateStatusResponse {
            status,
            log_tail,
            repo_dir: self.inner.cfg.repo_dir.display().to_string(),
            live_bin: live_bin.display().to_string(),
            new_bin: self.inner.cfg.new_bin.display().to_string(),
        }
    }

    pub async fn start(&self) -> Result<(), StartUpdateError> {
        if !self.inner.cfg.enabled {
            return Err(StartUpdateError::Disabled);
        }

        let mut running = self.inner.running.lock().await;
        if *running {
            return Err(StartUpdateError::AlreadyRunning);
        }
        *running = true;

        // Reset logs for the run.
        {
            let mut logs = self.inner.logs.lock().await;
            logs.clear();
        }

        let now = now_rfc3339();
        {
            let mut s = self.inner.status.write().await;
            s.enabled = self.inner.cfg.enabled;
            s.state = UpdateState::Running;
            s.step = Some("starting".to_string());
            s.started_at_utc = Some(now);
            s.finished_at_utc = None;
            s.error = None;
        }
        let _ = self.persist_status().await;

        let this = self.clone();
        tokio::spawn(async move {
            let result = this.run_update().await;
            if let Err(e) = result {
                this.set_error(format!("{e:#}"))
                    .await
                    .unwrap_or_else(|_| ());
            }

            let mut running = this.inner.running.lock().await;
            *running = false;
        });

        Ok(())
    }

    async fn run_update(&self) -> anyhow::Result<()> {
        self.set_step("git pull").await?;
        self.run_command("git", &["pull", "--ff-only"]).await?;

        self.set_step("make build").await?;
        self.run_command("make", &["build"]).await?;

        self.set_step("swap binaries").await?;
        self.swap_binaries().await?;

        {
            let mut s = self.inner.status.write().await;
            s.state = UpdateState::Success;
            s.step = Some("completed".to_string());
            s.finished_at_utc = Some(now_rfc3339());
            s.error = None;
        }
        self.persist_status().await?;

        if self.inner.cfg.auto_restart {
            // On Windows, in-place swapping is not supported; don't hard-exit by default.
            // The Pi deployment target is Linux and *should* use systemd Restart=always.
            if cfg!(windows) {
                self.log_line(
                    "auto-restart is enabled, but Windows cannot swap the running .exe; restart manually"
                        .to_string(),
                )
                .await;
                return Ok(());
            }

            {
                let mut s = self.inner.status.write().await;
                s.state = UpdateState::Restarting;
                s.step = Some("restarting".to_string());
            }
            self.persist_status().await?;

            // Give the UI a moment to fetch the "restarting" status.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Exit cleanly; systemd should bring us back up.
            std::process::exit(0);
        }

        Ok(())
    }

    async fn run_command(&self, program: &str, args: &[&str]) -> anyhow::Result<()> {
        self.log_line(format!("$ {program} {}", args.join(" "))).await;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.inner.cfg.repo_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {program}"))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let this = self.clone();
        let stdout_task = match stdout {
            Some(out) => Some(tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    this.log_line(line).await;
                }
            })),
            None => None,
        };

        let this = self.clone();
        let stderr_task = match stderr {
            Some(err) => Some(tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    this.log_line(line).await;
                }
            })),
            None => None,
        };

        let status = child.wait().await.context("wait on child process")?;

        if let Some(t) = stdout_task {
            let _ = t.await;
        }
        if let Some(t) = stderr_task {
            let _ = t.await;
        }

        if !status.success() {
            anyhow::bail!("{program} failed with exit code {code:?}", code = status.code());
        }

        Ok(())
    }

    async fn swap_binaries(&self) -> anyhow::Result<()> {
        let repo_dir = &self.inner.cfg.repo_dir;

        let new_bin = absolutize(repo_dir, &self.inner.cfg.new_bin);

        if !tokio::fs::try_exists(&new_bin).await.unwrap_or(false) {
            anyhow::bail!("staged binary does not exist: {}", new_bin.display());
        }

        #[cfg(windows)]
        {
            // Windows cannot replace a running executable. Keep the staged binary and let the
            // operator restart manually (or run behind a supervisor that can restart and swap).
            anyhow::bail!(
                "binary swap is not supported on Windows; staged build is at {}",
                new_bin.display()
            );
        }

        #[cfg(not(windows))]
        {
            let live_bin = match &self.inner.cfg.live_bin {
                Some(p) => absolutize(repo_dir, p),
                None => std::env::current_exe().context("determine current exe path")?,
            };

            let ts = time::OffsetDateTime::now_utc().unix_timestamp();
            let backup = with_suffix(&live_bin, &format!(".old-{ts}"));

            // If a live binary exists, move it out of the way first.
            if tokio::fs::try_exists(&live_bin).await.unwrap_or(false) {
                tokio::fs::rename(&live_bin, &backup)
                    .await
                    .with_context(|| {
                        format!(
                            "rename live binary {} -> {}",
                            live_bin.display(),
                            backup.display()
                        )
                    })?;
            }

            tokio::fs::rename(&new_bin, &live_bin)
                .await
                .with_context(|| {
                    format!(
                        "rename staged binary {} -> {}",
                        new_bin.display(),
                        live_bin.display()
                    )
                })?;

            self.log_line(format!(
                "swapped binaries: {} is now live (backup: {})",
                live_bin.display(),
                backup.display()
            ))
            .await;

            Ok(())
        }
    }

    async fn set_step(&self, step: &str) -> anyhow::Result<()> {
        {
            let mut s = self.inner.status.write().await;
            s.step = Some(step.to_string());
        }
        self.persist_status().await?;
        Ok(())
    }

    async fn set_error(&self, msg: String) -> anyhow::Result<()> {
        {
            let mut s = self.inner.status.write().await;
            s.state = UpdateState::Error;
            s.step = Some("error".to_string());
            s.finished_at_utc = Some(now_rfc3339());
            s.error = Some(msg);
        }
        self.persist_status().await?;
        Ok(())
    }

    async fn log_line(&self, line: String) {
        let mut logs = self.inner.logs.lock().await;
        logs.push_back(line);
        while logs.len() > self.inner.cfg.max_log_lines {
            logs.pop_front();
        }
    }

    async fn persist_status(&self) -> anyhow::Result<()> {
        let status = { self.inner.status.read().await.clone() };

        if let Some(parent) = self.inner.cfg.state_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "create update state directory {}",
                    parent.display()
                )
            })?;
        }

        let json = serde_json::to_string_pretty(&status).context("serialize update status")?;
        write_atomic_json(&self.inner.cfg.state_path, json).await?;
        Ok(())
    }
}

fn absolutize(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

#[cfg(not(windows))]
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let file = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "cap-cdts-backend".to_string());

    path.with_file_name(format!("{file}{suffix}"))
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

async fn write_atomic_json(path: &Path, contents: String) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");

    tokio::fs::write(&tmp, contents)
        .await
        .with_context(|| format!("write temp update state file: {}", tmp.display()))?;

    // Windows rename fails if the destination exists.
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(path).await;
    }

    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename temp update state into place: {}", path.display()))?;

    Ok(())
}
