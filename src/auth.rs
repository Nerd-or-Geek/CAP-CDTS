use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use rand_core::{OsRng, RngCore};
use tokio::sync::RwLock;

use crate::models::{
    normalize_level, level_name, LoginResponse, UserPublic, LEVEL_BASIC,
};
use crate::store::JsonStore;

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub username: String,
    pub level: i32,
}

#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<Inner>,
}

struct Inner {
    ttl_secs: i64,
    sessions: RwLock<HashMap<String, SessionInfo>>,
}

#[derive(Clone, Debug)]
struct SessionInfo {
    user: AuthUser,
    expires_at_unix: i64,
    expires_at_utc: String,
}

#[derive(Debug)]
pub enum AuthErrorKind {
    BadRequest,
    Unauthorized,
    Forbidden,
    Internal,
}

#[derive(Debug)]
pub struct AuthError {
    pub kind: AuthErrorKind,
    pub message: String,
}

impl AuthError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            kind: AuthErrorKind::BadRequest,
            message: msg.into(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            kind: AuthErrorKind::Unauthorized,
            message: msg.into(),
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            kind: AuthErrorKind::Forbidden,
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            kind: AuthErrorKind::Internal,
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AuthError {}

impl AuthManager {
    pub fn new(ttl_secs: i64) -> Self {
        Self {
            inner: Arc::new(Inner {
                ttl_secs,
                sessions: RwLock::new(HashMap::new()),
            }),
        }
    }

    pub async fn login(
        &self,
        store: &JsonStore,
        username: &str,
        passcode: &str,
    ) -> Result<LoginResponse, AuthError> {
        let username = username.trim();
        if username.is_empty() {
            return Err(AuthError::bad_request("username is required"));
        }

        let passcode = passcode.trim();
        if passcode.is_empty() {
            return Err(AuthError::bad_request("passcode is required"));
        }

        let user = store
            .get_user_record(username)
            .await
            .ok_or_else(|| AuthError::unauthorized("invalid username or passcode"))?;

        let level = normalize_level(user.level);
        if level == LEVEL_BASIC {
            return Err(AuthError::forbidden("basic users cannot log in"));
        }

        let Some(hash) = user.passcode_hash.as_deref() else {
            return Err(AuthError::unauthorized("invalid username or passcode"));
        };

        let ok = verify_passcode(hash, passcode).map_err(|e| {
            AuthError::internal(format!("failed to verify passcode: {e}"))
        })?;

        if !ok {
            return Err(AuthError::unauthorized("invalid username or passcode"));
        }

        let token = generate_token();
        let now_unix = now_unix();
        let expires_unix = now_unix + self.inner.ttl_secs;
        let expires_at_utc = unix_to_rfc3339(expires_unix).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());

        let auth_user = AuthUser {
            username: user.username.clone(),
            level,
        };

        let info = SessionInfo {
            user: auth_user.clone(),
            expires_at_unix: expires_unix,
            expires_at_utc: expires_at_utc.clone(),
        };

        {
            let mut sessions = self.inner.sessions.write().await;
            sessions.insert(token.clone(), info);
        }

        let public = UserPublic::from(&user);

        Ok(LoginResponse {
            token,
            user: UserPublic {
                level,
                ..public
            },
            expires_at_utc,
        })
    }

    pub async fn authenticate(&self, token: &str) -> Option<AuthUser> {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }

        let now = now_unix();

        let mut remove = false;
        let out = {
            let sessions = self.inner.sessions.read().await;
            match sessions.get(token) {
                None => None,
                Some(info) => {
                    if info.expires_at_unix <= now {
                        remove = true;
                        None
                    } else {
                        Some(info.user.clone())
                    }
                }
            }
        };

        if remove {
            let mut sessions = self.inner.sessions.write().await;
            sessions.remove(token);
        }

        out
    }

    pub async fn logout(&self, token: &str) -> bool {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        let mut sessions = self.inner.sessions.write().await;
        sessions.remove(token).is_some()
    }

    pub fn role_name(level: i32) -> &'static str {
        level_name(level)
    }
}

fn verify_passcode(hash: &str, passcode: &str) -> anyhow::Result<bool> {
    use argon2::{Argon2, PasswordVerifier};
    use password_hash::PasswordHash;

    let parsed = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!(e))
        .context("parse passcode hash")?;
    let argon2 = Argon2::default();

    Ok(argon2
        .verify_password(passcode.as_bytes(), &parsed)
        .is_ok())
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);

    // Hex string
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn unix_to_rfc3339(ts: i64) -> anyhow::Result<String> {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    let dt = OffsetDateTime::from_unix_timestamp(ts).context("from unix timestamp")?;
    Ok(dt
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()))
}
