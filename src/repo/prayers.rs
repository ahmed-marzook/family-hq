//! Prayer reminders (which chats get them, which were sent) and the log
//! of who has prayed what. Settings are in `repo::settings`.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::prayer::{Prayer, ReminderKind};
use crate::prayer_stats::{Entry, Status};
use crate::reports::Delivery;
use crate::settings::Overrides;

/// A chat that has asked for prayer reminders, with its own settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ReminderChat {
    pub id: i64,
    pub wa_chat_id: String,
    /// Whose DM this is; `None` for a group.
    pub dm_user_id: Option<i64>,
    /// Where its prayer reports go.
    pub delivery: Delivery,
    pub overrides: Overrides,
}

/// Every chat with prayer reminders on, with its settings.
pub async fn reminder_chats(conn: &mut SqliteConnection) -> Result<Vec<ReminderChat>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT c.id, c.wa_chat_id, c.dm_user_id, s.timezone, s.city, s.country,
                  s.latitude, s.longitude, s.prayer_method, s.prayer_school, s.prayer_ending_minutes,
                  r.delivery AS "delivery?"
           FROM chat_settings s
           JOIN chats c ON c.id = s.chat_id
           LEFT JOIN report_settings r ON r.chat_id = c.id AND r.feature = 'prayers'
           WHERE s.prayer_reminders = 1
           ORDER BY c.id"#
    )
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ReminderChat {
            id: r.id,
            wa_chat_id: r.wa_chat_id,
            dm_user_id: r.dm_user_id,
            // No row (or, if the code and the schema disagree, a value it
            // can't read) means the default, `Here`.
            delivery: r.delivery.and_then(|d| d.parse().ok()).unwrap_or_default(),
            overrides: Overrides {
                timezone: r.timezone,
                city: r.city,
                country: r.country,
                latitude: r.latitude,
                longitude: r.longitude,
                prayer_method: r.prayer_method,
                prayer_school: r.prayer_school,
                prayer_ending_minutes: r.prayer_ending_minutes,
            },
        })
        .collect())
}

/// One reminder to claim. A struct rather than eight separate arguments,
/// so each value is named where it's built and two of the same type (the
/// two times) can't be swapped by accident. `'a`: `place` is borrowed, so
/// the struct can't outlive the string it points to.
#[derive(Debug, Clone)]
pub struct NewReminder<'a> {
    pub chat_id: i64,
    /// The day, in the place's timezone.
    pub date: NaiveDate,
    pub prayer: Prayer,
    pub kind: ReminderKind,
    /// Which members' times this is for (`Location::key`); '' for a DM.
    pub place: &'a str,
    /// When the prayer starts.
    pub prayer_time: DateTime<Utc>,
}

/// Claims the right to send one reminder. Returns the new row's id, or
/// `None` if this chat already got (or is getting) this reminder (this
/// prayer, this kind, this place) today. Insert-first means two checks can
/// never both send it.
pub async fn claim_reminder(
    conn: &mut SqliteConnection,
    reminder: &NewReminder<'_>,
    now: DateTime<Utc>,
) -> Result<Option<i64>, sqlx::Error> {
    let prayer = reminder.prayer.as_db_str();
    let kind = reminder.kind.as_db_str();
    // On a duplicate, `DO NOTHING` inserts no row, so `RETURNING` returns
    // no row either and `fetch_optional` gives `None`. (`AS "id!"`: the id
    // itself is never NULL, which the macro can't work out alone.)
    sqlx::query_scalar!(
        r#"INSERT INTO prayer_reminders (chat_id, prayer_date, prayer, kind, place, prayer_time, sent_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (chat_id, prayer_date, prayer, kind, place) DO NOTHING
           RETURNING id AS "id!""#,
        reminder.chat_id,
        reminder.date,
        prayer,
        kind,
        reminder.place,
        reminder.prayer_time,
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

/// Marks a prayer as prayed. If the bot had already marked it missed, it
/// becomes made up (`Late`). Returns what it is now, or `None` if nothing
/// changed: it was already marked (a second reaction, or a retried
/// webhook), so the caller needn't say so twice.
pub async fn mark_prayed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<Option<Status>, sqlx::Error> {
    let prayer = prayer.as_db_str();
    // An "upsert" with a condition: insert a new 'prayed' row, or turn a
    // 'missed' one into 'late'. Any other existing row fails the `WHERE`,
    // so nothing changes and `RETURNING` gives no row.
    let status = sqlx::query_scalar!(
        "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, prayed_at)
         VALUES (?, ?, ?, 'prayed', ?)
         ON CONFLICT (user_id, prayer_date, prayer) DO UPDATE
         SET status = 'late', prayed_at = excluded.prayed_at
         WHERE prayer_log.status = 'missed'
         RETURNING status",
        user_id,
        date,
        prayer,
        now
    )
    .fetch_optional(conn)
    .await?;
    Ok(status.and_then(|s| s.parse().ok()))
}

/// Sets how a prayer went when the person says so themselves (`+prayed
/// asr on time` / `late`): `Prayed` replaces a ❌ or a 🕐, `Late` replaces
/// a ✅ or a ❌. Unlike `mark_prayed`, this overrides what's there.
pub async fn set_prayed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    on_time: bool,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let prayer = prayer.as_db_str();
    let status = if on_time {
        Status::Prayed
    } else {
        Status::Late
    };
    let status = status.as_db_str();
    // The CHECKs want `missed_at` NULL exactly when it's 'prayed'. A
    // prayer made up that the bot never marked missed gets `now`.
    let missed_at = (!on_time).then_some(now);
    // `COALESCE(a, b)` is the first that isn't NULL: an earlier time is
    // kept rather than replaced.
    sqlx::query!(
        "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, prayed_at, missed_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id, prayer_date, prayer) DO UPDATE
         SET status = excluded.status,
             prayed_at = COALESCE(prayer_log.prayed_at, excluded.prayed_at),
             missed_at = CASE WHEN excluded.missed_at IS NULL THEN NULL
                              ELSE COALESCE(prayer_log.missed_at, excluded.missed_at) END",
        user_id,
        date,
        prayer,
        status,
        now,
        missed_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Sets a prayer to exactly `status`, or (`None`) takes it out of the log,
/// whatever it was before. For the admin dashboard's corrections.
pub async fn set_status(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    status: Option<Status>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    match status {
        Some(Status::Prayed) => set_prayed(conn, user_id, date, prayer, true, now).await,
        Some(Status::Late) => set_prayed(conn, user_id, date, prayer, false, now).await,
        Some(Status::Missed) => {
            let prayer = prayer.as_db_str();
            // Prayed or not, it becomes missed: no check-in time, and the
            // earliest time it was marked missed (or now).
            sqlx::query!(
                "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, missed_at)
                 VALUES (?, ?, ?, 'missed', ?)
                 ON CONFLICT (user_id, prayer_date, prayer) DO UPDATE
                 SET status = 'missed', prayed_at = NULL,
                     missed_at = COALESCE(prayer_log.missed_at, excluded.missed_at)",
                user_id,
                date,
                prayer,
                now
            )
            .execute(conn)
            .await?;
            Ok(())
        }
        None => {
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
    }
}

/// Undoes `mark_prayed` (the reaction was removed): a made-up prayer goes
/// back to missed, an on-time one is deleted. (If its time has already
/// ended, the reminder loop then marks it missed again.)
pub async fn unmark_prayed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
) -> Result<bool, sqlx::Error> {
    let prayer = prayer.as_db_str();
    // `&mut *conn` reborrows the connection for this query, so `conn`
    // itself is still ours for the next one.
    let back_to_missed = sqlx::query!(
        "UPDATE prayer_log SET status = 'missed', prayed_at = NULL
         WHERE user_id = ? AND prayer_date = ? AND prayer = ? AND status = 'late'",
        user_id,
        date,
        prayer
    )
    .execute(&mut *conn)
    .await?;
    let deleted = sqlx::query!(
        "DELETE FROM prayer_log
         WHERE user_id = ? AND prayer_date = ? AND prayer = ? AND status = 'prayed'",
        user_id,
        date,
        prayer
    )
    .execute(conn)
    .await?;
    // `true` if it was marked and now isn't.
    Ok(back_to_missed.rows_affected() + deleted.rows_affected() == 1)
}

/// Marks a prayer missed, unless it's already in the log (prayed, or
/// marked missed by an earlier check). Returns whether it was newly marked.
pub async fn mark_missed(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let prayer = prayer.as_db_str();
    let result = sqlx::query!(
        "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, missed_at)
         VALUES (?, ?, ?, 'missed', ?)
         ON CONFLICT (user_id, prayer_date, prayer) DO NOTHING",
        user_id,
        date,
        prayer,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The prayers `user_id` has prayed on `date`, on time or made up.
pub async fn prayed_on(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
) -> Result<Vec<Prayer>, sqlx::Error> {
    let names = sqlx::query_scalar!(
        "SELECT prayer FROM prayer_log
         WHERE user_id = ? AND prayer_date = ? AND status != 'missed'",
        user_id,
        date
    )
    .fetch_all(conn)
    .await?;
    Ok(names.iter().filter_map(|n| n.parse().ok()).collect())
}

/// Everything in `user_id`'s log from `first` to `last` (both included),
/// in date order.
pub async fn log_between(
    conn: &mut SqliteConnection,
    user_id: i64,
    first: NaiveDate,
    last: NaiveDate,
) -> Result<Vec<Entry>, sqlx::Error> {
    // Dates are 'YYYY-MM-DD' text, so comparing them as text compares
    // them as dates.
    let rows = sqlx::query!(
        r#"SELECT prayer_date AS "prayer_date: NaiveDate", prayer, status
           FROM prayer_log
           WHERE user_id = ? AND prayer_date BETWEEN ? AND ?
           ORDER BY prayer_date"#,
        user_id,
        first,
        last
    )
    .fetch_all(conn)
    .await?;
    // As in `find_reminder`: a row the code can't read is dropped.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Entry {
                date: r.prayer_date,
                prayer: r.prayer.parse().ok()?,
                status: r.status.parse().ok()?,
            })
        })
        .collect())
}

/// Whether this chat was sent any reminder for this prayer, for this
/// place. Only those count as missed: if the bot was down for the whole
/// time, nobody was told, and a ❌ would be unfair.
pub async fn was_reminded(
    conn: &mut SqliteConnection,
    chat_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    place: &str,
) -> Result<bool, sqlx::Error> {
    let prayer = prayer.as_db_str();
    // `EXISTS` is 1 or 0; `AS "x: bool"` reads it as a Rust bool.
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM prayer_reminders
               WHERE chat_id = ? AND prayer_date = ? AND prayer = ? AND place = ?
           ) AS "reminded: bool""#,
        chat_id,
        date,
        prayer,
        place
    )
    .fetch_one(conn)
    .await
}

/// Whether prayer reminders are on in this person's DM with the bot. Then
/// that DM gets its own prayer reports, and a group's sent privately would
/// be a copy.
pub async fn dm_reminders_on(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM chats c
               JOIN chat_settings s ON s.chat_id = c.id
               WHERE c.dm_user_id = ? AND c.left_at IS NULL AND s.prayer_reminders = 1
           ) AS "on: bool""#,
        user_id
    )
    .fetch_one(conn)
    .await
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
            &NewReminder {
                chat_id: chat,
                date: date(),
                prayer: Prayer::Asr,
                kind: ReminderKind::Start,
                place: "uk",
                prayer_time: now,
            },
            now,
        )
        .await
        .unwrap()
        .expect("first claim works");
        let again = claim_reminder(
            &mut conn,
            &NewReminder {
                chat_id: chat,
                date: date(),
                prayer: Prayer::Asr,
                kind: ReminderKind::Start,
                place: "uk",
                prayer_time: now,
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(again, None, "second claim is refused");
        let ending = claim_reminder(
            &mut conn,
            &NewReminder {
                chat_id: chat,
                date: date(),
                prayer: Prayer::Asr,
                kind: ReminderKind::Ending,
                place: "uk",
                prayer_time: now,
            },
            now,
        )
        .await
        .unwrap();
        assert!(ending.is_some(), "the other kind is a separate reminder");
        let elsewhere = claim_reminder(
            &mut conn,
            &NewReminder {
                chat_id: chat,
                date: date(),
                prayer: Prayer::Asr,
                kind: ReminderKind::Start,
                place: "lk",
                prayer_time: now,
            },
            now,
        )
        .await
        .unwrap();
        assert!(
            elsewhere.is_some(),
            "another place in the same group is separate"
        );

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

        let first = mark_prayed(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        let again = mark_prayed(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        assert_eq!(first, Some(Status::Prayed));
        assert_eq!(again, None, "only the first one changes anything");
        mark_prayed(&mut conn, user.id, date(), Prayer::Asr, now)
            .await
            .unwrap();
        let mut prayed = prayed_on(&mut conn, user.id, date()).await.unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Asr, Prayer::Fajr]);

        assert!(
            unmark_prayed(&mut conn, user.id, date(), Prayer::Fajr)
                .await
                .unwrap()
        );
        assert!(
            !unmark_prayed(&mut conn, user.id, date(), Prayer::Fajr)
                .await
                .unwrap(),
            "already gone"
        );
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

    async fn bilal(conn: &mut SqliteConnection) -> i64 {
        let person = Person {
            name: "Bilal".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        users::find_or_register(conn, &person, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_missed_prayer_can_be_made_up_and_unticked_back_to_missed() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = bilal(&mut conn).await;
        let log = async |conn: &mut SqliteConnection| {
            log_between(conn, user, date(), date()).await.unwrap()
        };
        let asr = |status| Entry {
            date: date(),
            prayer: Prayer::Asr,
            status,
        };

        assert!(
            mark_missed(&mut conn, user, date(), Prayer::Asr, now)
                .await
                .unwrap()
        );
        assert!(
            !mark_missed(&mut conn, user, date(), Prayer::Asr, now)
                .await
                .unwrap(),
            "already missed"
        );
        assert_eq!(log(&mut conn).await, [asr(Status::Missed)]);
        assert!(prayed_on(&mut conn, user, date()).await.unwrap().is_empty());

        // Prayed after its time: made up.
        let late = mark_prayed(&mut conn, user, date(), Prayer::Asr, now).await;
        assert_eq!(late.unwrap(), Some(Status::Late));
        let again = mark_prayed(&mut conn, user, date(), Prayer::Asr, now).await;
        assert_eq!(again.unwrap(), None, "already made up");
        assert_eq!(log(&mut conn).await, [asr(Status::Late)]);
        assert_eq!(
            prayed_on(&mut conn, user, date()).await.unwrap(),
            [Prayer::Asr]
        );

        // Removing the reaction puts the ❌ back, rather than deleting it.
        assert!(
            unmark_prayed(&mut conn, user, date(), Prayer::Asr)
                .await
                .unwrap()
        );
        assert_eq!(log(&mut conn).await, [asr(Status::Missed)]);
        assert!(
            !unmark_prayed(&mut conn, user, date(), Prayer::Asr)
                .await
                .unwrap(),
            "a missed prayer has nothing to untick"
        );

        // "I prayed it on time, I just forgot to tick it": ❌ → ✅.
        set_prayed(&mut conn, user, date(), Prayer::Asr, true, now)
            .await
            .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Prayed)]);
        // And the other way: ✅ → 🕐.
        set_prayed(&mut conn, user, date(), Prayer::Asr, false, now)
            .await
            .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Late)]);
        // With no row yet, either works too.
        set_prayed(&mut conn, user, date(), Prayer::Isha, false, now)
            .await
            .unwrap();
        unmark_prayed(&mut conn, user, date(), Prayer::Asr)
            .await
            .unwrap();
        unmark_prayed(&mut conn, user, date(), Prayer::Isha)
            .await
            .unwrap();

        // The dashboard can set anything, or clear it. (Isha is ❌ from
        // above; cleared first so the day holds only Asr.)
        set_status(&mut conn, user, date(), Prayer::Isha, None, now)
            .await
            .unwrap();
        set_status(
            &mut conn,
            user,
            date(),
            Prayer::Asr,
            Some(Status::Missed),
            now,
        )
        .await
        .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Missed)]);
        set_status(
            &mut conn,
            user,
            date(),
            Prayer::Asr,
            Some(Status::Prayed),
            now,
        )
        .await
        .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Prayed)]);
        set_status(
            &mut conn,
            user,
            date(),
            Prayer::Asr,
            Some(Status::Missed),
            now,
        )
        .await
        .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Missed)], "✅ → ❌");
        set_status(&mut conn, user, date(), Prayer::Asr, None, now)
            .await
            .unwrap();
        assert!(log(&mut conn).await.is_empty());

        // A prayer prayed in time is never marked missed afterwards.
        mark_prayed(&mut conn, user, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        assert!(
            !mark_missed(&mut conn, user, date(), Prayer::Fajr, now)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn the_log_covers_the_days_asked_for() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = bilal(&mut conn).await;
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap();
        for d in [25, 26, 27, 28] {
            mark_prayed(&mut conn, user, day(d), Prayer::Isha, now)
                .await
                .unwrap();
        }
        let log = log_between(&mut conn, user, day(26), day(27))
            .await
            .unwrap();
        let dates: Vec<NaiveDate> = log.iter().map(|e| e.date).collect();
        assert_eq!(dates, [day(26), day(27)]);
    }

    #[tokio::test]
    async fn reminded_prayers_are_per_chat_and_place() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let reminder = NewReminder {
            chat_id: chat,
            date: date(),
            prayer: Prayer::Asr,
            kind: ReminderKind::Ending,
            place: "uk",
            prayer_time: now,
        };
        claim_reminder(&mut conn, &reminder, now).await.unwrap();
        let reminded = async |conn: &mut SqliteConnection, prayer, place| {
            was_reminded(conn, chat, date(), prayer, place)
                .await
                .unwrap()
        };
        assert!(reminded(&mut conn, Prayer::Asr, "uk").await);
        assert!(
            !reminded(&mut conn, Prayer::Asr, "lk").await,
            "another place"
        );
        assert!(
            !reminded(&mut conn, Prayer::Fajr, "uk").await,
            "another prayer"
        );
    }
}
