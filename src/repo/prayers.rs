//! Prayer reminders (which chats get them, which were sent) and the log
//! of who has prayed what. Settings are in `repo::settings`.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::prayer::{Prayer, ReminderKind};
use crate::settings::Overrides;

/// A chat that has asked for prayer reminders, with its own settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ReminderChat {
    pub id: i64,
    pub wa_chat_id: String,
    /// Whose DM this is; `None` for a group.
    pub dm_user_id: Option<i64>,
    pub overrides: Overrides,
}

/// Every chat with prayer reminders on, with its settings.
pub async fn reminder_chats(conn: &mut SqliteConnection) -> Result<Vec<ReminderChat>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT c.id, c.wa_chat_id, c.dm_user_id, s.timezone, s.city, s.country,
                s.prayer_method, s.prayer_school, s.prayer_ending_minutes
         FROM chat_settings s
         JOIN chats c ON c.id = s.chat_id
         WHERE s.prayer_reminders = 1
         ORDER BY c.id"
    )
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ReminderChat {
            id: r.id,
            wa_chat_id: r.wa_chat_id,
            dm_user_id: r.dm_user_id,
            overrides: Overrides {
                timezone: r.timezone,
                city: r.city,
                country: r.country,
                prayer_method: r.prayer_method,
                prayer_school: r.prayer_school,
                prayer_ending_minutes: r.prayer_ending_minutes,
            },
        })
        .collect())
}

/// Claims the right to send one reminder. Returns the new row's id, or
/// `None` if this chat already got (or is getting) this reminder (this
/// prayer, this kind) today. Insert-first means two checks can never both send it.
pub async fn claim_reminder(
    conn: &mut SqliteConnection,
    chat_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    kind: ReminderKind,
    prayer_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<i64>, sqlx::Error> {
    let prayer = prayer.as_db_str();
    let kind = kind.as_db_str();
    // On a duplicate, `DO NOTHING` inserts no row, so `RETURNING` returns
    // no row either and `fetch_optional` gives `None`. (`AS "id!"`: the id
    // itself is never NULL, which the macro can't work out alone.)
    sqlx::query_scalar!(
        r#"INSERT INTO prayer_reminders (chat_id, prayer_date, prayer, kind, prayer_time, sent_at)
           VALUES (?, ?, ?, ?, ?, ?)
           ON CONFLICT (chat_id, prayer_date, prayer, kind) DO NOTHING
           RETURNING id AS "id!""#,
        chat_id,
        date,
        prayer,
        kind,
        prayer_time,
        now
    )
    .fetch_optional(conn)
    .await
}

/// Records the WhatsApp ID of a sent reminder, so reactions can find it.
pub async fn set_reminder_message(
    conn: &mut SqliteConnection,
    reminder_id: i64,
    wa_message_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE prayer_reminders SET wa_message_id = ? WHERE id = ?",
        wa_message_id,
        reminder_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Which prayer and day a reminder message was for, if `wa_message_id` is
/// one of our reminders in this chat.
pub async fn find_reminder(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    wa_chat_id: &str,
) -> Result<Option<(NaiveDate, Prayer)>, sqlx::Error> {
    // `as "prayer_date: NaiveDate"` asks the macro to decode the TEXT
    // column straight into a chrono date.
    let row = sqlx::query!(
        r#"SELECT r.prayer_date AS "prayer_date: NaiveDate", r.prayer
           FROM prayer_reminders r
           JOIN chats c ON c.id = r.chat_id
           WHERE r.wa_message_id = ? AND c.wa_chat_id = ?"#,
        wa_message_id,
        wa_chat_id
    )
    .fetch_optional(conn)
    .await?;

    // The CHECK constraint only allows the five names, so this parse can
    // only fail if the code and the schema disagree. `.ok()` drops such a
    // row rather than crashing.
    Ok(row.and_then(|r| Some((r.prayer_date, r.prayer.parse().ok()?))))
}

/// Marks a prayer as prayed. Doing it twice is fine (the first time wins).
pub async fn mark_prayed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let prayer = prayer.as_db_str();
    sqlx::query!(
        "INSERT INTO prayer_log (user_id, prayer_date, prayer, prayed_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, prayer_date, prayer) DO NOTHING",
        user_id,
        date,
        prayer,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Undoes `mark_prayed` (the reaction was removed).
pub async fn unmark_prayed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
) -> Result<(), sqlx::Error> {
    let prayer = prayer.as_db_str();
    sqlx::query!(
        "DELETE FROM prayer_log WHERE user_id = ? AND prayer_date = ? AND prayer = ?",
        user_id,
        date,
        prayer
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The prayers `user_id` has marked on `date`.
pub async fn prayed_on(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
) -> Result<Vec<Prayer>, sqlx::Error> {
    let names = sqlx::query_scalar!(
        "SELECT prayer FROM prayer_log WHERE user_id = ? AND prayer_date = ?",
        user_id,
        date
    )
    .fetch_all(conn)
    .await?;
    Ok(names.iter().filter_map(|n| n.parse().ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::repo::{chats, settings, users};

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 27).unwrap()
    }

    #[tokio::test]
    async fn only_opted_in_chats_get_reminders() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let a = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();

        assert!(reminder_chats(&mut conn).await.unwrap().is_empty());
        settings::set_prayer_reminders(&mut conn, a, true)
            .await
            .unwrap();
        let on = reminder_chats(&mut conn).await.unwrap();
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].wa_chat_id, "120363000000000001@g.us");

        settings::set_prayer_reminders(&mut conn, a, false)
            .await
            .unwrap();
        assert!(reminder_chats(&mut conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_reminder_is_claimed_once_and_found_by_message_id() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let group = "120363000000000001@g.us";
        let chat = chats::find_or_create(&mut conn, group, None, now)
            .await
            .unwrap();

        let id = claim_reminder(
            &mut conn,
            chat,
            date(),
            Prayer::Asr,
            ReminderKind::Start,
            now,
            now,
        )
        .await
        .unwrap()
        .expect("first claim works");
        let again = claim_reminder(
            &mut conn,
            chat,
            date(),
            Prayer::Asr,
            ReminderKind::Start,
            now,
            now,
        )
        .await
        .unwrap();
        assert_eq!(again, None, "second claim is refused");
        let ending = claim_reminder(
            &mut conn,
            chat,
            date(),
            Prayer::Asr,
            ReminderKind::Ending,
            now,
            now,
        )
        .await
        .unwrap();
        assert!(ending.is_some(), "the other kind is a separate reminder");

        set_reminder_message(&mut conn, id, "3EB0AA").await.unwrap();
        assert_eq!(
            find_reminder(&mut conn, "3EB0AA", group).await.unwrap(),
            Some((date(), Prayer::Asr))
        );
        let elsewhere = find_reminder(&mut conn, "3EB0AA", "120363000000000009@g.us").await;
        assert_eq!(elsewhere.unwrap(), None, "same message ID, other chat");
    }

    #[tokio::test]
    async fn mark_and_unmark_prayed() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let person = Person {
            name: "Bilal".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        let user = users::find_or_register(&mut conn, &person, now)
            .await
            .unwrap()
            .unwrap();

        mark_prayed(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        mark_prayed(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        mark_prayed(&mut conn, user.id, date(), Prayer::Asr, now)
            .await
            .unwrap();
        let mut prayed = prayed_on(&mut conn, user.id, date()).await.unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Asr, Prayer::Fajr]);

        unmark_prayed(&mut conn, user.id, date(), Prayer::Fajr)
            .await
            .unwrap();
        assert_eq!(
            prayed_on(&mut conn, user.id, date()).await.unwrap(),
            [Prayer::Asr]
        );

        let tomorrow = date().succ_opt().unwrap();
        assert!(
            prayed_on(&mut conn, user.id, tomorrow)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
