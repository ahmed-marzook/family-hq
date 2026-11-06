//! Reports: where each chat's go, per feature (`report_settings`), and
//! which have been sent (`reports_sent`).

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::reports::{Delivery, Feature, ReportKind};

/// Where this chat's reports for `feature` go. No row means `Here`.
pub async fn delivery(
    conn: &mut SqliteConnection,
    chat_id: i64,
    feature: Feature,
) -> Result<Delivery, sqlx::Error> {
    let feature = feature.as_db_str();
    let stored = sqlx::query_scalar!(
        "SELECT delivery FROM report_settings WHERE chat_id = ? AND feature = ?",
        chat_id,
        feature
    )
    .fetch_optional(conn)
    .await?;
    // The CHECK allows only the three, so the default is only used if the
    // code and the schema disagree (or there's no row).
    Ok(stored.and_then(|s| s.parse().ok()).unwrap_or_default())
}

/// Sets where this chat's reports for `feature` go.
pub async fn set_delivery(
    conn: &mut SqliteConnection,
    chat_id: i64,
    feature: Feature,
    delivery: Delivery,
) -> Result<(), sqlx::Error> {
    let (feature, delivery) = (feature.as_db_str(), delivery.as_db_str());
    sqlx::query!(
        "INSERT INTO report_settings (chat_id, feature, delivery) VALUES (?, ?, ?)
         ON CONFLICT (chat_id, feature) DO UPDATE SET delivery = excluded.delivery",
        chat_id,
        feature,
        delivery
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Claims the right to send one report, as `prayers::claim_reminder` does
/// for reminders. `false` if this chat already had it.
pub async fn claim(
    conn: &mut SqliteConnection,
    chat_id: i64,
    feature: Feature,
    kind: ReportKind,
    period_end: NaiveDate,
    place: &str,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let (feature, kind) = (feature.as_db_str(), kind.as_db_str());
    let result = sqlx::query!(
        "INSERT INTO reports_sent (chat_id, feature, kind, period_end, place, sent_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (chat_id, feature, kind, period_end, place) DO NOTHING",
        chat_id,
        feature,
        kind,
        period_end,
        place,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A person's DM with the bot, for reports sent privately: its WhatsApp
/// ID. `None` if they've never messaged the bot privately, or the bot has
/// left it.
pub async fn dm_chat(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT wa_chat_id FROM chats WHERE dm_user_id = ? AND left_at IS NULL",
        user_id
    )
    .fetch_optional(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;
    use crate::reports::Span;
    use crate::testkit::{jid, register};

    #[tokio::test]
    async fn delivery_is_per_chat_and_defaults_to_here() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let a = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let b = chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();
        let prayers = Feature::Prayers;
        assert_eq!(
            delivery(&mut conn, a, prayers).await.unwrap(),
            Delivery::Here
        );
        set_delivery(&mut conn, a, prayers, Delivery::Dm)
            .await
            .unwrap();
        set_delivery(&mut conn, a, prayers, Delivery::Off)
            .await
            .unwrap();
        assert_eq!(
            delivery(&mut conn, a, prayers).await.unwrap(),
            Delivery::Off
        );
        assert_eq!(
            delivery(&mut conn, b, prayers).await.unwrap(),
            Delivery::Here
        );
    }

    #[tokio::test]
    async fn a_report_is_claimed_once_per_chat_kind_day_and_place() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let week = ReportKind::Summary(Span::Week);
        let mut claim = async |kind, place| {
            claim(&mut conn, chat, Feature::Prayers, kind, day, place, now)
                .await
                .unwrap()
        };
        assert!(claim(week, "uk").await);
        assert!(!claim(week, "uk").await, "already sent");
        assert!(claim(ReportKind::Day, "uk").await, "another kind");
        assert!(claim(week, "lk").await, "another place");
    }

    #[tokio::test]
    async fn only_a_dm_the_bot_is_still_in_gets_reports() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = register(&mut conn, "Bilal", "447700900001", now).await.id;
        assert_eq!(dm_chat(&mut conn, user).await.unwrap(), None);
        chats::find_or_create(&mut conn, &jid("447700900001"), Some(user), now)
            .await
            .unwrap();
        assert_eq!(
            dm_chat(&mut conn, user).await.unwrap().as_deref(),
            Some("447700900001@s.whatsapp.net")
        );
    }
}
