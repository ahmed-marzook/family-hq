//! Prayer reminders (which chats get them, which were sent) and the log
//! of who has prayed what. Settings are in `repo::settings`.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;
use tracing::warn;

use crate::prayer::checkin::{Logged, Write};
use crate::prayer::stats::Entry;
use crate::prayer::{Prayer, ReminderKind};
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
/// two times) can't be swapped by accident.
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
    // no row either and `fetch_optional` gives `None`.
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
    // row rather than crashing; the warning says which one, since that
    // would be a bug worth finding.
    Ok(row.and_then(|r| {
        let prayer = r
            .prayer
            .parse()
            .inspect_err(|err| warn!("reminder {wa_message_id} dropped: {err}"))
            .ok()?;
        Some((r.prayer_date, prayer))
    }))
}

/// One prayer's entry in the log, if it has one. The rules for changing
/// it are `prayer::checkin`'s; this only reads it.
pub async fn load(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
) -> Result<Option<Logged>, sqlx::Error> {
    let prayer = prayer.as_db_str();
    let row = sqlx::query!(
        r#"SELECT status, prayed_at AS "prayed_at: DateTime<Utc>",
                  missed_at AS "missed_at: DateTime<Utc>"
           FROM prayer_log WHERE user_id = ? AND prayer_date = ? AND prayer = ?"#,
        user_id,
        date,
        prayer
    )
    .fetch_optional(conn)
    .await?;
    // The CHECK allows only the three statuses, so the parse fails only if
    // the code and the schema disagree; unlike a report, a change can't just
    // skip the row, so that's an error.
    row.map(|r| {
        let status = r
            .status
            .parse()
            .map_err(|err: String| sqlx::Error::Decode(err.into()))?;
        Ok(Logged {
            status,
            prayed_at: r.prayed_at,
            missed_at: r.missed_at,
        })
    })
    .transpose()
}

/// Stores what `prayer::checkin::next` decided, whatever was there before:
/// the entry as given, or none at all.
pub async fn store(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    write: Write,
) -> Result<(), sqlx::Error> {
    let prayer = prayer.as_db_str();
    match write {
        Write::Store(logged) => {
            let status = logged.status.as_db_str();
            // Insert the row, or overwrite the one already there for this prayer.
            sqlx::query!(
                "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, prayed_at, missed_at)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (user_id, prayer_date, prayer) DO UPDATE
                 SET status = excluded.status,
                     prayed_at = excluded.prayed_at,
                     missed_at = excluded.missed_at",
                user_id,
                date,
                prayer,
                status,
                logged.prayed_at,
                logged.missed_at
            )
            .execute(conn)
            .await?;
        }
        Write::Delete => {
            sqlx::query!(
                "DELETE FROM prayer_log WHERE user_id = ? AND prayer_date = ? AND prayer = ?",
                user_id,
                date,
                prayer
            )
            .execute(conn)
            .await?;
        }
    }
    Ok(())
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
    Ok(names
        .iter()
        .filter_map(|n| {
            n.parse()
                .inspect_err(|err| {
                    warn!("prayer_log row of user {user_id} on {date} dropped: {err}")
                })
                .ok()
        })
        .collect())
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
    // As in `find_reminder`: a row the code can't read is dropped, with a
    // warning.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let read = || -> Result<Entry, String> {
                Ok(Entry {
                    date: r.prayer_date,
                    prayer: r.prayer.parse()?,
                    status: r.status.parse()?,
                })
            };
            read()
                .inspect_err(|err| {
                    warn!(
                        "prayer_log row of user {user_id} on {} dropped: {err}",
                        r.prayer_date
                    )
                })
                .ok()
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
    use crate::prayer::checkin;
    use crate::prayer::stats::Status;
    use crate::repo::{chats, settings};
    use crate::testkit::register;
    use chrono::TimeZone;

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

    async fn bilal(conn: &mut SqliteConnection) -> i64 {
        register(conn, "Bilal", "447700900001", Utc::now()).await.id
    }

    /// One prayer_log row, exactly as stored: `(status, prayed_at,
    /// missed_at)`. `None` is no row at all.
    type Row = Option<(&'static str, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>;

    /// Something that can happen to one prayer in the log, for the table
    /// below: each one is a way the code changes it.
    #[derive(Debug, Clone, Copy)]
    enum Happens {
        /// Its time ended unticked (the reminder loop).
        MarkMissed,
        /// A reaction, the push button, or `+prayed` with no timing.
        Tick,
        /// `+prayed asr on time`.
        OnTime,
        /// `+prayed asr late`.
        Late,
        /// The reaction was taken off.
        Untick,
        /// The admin dashboard sets it to exactly this, or (`None`) clears it.
        Set(Option<Status>),
    }

    /// Puts a row in place, whatever is there.
    async fn seed(conn: &mut SqliteConnection, user: i64, row: Row) {
        let prayer = Prayer::Asr.as_db_str();
        let day = date();
        sqlx::query!(
            "DELETE FROM prayer_log WHERE user_id = ? AND prayer_date = ? AND prayer = ?",
            user,
            day,
            prayer
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        if let Some((status, prayed_at, missed_at)) = row {
            sqlx::query!(
                "INSERT INTO prayer_log (user_id, prayer_date, prayer, status, prayed_at, missed_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
                user,
                day,
                prayer,
                status,
                prayed_at,
                missed_at
            )
            .execute(&mut *conn)
            .await
            .unwrap();
        }
    }

    /// The row as it's stored now.
    async fn stored(conn: &mut SqliteConnection, user: i64) -> Row {
        let prayer = Prayer::Asr.as_db_str();
        let day = date();
        let row = sqlx::query!(
            r#"SELECT status, prayed_at AS "prayed_at: DateTime<Utc>",
                      missed_at AS "missed_at: DateTime<Utc>"
               FROM prayer_log WHERE user_id = ? AND prayer_date = ? AND prayer = ?"#,
            user,
            day,
            prayer
        )
        .fetch_optional(conn)
        .await
        .unwrap()?;
        let status = ["prayed", "late", "missed"]
            .into_iter()
            .find(|s| *s == row.status)
            .expect("the CHECK allows only these");
        Some((status, row.prayed_at, row.missed_at))
    }

    /// Makes `happens` happen to Asr on `date()`, at `now`, the way the
    /// code does it. (Since the rules moved into `prayer::checkin`, that's
    /// its functions; the table below hasn't changed.)
    async fn fire(conn: &mut SqliteConnection, user: i64, happens: Happens, now: DateTime<Utc>) {
        let (day, asr) = (date(), Prayer::Asr);
        match happens {
            Happens::MarkMissed => {
                checkin::mark_missed(conn, user, day, asr, now)
                    .await
                    .unwrap();
            }
            Happens::Tick => {
                checkin::tick(conn, user, day, asr, None, now)
                    .await
                    .unwrap();
            }
            Happens::OnTime => {
                checkin::correct(conn, user, day, asr, checkin::Correction::OnTime, None, now)
                    .await
                    .unwrap()
            }
            Happens::Late => {
                checkin::correct(conn, user, day, asr, checkin::Correction::Late, None, now)
                    .await
                    .unwrap()
            }
            Happens::Untick => {
                checkin::untick(conn, user, day, asr, now).await.unwrap();
            }
            Happens::Set(status) => checkin::set_status(conn, user, day, asr, status, now)
                .await
                .unwrap(),
        }
    }

    /// Every way a prayer's log entry can change, from every state it can
    /// be in. (No other state can be stored: the table's CHECKs allow only
    /// these three shapes of row.) This pins the rules down, so moving them
    /// from SQL into Rust can be checked to change nothing.
    #[tokio::test]
    async fn every_change_from_every_state() {
        use Happens::*;
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let user = bilal(&mut conn).await;
        let at = |h| Some(Utc.with_ymd_and_hms(2026, 10, 27, h, 0, 0).unwrap());
        // When it was prayed or marked missed before (t0, m0), and now.
        let (t0, m0, now) = (at(15), at(17), at(20));
        let none: Row = None;
        let missed = Some(("missed", None, m0));
        let prayed = Some(("prayed", t0, None));
        let late = Some(("late", t0, m0));

        let rows: [(Row, Happens, Row); 36] = [
            // Nothing logged yet.
            (none, MarkMissed, Some(("missed", None, now))),
            (none, Tick, Some(("prayed", now, None))),
            (none, OnTime, Some(("prayed", now, None))),
            (none, Late, Some(("late", now, now))),
            (none, Untick, none),
            (none, Set(Some(Status::Prayed)), Some(("prayed", now, None))),
            (none, Set(Some(Status::Late)), Some(("late", now, now))),
            (none, Set(Some(Status::Missed)), Some(("missed", None, now))),
            (none, Set(None), none),
            // Marked missed.
            (missed, MarkMissed, missed),
            (missed, Tick, Some(("late", now, m0))),
            (missed, OnTime, Some(("prayed", now, None))),
            (missed, Late, Some(("late", now, m0))),
            (missed, Untick, missed),
            (
                missed,
                Set(Some(Status::Prayed)),
                Some(("prayed", now, None)),
            ),
            (missed, Set(Some(Status::Late)), Some(("late", now, m0))),
            (missed, Set(Some(Status::Missed)), missed),
            (missed, Set(None), none),
            // Prayed on time.
            (prayed, MarkMissed, prayed),
            (prayed, Tick, prayed),
            (prayed, OnTime, prayed),
            (prayed, Late, Some(("late", t0, now))),
            (prayed, Untick, none),
            (prayed, Set(Some(Status::Prayed)), prayed),
            (prayed, Set(Some(Status::Late)), Some(("late", t0, now))),
            (
                prayed,
                Set(Some(Status::Missed)),
                Some(("missed", None, now)),
            ),
            (prayed, Set(None), none),
            // Made up.
            (late, MarkMissed, late),
            (late, Tick, late),
            (late, OnTime, Some(("prayed", t0, None))),
            (late, Late, late),
            (late, Untick, missed),
            (late, Set(Some(Status::Prayed)), Some(("prayed", t0, None))),
            (late, Set(Some(Status::Late)), late),
            (late, Set(Some(Status::Missed)), missed),
            (late, Set(None), none),
        ];
        for (before, happens, after) in rows {
            seed(&mut conn, user, before).await;
            fire(&mut conn, user, happens, now.unwrap()).await;
            assert_eq!(
                stored(&mut conn, user).await,
                after,
                "{happens:?} from {before:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_log_covers_the_days_asked_for() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = bilal(&mut conn).await;
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap();
        for d in [25, 26, 27, 28] {
            checkin::tick(&mut conn, user, day(d), Prayer::Isha, None, now)
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
