//! Tiers: what a plan allows. Seeded by the first migration (free, pro,
//! team); the rows in `tiers` are what the API reports, so prices and limits
//! can be changed in the database without a release.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::db::{TierRow, UserRow};
use crate::{ApiResult, Hub};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tier {
    pub id: String,
    pub name: String,
    pub limits: Limits,
    /// `None` for the free tier.
    pub price_eur_monthly: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// `None` = unlimited.
    pub servers: Option<i64>,
    pub relay: bool,
}

impl From<TierRow> for Tier {
    fn from(row: TierRow) -> Self {
        Tier {
            id: row.id,
            name: row.name,
            limits: Limits {
                servers: row.servers,
                relay: row.relay,
            },
            price_eur_monthly: row.price_eur_monthly,
        }
    }
}

/// `GET /v1/tiers` (no auth).
pub async fn list(State(hub): State<Hub>) -> ApiResult<Json<Vec<Tier>>> {
    Ok(Json(
        hub.db().tiers()?.into_iter().map(Tier::from).collect(),
    ))
}

/// The user's tier; a user whose tier row vanished counts as free.
pub(crate) fn of_user(hub: &Hub, user: &UserRow) -> rusqlite::Result<Tier> {
    let row = match hub.db().tier(&user.tier_id)? {
        Some(row) => row,
        None => hub.db().tier("free")?.unwrap_or(TierRow {
            id: "free".into(),
            name: "Free".into(),
            servers: Some(2),
            relay: true,
            price_eur_monthly: None,
        }),
    };
    Ok(row.into())
}
