//! Reports on the admin dashboard: where a person's or a group's reports
//! go, for each feature (`/prayers reports here|dm|off` in WhatsApp).
//!
//! Generic over `Feature`: a feature that adds reports (habits, later)
//! gets these endpoints by adding a field to `ReportsView`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::repo::reports;
use crate::reports::{Delivery, Feature};
use crate::routes::admin::{self, UserDetail};
use crate::routes::admin_groups::{self, GroupDetail};
use crate::routes::admin_settings::dm_for;

/// Where each feature's reports go: "here", "dm" (a group only) or "off".
#[derive(Debug, Serialize)]
pub(crate) struct ReportsView {
    prayers: String,
    /// Points: a group's Sunday summary (`points::reports`), or a person's
    /// monthly and yearly reviews in their DM (`points::reviews`).
    points: String,
}

/// `chat_id`'s report settings; `None` (a person with no DM yet) reads as
/// the defaults, as WhatsApp would.
pub(crate) async fn reports_view(
    conn: &mut SqliteConnection,
    chat_id: Option<i64>,
) -> Result<ReportsView, sqlx::Error> {
    let mut view = ReportsView {
        prayers: Delivery::default().as_db_str().to_string(),
        points: Delivery::default().as_db_str().to_string(),
    };
    if let Some(chat) = chat_id {
        view.prayers = reports::delivery(conn, chat, Feature::Prayers)
            .await?
            .as_db_str()
            .to_string();
        view.points = reports::delivery(conn, chat, Feature::Points)
            .await?
            .as_db_str()
            .to_string();
    }
    Ok(view)
}

/// The body of the report endpoints: `{"delivery": "here"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryBody {
    delivery: String,
}

/// The feature named in the path (404 for one with no reports) and the
/// delivery asked for (400 for one that isn't allowed here).
fn parse(
    feature: &str,
    body: &DeliveryBody,
    is_group: bool,
) -> Result<(Feature, Delivery), AppError> {
    let feature: Feature = feature.parse().map_err(|_| AppError::NotFound)?;
    match body.delivery.parse() {
        Ok(Delivery::Dm) if !is_group => Err(AppError::BadRequest(
            "\"dm\" is only for a group; a person's reports already come to their DM".into(),
        )),
        Ok(delivery) => Ok((feature, delivery)),
        Err(_) => Err(AppError::BadRequest(format!(
            "delivery must be \"here\", \"dm\" or \"off\", not \"{}\"",
            body.delivery
        ))),
    }
}

/// `POST /api/users/{id}/reports/{feature}` with `{"delivery": "here"}` or
/// `"off"`: their reports, in their DM.
pub async fn user_reports(
    State(state): State<Arc<AppState>>,
    Path((id, feature)): Path<(i64, String)>,
    Json(body): Json<DeliveryBody>,
) -> Result<Json<UserDetail>, AppError> {
    let (feature, delivery) = parse(&feature, &body, false)?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let dm = dm_for(&mut tx, id).await?;
    reports::set_delivery(&mut tx, dm, feature, delivery).await?;
    tx.commit().await?;
    info!(
        "user {id}'s {} reports: {}",
        feature.as_db_str(),
        delivery.as_db_str()
    );

    let mut conn = state.db.acquire().await?;
    Ok(Json(admin::detail(&mut conn, &state, id).await?))
}

/// `POST /api/groups/{id}/reports/{feature}` with `{"delivery": "dm"}`:
/// the group's reports. 404 for a DM.
pub async fn group_reports(
    State(state): State<Arc<AppState>>,
    Path((id, feature)): Path<(i64, String)>,
    Json(body): Json<DeliveryBody>,
) -> Result<Json<GroupDetail>, AppError> {
    let (feature, delivery) = parse(&feature, &body, true)?;
    let mut conn = state.db.acquire().await?;
    admin_groups::ensure_group(&mut conn, id).await?;
    reports::set_delivery(&mut conn, id, feature, delivery).await?;
    info!(
        "group {id}'s {} reports: {}",
        feature.as_db_str(),
        delivery.as_db_str()
    );
    Ok(Json(admin_groups::detail(&mut conn, &state, id).await?))
}
