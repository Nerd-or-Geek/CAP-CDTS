use serde::{Deserialize, Serialize};

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
