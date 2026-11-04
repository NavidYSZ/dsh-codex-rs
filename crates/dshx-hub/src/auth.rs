//! Accounts: sign-up, sign-in, sessions, verification codes, passwords, the
//! login limiter and the admin tier change.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{FromRef, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::UserRow;
use crate::error::Body;
use crate::tiers::Tier;
use crate::{ApiError, ApiResult, Hub, constant_time_eq, now_ms, random_token, sha256_hex};

/// Sessions last 90 days from their last use.
pub const SESSION_TTL_MS: i64 = 90 * 24 * 3_600_000;
/// A session's expiry is pushed at most once a minute, not on every request.
const SESSION_REFRESH_MS: i64 = 60_000;
const VERIFICATION_TTL_MS: i64 = 24 * 3_600_000;
pub const MIN_PASSWORD_CHARS: usize = 10;
const MAX_PASSWORD_CHARS: usize = 512;
pub const MAX_NAME_CHARS: usize = 80;
const MAX_EMAIL_CHARS: usize = 254;
pub const LOGIN_ATTEMPTS_PER_MINUTE: usize = 10;

/// The account as the API shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub email_verified: bool,
    pub created_at: i64,
}

impl From<&UserRow> for User {
    fn from(row: &UserRow) -> Self {
        User {
            id: row.id.clone(),
            email: row.email.clone(),
            name: row.name.clone(),
            email_verified: row.email_verified,
            created_at: row.created_at,
        }
    }
}

/// Argon2id with the crate's defaults, salted by the crate; the PHC string
/// carries the parameters, so they can change without a migration.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    use argon2::password_hash::PasswordHasher as _;
    Ok(argon2::Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::password_hash::PasswordVerifier as _;
    use argon2::password_hash::phc::PasswordHash;
    PasswordHash::new(hash)
        .map(|parsed| {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Hashing takes tens of milliseconds: off the async threads.
async fn hash_blocking(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| anyhow::anyhow!("hashing task: {e}"))?
        .map_err(|e| anyhow::anyhow!("hashing: {e}").into())
}

async fn verify_blocking(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false)
}

/// The signed-in user behind a request (`Authorization: Bearer ahs_…`).
#[derive(Debug, Clone)]
pub struct Session {
    pub user: UserRow,
    pub token_hash: String,
}

impl<S> FromRequestParts<S> for Session
where
    S: Send + Sync,
    Hub: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let hub = Hub::from_ref(state);
        let token = bearer(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        authenticate(&hub, &token)?.ok_or_else(ApiError::unauthorized)
    }
}

/// `Authorization: Bearer <admin token>`; 403 `admin-disabled` without one.
pub struct Admin;

impl<S> FromRequestParts<S> for Admin
where
    S: Send + Sync,
    Hub: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let hub = Hub::from_ref(state);
        let Some(expected) = hub.options().admin_token.as_deref() else {
            return Err(ApiError::forbidden(
                "admin-disabled",
                "the hub runs without an admin token",
            ));
        };
        let given = bearer(&parts.headers).unwrap_or_default();
        if constant_time_eq(given.as_bytes(), expected.as_bytes()) {
            Ok(Admin)
        } else {
            Err(ApiError::unauthorized())
        }
    }
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// The session behind a token, refreshed; `None` for unknown or expired ones.
pub(crate) fn authenticate(hub: &Hub, token: &str) -> rusqlite::Result<Option<Session>> {
    if !token.starts_with("ahs_") {
        return Ok(None);
    }
    let hash = sha256_hex(token);
    let Some(row) = hub.db().session(&hash)? else {
        return Ok(None);
    };
    let now = now_ms();
    if row.expires_at <= now {
        hub.db().delete_session(&hash)?;
        return Ok(None);
    }
    if now - row.last_used_at >= SESSION_REFRESH_MS {
        hub.db().refresh_session(&hash, now, now + SESSION_TTL_MS)?;
    }
    let Some(user) = hub.db().user_by_id(&row.user_id)? else {
        return Ok(None);
    };
    Ok(Some(Session {
        user,
        token_hash: hash,
    }))
}

pub(crate) fn issue_session(hub: &Hub, user_id: &str) -> rusqlite::Result<String> {
    let token = random_token("ahs_");
    let now = now_ms();
    hub.db()
        .insert_session(&sha256_hex(&token), user_id, now, now + SESSION_TTL_MS)?;
    Ok(token)
}

// Validation

pub fn normalize_email(raw: &str) -> ApiResult<String> {
    let email = raw.trim().to_lowercase();
    let shape_ok = email.len() <= MAX_EMAIL_CHARS
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if shape_ok {
        Ok(email)
    } else {
        Err(ApiError::bad_request(
            "invalid-email",
            "enter a valid email address",
        ))
    }
}

pub fn validate_password(password: &str) -> ApiResult<()> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        Err(ApiError::bad_request(
            "weak-password",
            format!("the password needs at least {MIN_PASSWORD_CHARS} characters"),
        ))
    } else if chars > MAX_PASSWORD_CHARS {
        Err(ApiError::bad_request(
            "weak-password",
            format!("the password is too long ({MAX_PASSWORD_CHARS} characters at most)"),
        ))
    } else {
        Ok(())
    }
}

/// Trims; an empty name is "no name".
pub fn validate_name(name: Option<String>) -> ApiResult<Option<String>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let name = name.trim();
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request(
            "invalid-name",
            format!("the name is too long ({MAX_NAME_CHARS} characters at most)"),
        ));
    }
    Ok((!name.is_empty()).then(|| name.to_owned()))
}

fn verification_code() -> String {
    use rand::Rng as _;
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

/// Sliding one-minute window per key, in memory: enough for one hub process.
pub struct RateLimiter {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_per_minute: usize) -> Self {
        RateLimiter {
            max: max_per_minute,
            window: Duration::from_secs(60),
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Records an attempt; false once `max` of them fell into the window.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if hits.len() > 10_000 {
            // Keep the map bounded: forget keys whose window emptied.
            hits.retain(|_, queue| {
                queue
                    .back()
                    .is_some_and(|t| now.duration_since(*t) < self.window)
            });
        }
        let queue = hits.entry(key.to_owned()).or_default();
        while queue
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            queue.pop_front();
        }
        if queue.len() >= self.max {
            return false;
        }
        queue.push_back(now);
        true
    }
}

// Handlers

#[derive(Deserialize)]
pub struct SignupBody {
    email: String,
    password: String,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /v1/auth/signup` → 201 `{session, user, verification?}`.
pub async fn signup(
    State(hub): State<Hub>,
    Body(body): Body<SignupBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let email = normalize_email(&body.email)?;
    validate_password(&body.password)?;
    let name = validate_name(body.name)?;
    let taken = || ApiError::conflict("email-taken", "an account with this email exists");
    if hub.db().user_by_email(&email)?.is_some() {
        return Err(taken());
    }
    let password_hash = hash_blocking(body.password).await?;
    let now = now_ms();
    let code = verification_code();
    let user = UserRow {
        id: format!("usr_{}", uuid::Uuid::new_v4().simple()),
        email,
        name,
        password_hash,
        email_verified: false,
        verification_code: Some(code.clone()),
        verification_expires_at: Some(now + VERIFICATION_TTL_MS),
        tier_id: "free".into(),
        created_at: now,
    };
    match hub.db().insert_user(&user) {
        Ok(()) => {}
        // Two signups raced for the same email.
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            return Err(taken());
        }
        Err(e) => return Err(e.into()),
    }
    let session = issue_session(&hub, &user.id)?;
    let mut response = json!({ "session": session, "user": User::from(&user) });
    if hub.options().dev {
        tracing::info!("dev: verification code for {} is {code}", user.email);
        response["verification"] = json!({
            "code": code,
            "expiresAt": user.verification_expires_at,
        });
    }
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Deserialize)]
pub struct LoginBody {
    email: String,
    password: String,
}

/// `POST /v1/auth/login` → `{session, user}`; 401 `invalid-credentials`.
pub async fn login(State(hub): State<Hub>, Body(body): Body<LoginBody>) -> ApiResult<Json<Value>> {
    if !hub.logins().allow(&body.email.trim().to_lowercase()) {
        return Err(ApiError::too_many(
            "too many sign-in attempts; try again in a minute",
        ));
    }
    let user = match normalize_email(&body.email) {
        Ok(email) => hub.db().user_by_email(&email)?,
        Err(_) => None,
    };
    // Unknown emails verify against a decoy so the timing gives nothing away.
    let hash = user
        .as_ref()
        .map_or_else(|| hub.decoy_hash().to_owned(), |u| u.password_hash.clone());
    let ok = verify_blocking(body.password, hash).await;
    let Some(user) = user.filter(|_| ok) else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid-credentials",
            "wrong email or password",
        ));
    };
    let _ = hub.db().purge_expired(now_ms());
    let session = issue_session(&hub, &user.id)?;
    Ok(Json(
        json!({ "session": session, "user": User::from(&user) }),
    ))
}

/// `POST /v1/auth/logout` → 204.
pub async fn logout(State(hub): State<Hub>, session: Session) -> ApiResult<StatusCode> {
    hub.db().delete_session(&session.token_hash)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct VerifyBody {
    code: String,
}

/// `POST /v1/auth/verify {code}` → `{user}`; 400 `invalid-code`.
pub async fn verify(
    State(hub): State<Hub>,
    session: Session,
    Body(body): Body<VerifyBody>,
) -> ApiResult<Json<Value>> {
    let user = session.user;
    if user.email_verified {
        return Ok(Json(json!({ "user": User::from(&user) })));
    }
    let code = body.code.trim();
    let valid = user
        .verification_code
        .as_deref()
        .is_some_and(|expected| constant_time_eq(expected.as_bytes(), code.as_bytes()))
        && user.verification_expires_at.is_some_and(|t| t > now_ms());
    if !valid {
        return Err(ApiError::bad_request(
            "invalid-code",
            "the code is wrong or expired",
        ));
    }
    hub.db().set_email_verified(&user.id)?;
    let user = hub
        .db()
        .user_by_id(&user.id)?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(json!({ "user": User::from(&user) })))
}

/// `GET /v1/me` → `{user, tier, usage:{servers}}`.
pub async fn me(State(hub): State<Hub>, session: Session) -> ApiResult<Json<Value>> {
    let tier = crate::tiers::of_user(&hub, &session.user)?;
    let servers = hub.db().count_servers(&session.user.id)?;
    Ok(Json(json!({
        "user": User::from(&session.user),
        "tier": tier,
        "usage": { "servers": servers },
    })))
}

#[derive(Deserialize)]
pub struct UpdateMeBody {
    #[serde(default)]
    name: Option<String>,
}

/// `PATCH /v1/me {name?}` → `{user}`; an empty name clears it.
pub async fn update_me(
    State(hub): State<Hub>,
    session: Session,
    Body(body): Body<UpdateMeBody>,
) -> ApiResult<Json<Value>> {
    if let Some(name) = body.name {
        let name = validate_name(Some(name))?;
        hub.db().set_user_name(&session.user.id, name.as_deref())?;
    }
    let user = hub
        .db()
        .user_by_id(&session.user.id)?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(json!({ "user": User::from(&user) })))
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

/// `POST /v1/auth/password {current, new}` → 204; the other sessions of the
/// account end, the one making the change stays.
pub async fn change_password(
    State(hub): State<Hub>,
    session: Session,
    Body(body): Body<PasswordBody>,
) -> ApiResult<StatusCode> {
    validate_password(&body.new)?;
    if !verify_blocking(body.current, session.user.password_hash.clone()).await {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid-credentials",
            "the current password is wrong",
        ));
    }
    let hash = hash_blocking(body.new).await?;
    hub.db().set_password_hash(&session.user.id, &hash)?;
    hub.db()
        .delete_other_sessions(&session.user.id, &session.token_hash)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierBody {
    tier_id: String,
}

/// `POST /v1/admin/users/:id/tier {tierId}` → `{user, tier}`.
pub async fn admin_set_tier(
    State(hub): State<Hub>,
    _admin: Admin,
    Path(id): Path<String>,
    Body(body): Body<TierBody>,
) -> ApiResult<Json<Value>> {
    let Some(tier) = hub.db().tier(&body.tier_id)? else {
        return Err(ApiError::bad_request(
            "unknown-tier",
            format!("no tier {:?}", body.tier_id),
        ));
    };
    let no_user = || ApiError::not_found("no such user");
    let user = hub.db().user_by_id(&id)?.ok_or_else(no_user)?;
    hub.db().set_tier(&user.id, &tier.id)?;
    let user = hub.db().user_by_id(&id)?.ok_or_else(no_user)?;
    Ok(Json(
        json!({ "user": User::from(&user), "tier": Tier::from(tier) }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_shapes() {
        assert_eq!(
            normalize_email(" Ada@Example.com ").unwrap(),
            "ada@example.com"
        );
        for bad in [
            "",
            "ada",
            "ada@",
            "@example.com",
            "ada@example",
            "a b@example.com",
        ] {
            assert!(normalize_email(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn password_and_name_rules() {
        assert!(validate_password("123456789").is_err());
        assert!(validate_password("1234567890").is_ok());
        assert_eq!(validate_name(Some("  ".into())).unwrap(), None);
        assert!(validate_name(Some("x".repeat(81))).is_err());
    }

    #[test]
    fn limiter_counts_a_minute() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.allow("a"));
        assert!(limiter.allow("a"));
        assert!(!limiter.allow("a"));
        assert!(limiter.allow("b"));
    }

    #[test]
    fn hashes_verify() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("correct horse", &hash));
        assert!(!verify_password("x", "not a hash"));
    }
}
