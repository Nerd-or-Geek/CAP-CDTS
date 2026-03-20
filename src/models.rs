use serde::{Deserialize, Serialize};

// User permission levels (lower number = more privilege)
//
// Requested model:
//  1 = Admin
//  2 = Junior admin
//  3 = Moderator
//  4 = Advanced user
//  5 = Basic (name only; no login)
pub const LEVEL_ADMIN: i32 = 1;
pub const LEVEL_JUNIOR_ADMIN: i32 = 2;
pub const LEVEL_MODERATOR: i32 = 3;
pub const LEVEL_ADVANCED_USER: i32 = 4;
pub const LEVEL_BASIC: i32 = 5;

pub fn normalize_level(level: i32) -> i32 {
    match level {
        LEVEL_ADMIN => LEVEL_ADMIN,
        LEVEL_JUNIOR_ADMIN => LEVEL_JUNIOR_ADMIN,
        LEVEL_MODERATOR => LEVEL_MODERATOR,
        LEVEL_ADVANCED_USER => LEVEL_ADVANCED_USER,
        LEVEL_BASIC => LEVEL_BASIC,
        // Legacy: older builds used 0 for a generic "user".
        0 => LEVEL_MODERATOR,
        _ => LEVEL_BASIC,
    }
}

pub fn level_name(level: i32) -> &'static str {
    match normalize_level(level) {
        LEVEL_ADMIN => "Admin",
        LEVEL_JUNIOR_ADMIN => "Junior admin",
        LEVEL_MODERATOR => "Moderator",
        LEVEL_ADVANCED_USER => "Advanced user",
        LEVEL_BASIC => "Basic",
        _ => "Basic",
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiStatus {
    pub status: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRecord {
    pub username: String,
    pub rfid_uid: String,
    pub level: i32,

    // Stored only on disk; never returned in /api/users.
    pub passcode_hash: Option<String>,

    pub created_at_utc: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserPublic {
    pub username: String,
    pub rfid_uid: String,
    pub level: i32,
}

impl From<&UserRecord> for UserPublic {
    fn from(value: &UserRecord) -> Self {
        Self {
            username: value.username.clone(),
            rfid_uid: value.rfid_uid.clone(),
            level: value.level,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub rfid_uid: String,
    pub level: i32,

    // Optional so you can bootstrap a user list without passcodes during early UI work.
    pub passcode: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub rfid_uid: Option<String>,
    pub level: Option<i32>,

    // If present, must be exactly 5 digits (or empty string to clear).
    pub passcode: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub passcode: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserPublic,
    pub expires_at_utc: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MeResponse {
    pub user: UserPublic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportRecord {
    pub num: u32,

    pub created_at_utc: String,

    pub opened_by: String,
    pub opened_by_level: Option<i32>,

    pub closed_by: Option<String>,
    pub closed_at_utc: Option<String>,
    pub closing_comments: Option<String>,

    // UI/metadata fields
    pub person: String,
    pub title: String,
    pub category: String,
    pub priority: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateReportRequest {
    pub person: String,
    pub title: String,
    pub category: String,
    pub priority: String,
    pub description: String,

    // Optional: some UIs include this for better attribution.
    pub opened_by: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateReportRequest {
    // Editable fields (admin/junior admin only)
    pub person: Option<String>,
    pub title: Option<String>,
    pub category: Option<String>,
    pub priority: Option<String>,
    pub description: Option<String>,

    // Status change:
    // - Some(true)  => close the report
    // - Some(false) => reopen the report
    // - None        => leave status unchanged
    pub closed: Option<bool>,

    // Optional closing notes (stored only when closing).
    pub closing_comments: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreData {
    pub schema_version: u32,

    // Used to allocate 6-digit report numbers.
    pub next_report_num: u32,

    pub gpio_config: GpioConfig,

    pub users: Vec<UserRecord>,
    pub reports: Vec<ReportRecord>,
}

impl Default for StoreData {
    fn default() -> Self {
        Self {
            schema_version: 1,
            next_report_num: 100_000,
            gpio_config: GpioConfig::default(),
            users: Vec::new(),
            reports: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GpioConfig {
    // Use BCM GPIO numbering (not physical header pin numbers).

    // RFID (e.g., MFRC522)
    pub rfid_spi_bus: Option<u8>,
    pub rfid_spi_cs: Option<u8>,
    pub rfid_rst_gpio: Option<u8>,

    // SPDT switches (2 pins per switch)
    pub spdt1_a: Option<u8>,
    pub spdt1_b: Option<u8>,
    pub spdt2_a: Option<u8>,
    pub spdt2_b: Option<u8>,
    pub spdt3_a: Option<u8>,
    pub spdt3_b: Option<u8>,

    // Rotary encoder
    pub rotary_a: Option<u8>,
    pub rotary_b: Option<u8>,
    pub rotary_btn: Option<u8>,

    // Capacitive touch input
    pub cap_touch: Option<u8>,

    // Additional push button (e.g., confirm)
    pub push_btn: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveState {
    pub last_update_utc: String,
    pub counts: LiveCounts,
    pub auth: AuthLive,
    pub gpio_config: GpioConfig,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveCounts {
    pub users: usize,
    pub reports: usize,
    pub open_reports: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthLive {
    // Placeholder for future auth integration.
    pub stage: String,
    pub user: Option<LiveUser>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveUser {
    pub username: String,
    pub level: i32,
}
