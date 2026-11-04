//! The user's servers: enrol codes, enrolment with a server key, renaming,
//! deletion and the list with its online marks.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{Session, validate_name};
use crate::db::ServerRow;
use crate::error::Body;
use crate::relay_client::ServerInfo;
use crate::tiers::Tier;
use crate::{ApiError, ApiResult, Hub, now_ms, random_token, sha256_hex};

/// An enrol code is good for ten minutes.
pub const ENROL_CODE_TTL_MS: i64 = 10 * 60_000;
/// Enrolment attempts hub-wide per minute: codes are short-lived and
/// single-use, this keeps guessing them pointless.
pub const ENROL_ATTEMPTS_PER_MINUTE: usize = 60;
/// No I and O: the code is typed by hand.
const CODE_LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const MAX_FIELD_CHARS: usize = 200;

/// A server as the API shows it (`online` comes from the relay registry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub version: String,
    pub online: bool,
    pub last_seen_at: Option<i64>,
    pub created_at: i64,
}

impl Server {
    pub(crate) fn from_row(row: ServerRow, online: bool) -> Server {
        Server {
            id: row.id,
            name: row.name,
            hostname: row.hostname,
            platform: row.platform,
            version: row.version,
            online,
            last_seen_at: row.last_seen_at,
            created_at: row.created_at,
        }
    }
}

/// `ABCD-1234`: four letters, four digits.
pub fn new_enroll_code() -> String {
    use rand::Rng as _;
    let mut rng = rand::rng();
    let letters: String = (0..4)
        .map(|_| CODE_LETTERS[rng.random_range(0..CODE_LETTERS.len())] as char)
        .collect();
    format!("{letters}-{:04}", rng.random_range(0..10_000u32))
}

/// Accepts `abcd1234`, `ABCD-1234` or `abcd 1234`; the canonical form, or
/// `None` when it is not a code at all.
pub fn normalize_code(raw: &str) -> Option<String> {
    let chars: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let (letters, digits) = chars.split_at_checked(4)?;
    if digits.len() != 4
        || !letters.bytes().all(|b| b.is_ascii_uppercase())
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{letters}-{digits}"))
}

/// `GET /v1/servers` → `[Server]`.
pub async fn list(State(hub): State<Hub>, session: Session) -> ApiResult<Json<Vec<Server>>> {
    let rows = hub.db().servers_of(&session.user.id)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| {
                let online = hub.registry().is_online(&row.id);
                Server::from_row(row, online)
            })
            .collect(),
    ))
}

/// `POST /v1/servers/enroll-code` → `{code, expiresAt}`; 403 `tier-limit`.
/// A new code replaces the user's earlier unused one.
pub async fn enroll_code(State(hub): State<Hub>, session: Session) -> ApiResult<Json<Value>> {
    let tier = crate::tiers::of_user(&hub, &session.user)?;
    check_limit(&hub, &session.user.id, &tier)?;
    let now = now_ms();
    let expires_at = now + ENROL_CODE_TTL_MS;
    let mut code = new_enroll_code();
    for attempt in 0.. {
        match hub
            .db()
            .replace_enroll_code(&session.user.id, &code, now, expires_at)
        {
            Ok(()) => break,
            // Another user holds the same code right now: draw again.
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation && attempt < 5 =>
            {
                code = new_enroll_code();
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Json(json!({ "code": code, "expiresAt": expires_at })))
}

fn check_limit(hub: &Hub, user_id: &str, tier: &Tier) -> ApiResult<()> {
    if let Some(limit) = tier.limits.servers
        && hub.db().count_servers(user_id)? >= limit
    {
        let plural = if limit == 1 { "" } else { "s" };
        return Err(ApiError::forbidden(
            "tier-limit",
            format!("the {} tier allows {limit} server{plural}", tier.name),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct EnrollBody {
    code: String,
    #[serde(flatten)]
    server: ServerInfo,
}

/// `POST /v1/servers/enroll` (no auth) → 201 `{server, serverKey}`.
///
/// The same `serverId` enrolled again by its owner gets a new key and keeps
/// its row; an id that belongs to another account is refused with 409.
pub async fn enroll(
    State(hub): State<Hub>,
    Body(body): Body<EnrollBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !hub.enrolments().allow("enroll") {
        return Err(ApiError::too_many(
            "too many enrolment attempts; try again in a minute",
        ));
    }
    let Some(code) = normalize_code(&body.code) else {
        return Err(ApiError::bad_request(
            "invalid-code",
            "an enrol code looks like ABCD-1234",
        ));
    };
    let Some(row) = hub.db().enroll_code(&code)? else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "enroll-code-unknown",
            "no such enrol code",
        ));
    };
    let used = || ApiError::conflict("enroll-code-used", "the enrol code was already used");
    if row.used_at.is_some() {
        return Err(used());
    }
    let now = now_ms();
    if row.expires_at <= now {
        return Err(ApiError::bad_request(
            "enroll-code-expired",
            "the enrol code expired; make a new one in the app",
        ));
    }
    let Some(user) = hub.db().user_by_id(&row.user_id)? else {
        return Err(ApiError::not_found("no such enrol code"));
    };
    let info = body.server;
    for (field, value) in [
        ("hostname", &info.hostname),
        ("platform", &info.platform),
        ("version", &info.version),
    ] {
        if value.chars().count() > MAX_FIELD_CHARS {
            return Err(ApiError::bad_request(
                "invalid-body",
                format!("{field} is too long ({MAX_FIELD_CHARS} characters at most)"),
            ));
        }
    }
    let name = validate_name(Some(info.name))?.unwrap_or_else(|| {
        let hostname = info.hostname.trim();
        if hostname.is_empty() {
            "Server".to_owned()
        } else {
            hostname.to_owned()
        }
    });
    let id = match info.id {
        Some(id) => {
            validate_server_id(&id)?;
            id
        }
        None => format!("srv_{}", uuid::Uuid::new_v4().simple()),
    };
    let existing = hub.db().server(&id)?;
    if let Some(existing) = &existing
        && existing.user_id != user.id
    {
        return Err(ApiError::conflict(
            "server-exists",
            "a server with this id belongs to another account",
        ));
    }
    if existing.is_none() {
        let tier = crate::tiers::of_user(&hub, &user)?;
        check_limit(&hub, &user.id, &tier)?;
    }
    if !hub.db().use_enroll_code(&code, now)? {
        return Err(used());
    }
    let key = random_token("ask_");
    let key_hash = sha256_hex(&key);
    let row = ServerRow {
        id,
        user_id: user.id.clone(),
        name,
        hostname: info.hostname.trim().to_owned(),
        platform: info.platform.trim().to_owned(),
        version: info.version.trim().to_owned(),
        created_at: existing.as_ref().map_or(now, |e| e.created_at),
        last_seen_at: existing.as_ref().and_then(|e| e.last_seen_at),
    };
    if existing.is_some() {
        // The old key is gone: a connection made with it ends now.
        hub.registry().disconnect(&row.id);
        hub.db().reenroll_server(&row, &key_hash)?;
    } else {
        hub.db().insert_server(&row, &key_hash)?;
    }
    tracing::info!("server {} enrolled for account {}", row.id, user.id);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "server": Server::from_row(row, false), "serverKey": key })),
    ))
}

fn validate_server_id(id: &str) -> ApiResult<()> {
    let body = id.strip_prefix("srv_").unwrap_or("");
    let ok = !body.is_empty()
        && body.len() <= 64
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid-server-id",
            "serverId is srv_ followed by up to 64 letters, digits, - or _",
        ))
    }
}

#[derive(Deserialize)]
pub struct RenameBody {
    #[serde(default)]
    name: Option<String>,
}

/// `PATCH /v1/servers/:id {name?}` → `Server`.
pub async fn rename(
    State(hub): State<Hub>,
    session: Session,
    Path(id): Path<String>,
    Body(body): Body<RenameBody>,
) -> ApiResult<Json<Server>> {
    let row = owned_server(&hub, &session, &id)?;
    if let Some(name) = body.name {
        let name = validate_name(Some(name))?
            .ok_or_else(|| ApiError::bad_request("invalid-name", "the name must not be empty"))?;
        hub.db().rename_server(&row.id, &name)?;
    }
    let row = hub
        .db()
        .server(&id)?
        .ok_or_else(|| ApiError::not_found("no such server"))?;
    let online = hub.registry().is_online(&row.id);
    Ok(Json(Server::from_row(row, online)))
}

/// `DELETE /v1/servers/:id` → 204; its key is gone and its relay connection closes.
pub async fn delete(
    State(hub): State<Hub>,
    session: Session,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let row = owned_server(&hub, &session, &id)?;
    hub.db().delete_server(&row.id)?;
    hub.registry().disconnect(&row.id);
    tracing::info!("server {} deleted by account {}", row.id, session.user.id);
    Ok(StatusCode::NO_CONTENT)
}

/// The server if it is the user's: 404 when unknown, 403 when someone else's.
pub(crate) fn owned_server(hub: &Hub, session: &Session, id: &str) -> ApiResult<ServerRow> {
    let Some(row) = hub.db().server(id)? else {
        return Err(ApiError::not_found("no such server"));
    };
    if row.user_id != session.user.id {
        return Err(ApiError::forbidden(
            "forbidden",
            "this server belongs to another account",
        ));
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_have_the_documented_shape() {
        for _ in 0..50 {
            let code = new_enroll_code();
            assert_eq!(normalize_code(&code).as_deref(), Some(code.as_str()));
            assert!(!code.contains('I') && !code.contains('O'));
        }
        assert_eq!(normalize_code(" abcd 1234 ").as_deref(), Some("ABCD-1234"));
        assert_eq!(normalize_code("abcd-123"), None);
        assert_eq!(normalize_code("1234-abcd"), None);
    }

    #[test]
    fn server_ids() {
        assert!(validate_server_id("srv_8f1c").is_ok());
        assert!(validate_server_id("srv_").is_err());
        assert!(validate_server_id("cli_x").is_err());
        assert!(validate_server_id("srv_a/b").is_err());
    }
}
