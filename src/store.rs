use std::path::{Path, PathBuf};

use anyhow::Context;
use tokio::sync::{watch, RwLock};

use crate::models::{
    normalize_level, AuthLive, CreateReportRequest, CreateUserRequest, GpioConfig, LiveCounts, LiveState,
    ReportRecord, StoreData, UpdateReportRequest, UpdateUserRequest, UserPublic, UserRecord, LEVEL_BASIC,
};

#[derive(Debug, Clone, Copy)]
pub enum StoreErrorKind {
    BadRequest,
    Conflict,
    Internal,
}

#[derive(Debug)]
pub struct StoreError {
    pub kind: StoreErrorKind,
    pub message: String,
}

impl StoreError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::BadRequest,
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::Conflict,
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::Internal,
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StoreError {}

pub struct JsonStore {
    path: PathBuf,
    data: RwLock<StoreData>,
    live_tx: watch::Sender<LiveState>,
}

impl JsonStore {
    pub async fn open(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let parent = path.parent().unwrap_or(Path::new("."));
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create store directory: {}", parent.display()))?;

        let (data, should_persist) = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => match serde_json::from_str::<StoreData>(&raw) {
                Ok(mut d) => {
                    normalize_store(&mut d);
                    (d, false)
                }
                Err(e) => {
                    // Keep a best-effort backup of the corrupted file.
                    let bak = path.with_extension("json.bak");
                    let _ = tokio::fs::write(&bak, raw).await;
                    tracing::warn!(error = %e, backup = %bak.display(), "store.json was invalid; backed up and reinitialized");
                    (StoreData::default(), true)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (StoreData::default(), true),
            Err(e) => return Err(e).with_context(|| format!("read store file: {}", path.display())),
        };

        let live = compute_live_state(&data);
        let (live_tx, _rx) = watch::channel(live);

        let store = Self {
            path,
            data: RwLock::new(data),
            live_tx,
        };

        if should_persist {
            store.persist().await?;
        }

        Ok(store)
    }

    pub fn subscribe_live(&self) -> watch::Receiver<LiveState> {
        self.live_tx.subscribe()
    }

    pub async fn get_gpio_config(&self) -> GpioConfig {
        let data = self.data.read().await;
        data.gpio_config.clone()
    }

    pub async fn set_gpio_config(&self, cfg: GpioConfig) -> Result<GpioConfig, StoreError> {
        validate_gpio_config(&cfg)?;

        let mut data = self.data.write().await;
        data.gpio_config = cfg.clone();

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);
        Ok(cfg)
    }

    pub async fn persist(&self) -> anyhow::Result<()> {
        let data = self.data.read().await;
        let json = serde_json::to_string_pretty(&*data).context("serialize store")?;
        drop(data);

        write_atomic(&self.path, json)
            .await
            .with_context(|| format!("persist store to {}", self.path.display()))?;
        Ok(())
    }

    pub async fn list_users(&self) -> Vec<UserPublic> {
        let data = self.data.read().await;
        let mut out: Vec<UserPublic> = data.users.iter().map(UserPublic::from).collect();
        out.sort_by(|a, b| a.username.to_lowercase().cmp(&b.username.to_lowercase()));
        out
    }

    pub async fn get_user_record(&self, username: &str) -> Option<UserRecord> {
        let data = self.data.read().await;
        data.users
            .iter()
            .find(|u| u.username.eq_ignore_ascii_case(username.trim()))
            .cloned()
    }

    pub async fn create_user(&self, req: CreateUserRequest) -> Result<UserPublic, StoreError> {
        let username = req.username.trim();
        if username.is_empty() {
            return Err(StoreError::bad_request("username is required"));
        }

        // Validate level. Legacy level=0 is accepted and normalized.
        match req.level {
            0 | 1 | 2 | 3 | 4 | 5 => {}
            _ => {
                return Err(StoreError::bad_request(
                    "level must be 1 (admin), 2 (junior admin), 3 (moderator), 4 (advanced user), or 5 (basic)",
                ))
            }
        }

        let level = normalize_level(req.level);

        let passcode_hash = if level == LEVEL_BASIC {
            // Basic users are name-only; they should not have a passcode.
            None
        } else {
            match req.passcode.as_deref() {
                None => {
                    return Err(StoreError::bad_request(
                        "passcode is required for non-basic users (exactly 5 digits)",
                    ))
                }
                Some(pc) if pc.trim().is_empty() => {
                    return Err(StoreError::bad_request(
                        "passcode is required for non-basic users (exactly 5 digits)",
                    ))
                }
                Some(pc) => {
                    let pc = pc.trim();
                    if !is_five_digit_passcode(pc) {
                        return Err(StoreError::bad_request("passcode must be exactly 5 digits"));
                    }

                    Some(
                        hash_passcode(pc).map_err(|e| {
                            StoreError::internal(format!("failed to hash passcode: {e}"))
                        })?,
                    )
                }
            }
        };

        let mut data = self.data.write().await;

        if data
            .users
            .iter()
            .any(|u| u.username.eq_ignore_ascii_case(username))
        {
            return Err(StoreError::conflict("username already exists"));
        }

        let user = UserRecord {
            username: username.to_string(),
            rfid_uid: req.rfid_uid.trim().to_string(),
            level,
            passcode_hash,
            created_at_utc: now_rfc3339(),
        };

        data.users.push(user.clone());

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);

        Ok(UserPublic::from(&user))
    }

    pub async fn update_user(
        &self,
        username: &str,
        req: UpdateUserRequest,
    ) -> Result<Option<UserPublic>, StoreError> {
        let username = username.trim();
        if username.is_empty() {
            return Err(StoreError::bad_request("username is required"));
        }

        // Validate level (if provided).
        if let Some(lvl) = req.level {
            match lvl {
                0 | 1 | 2 | 3 | 4 | 5 => {}
                _ => {
                    return Err(StoreError::bad_request(
                        "level must be 1 (admin), 2 (junior admin), 3 (moderator), 4 (advanced user), or 5 (basic)",
                    ))
                }
            }
        }

        let mut data = self.data.write().await;
        let Some(u) = data
            .users
            .iter_mut()
            .find(|u| u.username.eq_ignore_ascii_case(username))
        else {
            return Ok(None);
        };

        if let Some(rfid) = req.rfid_uid {
            u.rfid_uid = rfid.trim().to_string();
        }

        if let Some(lvl) = req.level {
            u.level = normalize_level(lvl);
        }

        // Passcode update logic.
        // - If empty string => clear passcode
        // - If set => must be 5 digits
        // - If target level is BASIC => passcode is always cleared
        if u.level == LEVEL_BASIC {
            u.passcode_hash = None;
        } else if let Some(pc) = req.passcode {
            let pc = pc.trim().to_string();
            if pc.is_empty() {
                u.passcode_hash = None;
            } else {
                if !is_five_digit_passcode(&pc) {
                    return Err(StoreError::bad_request("passcode must be exactly 5 digits"));
                }
                u.passcode_hash = Some(
                    hash_passcode(&pc)
                        .map_err(|e| StoreError::internal(format!("failed to hash passcode: {e}")))?,
                );
            }
        }

        let updated = UserPublic::from(&*u);

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);
        Ok(Some(updated))
    }

    pub async fn list_reports(&self) -> Vec<ReportRecord> {
        let data = self.data.read().await;
        let mut out = data.reports.clone();
        out.sort_by(|a, b| b.num.cmp(&a.num));
        out
    }

    pub async fn create_report(
        &self,
        actor_username: &str,
        actor_level: i32,
        req: CreateReportRequest,
    ) -> Result<ReportRecord, StoreError> {
        if req.person.trim().is_empty() {
            return Err(StoreError::bad_request("person is required"));
        }
        if req.title.trim().is_empty() {
            return Err(StoreError::bad_request("title is required"));
        }
        if req.description.trim().is_empty() {
            return Err(StoreError::bad_request("description is required"));
        }

        let mut data = self.data.write().await;

        let num = allocate_report_num(&mut data);
        let now = now_rfc3339();

        let report = ReportRecord {
            num,
            created_at_utc: now,
            opened_by: actor_username.trim().to_string(),
            opened_by_level: Some(actor_level),
            closed_by: None,
            closed_at_utc: None,
            closing_comments: None,
            person: req.person.trim().to_string(),
            title: req.title.trim().to_string(),
            category: req.category.trim().to_string(),
            priority: req.priority.trim().to_string(),
            description: req.description.trim().to_string(),
        };

        data.reports.push(report.clone());

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);

        Ok(report)
    }

    pub async fn update_report(
        &self,
        num: u32,
        req: UpdateReportRequest,
        actor_username: &str,
    ) -> Result<Option<ReportRecord>, StoreError> {
        let mut data = self.data.write().await;
        let Some(r) = data.reports.iter_mut().find(|r| r.num == num) else {
            return Ok(None);
        };

        // Editable fields
        if let Some(v) = req.person {
            let v = v.trim();
            if v.is_empty() {
                return Err(StoreError::bad_request("person is required"));
            }
            r.person = v.to_string();
        }

        if let Some(v) = req.title {
            let v = v.trim();
            if v.is_empty() {
                return Err(StoreError::bad_request("title is required"));
            }
            r.title = v.to_string();
        }

        if let Some(v) = req.category {
            r.category = v.trim().to_string();
        }

        if let Some(v) = req.priority {
            r.priority = v.trim().to_string();
        }

        if let Some(v) = req.description {
            let v = v.trim();
            if v.is_empty() {
                return Err(StoreError::bad_request("description is required"));
            }
            r.description = v.to_string();
        }

        // Close/reopen
        if let Some(closed) = req.closed {
            if closed {
                r.closed_by = Some(actor_username.trim().to_string());
                r.closed_at_utc = Some(now_rfc3339());
                let cc = req.closing_comments.map(|s| s.trim().to_string());
                r.closing_comments = cc.and_then(|s| if s.is_empty() { None } else { Some(s) });
            } else {
                r.closed_by = None;
                r.closed_at_utc = None;
                r.closing_comments = None;
            }
        }

        let updated = r.clone();

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);
        Ok(Some(updated))
    }

    pub async fn get_report(&self, num: u32) -> Option<ReportRecord> {
        let data = self.data.read().await;
        data.reports.iter().find(|r| r.num == num).cloned()
    }

    pub async fn delete_report(&self, num: u32) -> Result<bool, StoreError> {
        let mut data = self.data.write().await;
        let before = data.reports.len();
        data.reports.retain(|r| r.num != num);
        let deleted = data.reports.len() != before;

        if !deleted {
            return Ok(false);
        }

        let live = compute_live_state(&data);
        if let Err(e) = self.persist_locked(&data).await {
            return Err(StoreError::internal(format!("failed to persist store: {e}")));
        }
        drop(data);

        let _ = self.live_tx.send(live);
        Ok(true)
    }

    async fn persist_locked(&self, data: &StoreData) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(data).context("serialize store")?;
        write_atomic(&self.path, json)
            .await
            .with_context(|| format!("persist store to {}", self.path.display()))?;
        Ok(())
    }
}

fn normalize_store(d: &mut StoreData) {
    if d.schema_version == 0 {
        d.schema_version = 1;
    }

    if d.next_report_num < 100_000 || d.next_report_num > 999_999 {
        d.next_report_num = 100_000;
    }

    // Normalize user levels for backward compatibility.
    for u in &mut d.users {
        u.level = normalize_level(u.level);
    }
}

fn allocate_report_num(d: &mut StoreData) -> u32 {
    normalize_store(d);

    // Keep it 6-digit.
    let mut candidate = d.next_report_num;

    // Worst-case (very full): try every value once.
    for _ in 0..900_000 {
        let exists = d.reports.iter().any(|r| r.num == candidate);
        if !exists {
            // Advance the counter for the next allocation.
            let mut next = candidate + 1;
            if next > 999_999 {
                next = 100_000;
            }
            d.next_report_num = next;
            return candidate;
        }

        candidate += 1;
        if candidate > 999_999 {
            candidate = 100_000;
        }
    }

    // If we somehow exhaust all numbers, wrap and reuse.
    100_000
}

fn compute_live_state(d: &StoreData) -> LiveState {
    let open_reports = d.reports.iter().filter(|r| r.closed_at_utc.is_none()).count();

    LiveState {
        last_update_utc: now_rfc3339(),
        counts: LiveCounts {
            users: d.users.len(),
            reports: d.reports.len(),
            open_reports,
        },
        auth: AuthLive {
            stage: "None".to_string(),
            user: None,
        },
        gpio_config: d.gpio_config.clone(),
    }
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn is_five_digit_passcode(s: &str) -> bool {
    if s.len() != 5 {
        return false;
    }
    s.chars().all(|c| c.is_ascii_digit())
}

fn hash_passcode(passcode: &str) -> anyhow::Result<String> {
    use argon2::{Argon2, PasswordHasher};
    use password_hash::SaltString;
    use rand_core::OsRng;

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(passcode.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(hash.to_string())
}

async fn write_atomic(path: &Path, contents: String) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");

    // Ensure parent exists, in case the user passed a custom path.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create store directory: {}", parent.display()))?;
    }

    tokio::fs::write(&tmp, contents)
        .await
        .with_context(|| format!("write temp store file: {}", tmp.display()))?;

    // Windows rename fails if the destination exists.
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(path).await;
    }

    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename temp store file into place: {}", path.display()))?;

    Ok(())
}

fn validate_gpio_config(cfg: &GpioConfig) -> Result<(), StoreError> {
    // Allow only header-usable BCM GPIOs by default.
    // If you need additional pins (e.g., GPIO 28-31 on some models), we can extend this.
    fn validate_pin(name: &str, pin: Option<u8>) -> Result<(), StoreError> {
        if let Some(p) = pin {
            if p > 27 {
                return Err(StoreError::bad_request(format!(
                    "{name} must be a BCM GPIO number 0-27 (got {p})"
                )));
            }
        }
        Ok(())
    }

    validate_pin("rfid_rst_gpio", cfg.rfid_rst_gpio)?;
    validate_pin("spdt1_a", cfg.spdt1_a)?;
    validate_pin("spdt1_b", cfg.spdt1_b)?;
    validate_pin("spdt2_a", cfg.spdt2_a)?;
    validate_pin("spdt2_b", cfg.spdt2_b)?;
    validate_pin("spdt3_a", cfg.spdt3_a)?;
    validate_pin("spdt3_b", cfg.spdt3_b)?;
    validate_pin("rotary_a", cfg.rotary_a)?;
    validate_pin("rotary_b", cfg.rotary_b)?;
    validate_pin("rotary_btn", cfg.rotary_btn)?;
    validate_pin("cap_touch", cfg.cap_touch)?;
    validate_pin("push_btn", cfg.push_btn)?;

    if let Some(bus) = cfg.rfid_spi_bus {
        if bus > 1 {
            return Err(StoreError::bad_request(
                "rfid_spi_bus must be 0 or 1 (typical Raspberry Pi uses 0)",
            ));
        }
    }

    if let Some(cs) = cfg.rfid_spi_cs {
        if cs > 1 {
            return Err(StoreError::bad_request(
                "rfid_spi_cs must be 0 (CE0) or 1 (CE1)",
            ));
        }
    }

    // Detect duplicate GPIO usage.
    use std::collections::HashMap;
    let mut seen: HashMap<u8, &'static str> = HashMap::new();
    let pairs: Vec<(&'static str, Option<u8>)> = vec![
        ("rfid_rst_gpio", cfg.rfid_rst_gpio),
        ("spdt1_a", cfg.spdt1_a),
        ("spdt1_b", cfg.spdt1_b),
        ("spdt2_a", cfg.spdt2_a),
        ("spdt2_b", cfg.spdt2_b),
        ("spdt3_a", cfg.spdt3_a),
        ("spdt3_b", cfg.spdt3_b),
        ("rotary_a", cfg.rotary_a),
        ("rotary_b", cfg.rotary_b),
        ("rotary_btn", cfg.rotary_btn),
        ("cap_touch", cfg.cap_touch),
        ("push_btn", cfg.push_btn),
    ];

    for (name, pin) in pairs {
        if let Some(p) = pin {
            if let Some(prev) = seen.insert(p, name) {
                return Err(StoreError::bad_request(format!(
                    "GPIO {p} is assigned more than once ({prev} and {name})"
                )));
            }
        }
    }

    // Common-sense checks.
    if cfg.spdt1_a.is_some() && cfg.spdt1_a == cfg.spdt1_b {
        return Err(StoreError::bad_request("spdt1_a and spdt1_b must be different"));
    }
    if cfg.spdt2_a.is_some() && cfg.spdt2_a == cfg.spdt2_b {
        return Err(StoreError::bad_request("spdt2_a and spdt2_b must be different"));
    }
    if cfg.spdt3_a.is_some() && cfg.spdt3_a == cfg.spdt3_b {
        return Err(StoreError::bad_request("spdt3_a and spdt3_b must be different"));
    }
    if cfg.rotary_a.is_some() && cfg.rotary_a == cfg.rotary_b {
        return Err(StoreError::bad_request("rotary_a and rotary_b must be different"));
    }

    Ok(())
}
