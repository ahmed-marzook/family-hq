//! Every change to one prayer's entry in the log, and the rules for it.
//!
//! Ticking a prayer off is the one way every path does it: a reaction on
//! a reminder (`routes::webhook`), the button on a push (`routes::actions`)
//! and `+prayed` (`prayer::commands`) all call `tick`, so they always
//! record the same thing. What each path then *counts* differs (a reaction
//! is logged under its chat, a button under the person's DM, a command by
//! the webhook), so that stays with each caller.
//!
//! The rules themselves are one pure function, `next`: given what's
//! stored and what happened, what to store instead. The database part
//! (`apply`) only loads the entry, asks `next`, and stores the answer.
//!
//! Why a function of (state, event) → new state: it's the shape of a
//! *state machine*, which is what the planned `PrayerStage` (Stage 2.4:
//! `NotStarted → Started → Warned → Done | Missed`) will be too. That
//! one will decide *when* things happen (on the reminder loop's clock);
//! when a prayer ends up done or missed, it feeds `Event::Ticked` or
//! `Event::Missed` in here, and new kinds of event become new `Event`
//! variants with their own rows in the table. And being pure, every rule
//! is tested without a database, from every state.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;
use tracing::info;

use crate::prayer::stats::Status;
use crate::prayer::{Prayer, Tick};
use crate::repo::prayers;

/// One prayer's entry in the log, as stored (a `prayer_log` row). The
/// table's CHECKs allow only three shapes, which the constructors below
/// make; `next` only ever builds those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logged {
    pub status: Status,
    /// When they checked in. `None` exactly when it's missed.
    pub prayed_at: Option<DateTime<Utc>>,
    /// When it was marked missed. `None` exactly when it was prayed on
    /// time (a prayer made up that was never marked missed gets the time
    /// it was said to be late).
    pub missed_at: Option<DateTime<Utc>>,
}

/// Something that happens to one prayer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Its time ended with no check-in (the reminder loop).
    Missed,
    /// Ticked off: a reaction on its reminder, the push button, or
    /// `+prayed` with no timing. On time, unless it was already marked
    /// missed: then it's made up.
    Ticked,
    /// The tick was taken back (the reaction was removed).
    Unticked,
    /// The person says how it went (`+prayed asr on time` / `late`),
    /// whatever was logged before.
    Corrected { on_time: bool },
    /// The admin dashboard sets it to exactly this, or (`None`) clears it.
    Set(Option<Status>),
}

/// What to do to the stored entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// Store this, in place of whatever is there.
    Store(Logged),
    /// Take the entry out of the log.
    Delete,
}

/// The rules: what `event` at `now` does to an entry that's `current`
/// (`None`: nothing logged). `None` back means nothing changes, so the
/// caller can tell a real change from a repeat (a second reaction, a
/// retried webhook).
///
/// | event         | nothing logged     | missed (at m)     | prayed (at t)     | made up (t, m) |
/// |---------------|--------------------|-------------------|-------------------|----------------|
/// | `Missed`      | missed (now)       | –                 | –                 | –              |
/// | `Ticked`      | prayed (now)       | made up (now, m)  | –                 | –              |
/// | `Unticked`    | –                  | –                 | deleted           | missed (m)     |
/// | on time       | prayed (now)       | prayed (now)      | –                 | prayed (t)     |
/// | late          | made up (now, now) | made up (now, m)  | made up (t, now)  | –              |
/// | `Set(Missed)` | missed (now)       | –                 | missed (now)      | missed (m)     |
/// | `Set(None)`   | –                  | deleted           | deleted           | deleted        |
///
/// (– is no change. `Set(Prayed)` and `Set(Late)` are "on time" and
/// "late".) An earlier time is always kept rather than replaced.
pub fn next(current: Option<Logged>, event: Event, now: DateTime<Utc>) -> Option<Write> {
    // The earlier time if there is one, otherwise now.
    let prayed_at = current.and_then(|c| c.prayed_at).unwrap_or(now);
    let missed_at = current.and_then(|c| c.missed_at).unwrap_or(now);
    let status = current.map(|c| c.status);

    let write = match (event, status) {
        // Only a prayer with nothing logged is marked missed: one already
        // ticked (or already marked) is left alone.
        (Event::Missed, None) => Write::Store(Logged::missed(now)),
        (Event::Missed, Some(_)) => return None,

        (Event::Ticked, None) => Write::Store(Logged::prayed(now)),
        // Ticked after it was marked missed: made up, now.
        (Event::Ticked, Some(Status::Missed)) => Write::Store(Logged::made_up(now, missed_at)),
        (Event::Ticked, Some(Status::Prayed | Status::Late)) => return None,

        // A made-up prayer goes back to missed; one prayed on time had no
        // entry before, so it's taken out. (If its time has ended, the
        // reminder loop then marks it missed again.)
        (Event::Unticked, Some(Status::Late)) => Write::Store(Logged::missed(missed_at)),
        (Event::Unticked, Some(Status::Prayed)) => Write::Delete,
        (Event::Unticked, None | Some(Status::Missed)) => return None,

        (Event::Corrected { on_time: true } | Event::Set(Some(Status::Prayed)), _) => {
            Write::Store(Logged::prayed(prayed_at))
        }
        (Event::Corrected { on_time: false } | Event::Set(Some(Status::Late)), _) => {
            Write::Store(Logged::made_up(prayed_at, missed_at))
        }
        (Event::Set(Some(Status::Missed)), _) => Write::Store(Logged::missed(missed_at)),
        (Event::Set(None), Some(_)) => Write::Delete,
        (Event::Set(None), None) => return None,
    };
    // Storing exactly what's there already changes nothing.
    match (write, current) {
        (Write::Store(new), Some(old)) if new == old => None,
        _ => Some(write),
    }
}

impl Logged {
    /// Prayed on time, checked in at `at`.
    fn prayed(at: DateTime<Utc>) -> Self {
        Logged {
            status: Status::Prayed,
            prayed_at: Some(at),
            missed_at: None,
        }
    }

    /// Made up: checked in at `prayed_at`, after being marked missed (or
    /// said to be late) at `missed_at`.
    fn made_up(prayed_at: DateTime<Utc>, missed_at: DateTime<Utc>) -> Self {
        Logged {
            status: Status::Late,
            prayed_at: Some(prayed_at),
            missed_at: Some(missed_at),
        }
    }

    /// Missed, marked so at `at`.
    fn missed(at: DateTime<Utc>) -> Self {
        Logged {
            status: Status::Missed,
            prayed_at: None,
            missed_at: Some(at),
        }
    }
}

/// Makes `event` happen to `user_id`'s `prayer` on `date`: loads the entry,
/// works out the new one (`next`), and stores it. Returns what was
/// written, or `None` if nothing changed.
///
/// Reading then writing is two statements, so another writer could change
/// the entry in between. `conn` must therefore be inside a write
/// transaction (`BEGIN IMMEDIATE`, which takes SQLite's write lock at the
/// start, so nobody else can write until it ends). The webhook, the push
/// button and `+prayed` already run in one; the reminder loop and the
/// dashboard open one around their changes.
async fn apply(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    event: Event,
    now: DateTime<Utc>,
) -> Result<Option<Write>, sqlx::Error> {
    let current = prayers::load(conn, user_id, date, prayer).await?;
    let write = next(current, event, now);
    if let Some(write) = write {
        prayers::store(conn, user_id, date, prayer, write).await?;
    }
    Ok(write)
}

/// Marks `prayer` on `date` as prayed by `user_id`: on time, or made up if
/// its time had ended and it was marked missed. Returns what changed, or
/// `None` if it was already ticked (a second reaction, a retried webhook,
/// the button tapped twice), so the caller needn't say so twice.
pub async fn tick(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<Option<Tick>, sqlx::Error> {
    let write = apply(conn, user_id, date, prayer, Event::Ticked, now).await?;
    info!("prayed {prayer} on {date}");
    // Its time had ended and it was marked missed: made up.
    Ok(write.map(|w| match w {
        Write::Store(Logged {
            status: Status::Late,
            ..
        }) => Tick::MadeUp,
        _ => Tick::Prayed,
    }))
}

/// Takes a tick back (the reaction was removed). Returns `None` if there
/// was nothing to take back.
pub async fn untick(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<Option<Tick>, sqlx::Error> {
    let write = apply(conn, user_id, date, prayer, Event::Unticked, now).await?;
    info!("unticked {prayer} on {date}");
    Ok(write.map(|_| Tick::Unticked))
}

/// Sets how a prayer went when the person says so themselves (`+prayed
/// asr on time` / `late`): on time replaces a ❌ or a 🕐, late replaces
/// a ✅ or a ❌. Unlike `tick`, this overrides what's there.
pub async fn correct(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    on_time: bool,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    apply(
        conn,
        user_id,
        date,
        prayer,
        Event::Corrected { on_time },
        now,
    )
    .await?;
    Ok(())
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
    let write = apply(conn, user_id, date, prayer, Event::Missed, now).await?;
    Ok(write.is_some())
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
    apply(conn, user_id, date, prayer, Event::Set(status), now).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::prayer::stats::Entry;
    use crate::repo::prayers::{log_between, prayed_on};
    use crate::testkit::register;
    use chrono::TimeZone;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 27).unwrap()
    }

    async fn bilal(conn: &mut SqliteConnection) -> i64 {
        register(conn, "Bilal", "447700900001", Utc::now()).await.id
    }

    // ---- with a database ----

    #[tokio::test]
    async fn mark_and_unmark_prayed() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = register(&mut conn, "Bilal", "447700900001", now).await;

        let first = tick(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        let again = tick(&mut conn, user.id, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        assert_eq!(first, Some(Tick::Prayed));
        assert_eq!(again, None, "only the first one changes anything");
        tick(&mut conn, user.id, date(), Prayer::Asr, now)
            .await
            .unwrap();
        let mut prayed = prayed_on(&mut conn, user.id, date()).await.unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Asr, Prayer::Fajr]);

        assert!(
            untick(&mut conn, user.id, date(), Prayer::Fajr, now)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            !untick(&mut conn, user.id, date(), Prayer::Fajr, now)
                .await
                .unwrap()
                .is_some(),
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
        let late = tick(&mut conn, user, date(), Prayer::Asr, now).await;
        assert_eq!(late.unwrap(), Some(Tick::MadeUp));
        let again = tick(&mut conn, user, date(), Prayer::Asr, now).await;
        assert_eq!(again.unwrap(), None, "already made up");
        assert_eq!(log(&mut conn).await, [asr(Status::Late)]);
        assert_eq!(
            prayed_on(&mut conn, user, date()).await.unwrap(),
            [Prayer::Asr]
        );

        // Removing the reaction puts the ❌ back, rather than deleting it.
        assert!(
            untick(&mut conn, user, date(), Prayer::Asr, now)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(log(&mut conn).await, [asr(Status::Missed)]);
        assert!(
            !untick(&mut conn, user, date(), Prayer::Asr, now)
                .await
                .unwrap()
                .is_some(),
            "a missed prayer has nothing to untick"
        );

        // "I prayed it on time, I just forgot to tick it": ❌ → ✅.
        correct(&mut conn, user, date(), Prayer::Asr, true, now)
            .await
            .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Prayed)]);
        // And the other way: ✅ → 🕐.
        correct(&mut conn, user, date(), Prayer::Asr, false, now)
            .await
            .unwrap();
        assert_eq!(log(&mut conn).await, [asr(Status::Late)]);
        // With no row yet, either works too.
        correct(&mut conn, user, date(), Prayer::Isha, false, now)
            .await
            .unwrap();
        untick(&mut conn, user, date(), Prayer::Asr, now)
            .await
            .unwrap();
        untick(&mut conn, user, date(), Prayer::Isha, now)
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
        tick(&mut conn, user, date(), Prayer::Fajr, now)
            .await
            .unwrap();
        assert!(
            !mark_missed(&mut conn, user, date(), Prayer::Fajr, now)
                .await
                .unwrap()
        );
    }

    // ---- the rules alone ----

    /// The same table as `repo::prayers`'s `every_change_from_every_state`
    /// (which checks the database ends up so), here for the pure rules.
    #[test]
    fn every_change_from_every_state() {
        use Event::*;
        let at = |h| Utc.with_ymd_and_hms(2026, 10, 27, h, 0, 0).unwrap();
        // When it was prayed or marked missed before (t0, m0), and now.
        let (t0, m0, now) = (at(15), at(17), at(20));
        let row = |status, prayed_at, missed_at| {
            Some(Logged {
                status,
                prayed_at,
                missed_at,
            })
        };
        let none = None;
        let missed = row(Status::Missed, None, Some(m0));
        let prayed = row(Status::Prayed, Some(t0), None);
        let late = row(Status::Late, Some(t0), Some(m0));
        let (on_time, made_up) = (Corrected { on_time: true }, Corrected { on_time: false });

        let rows: [(Option<Logged>, Event, Option<Logged>); 36] = [
            // Nothing logged yet.
            (none, Missed, row(Status::Missed, None, Some(now))),
            (none, Ticked, row(Status::Prayed, Some(now), None)),
            (none, on_time, row(Status::Prayed, Some(now), None)),
            (none, made_up, row(Status::Late, Some(now), Some(now))),
            (none, Unticked, none),
            (
                none,
                Set(Some(Status::Prayed)),
                row(Status::Prayed, Some(now), None),
            ),
            (
                none,
                Set(Some(Status::Late)),
                row(Status::Late, Some(now), Some(now)),
            ),
            (
                none,
                Set(Some(Status::Missed)),
                row(Status::Missed, None, Some(now)),
            ),
            (none, Set(None), none),
            // Marked missed.
            (missed, Missed, missed),
            (missed, Ticked, row(Status::Late, Some(now), Some(m0))),
            (missed, on_time, row(Status::Prayed, Some(now), None)),
            (missed, made_up, row(Status::Late, Some(now), Some(m0))),
            (missed, Unticked, missed),
            (
                missed,
                Set(Some(Status::Prayed)),
                row(Status::Prayed, Some(now), None),
            ),
            (
                missed,
                Set(Some(Status::Late)),
                row(Status::Late, Some(now), Some(m0)),
            ),
            (missed, Set(Some(Status::Missed)), missed),
            (missed, Set(None), none),
            // Prayed on time.
            (prayed, Missed, prayed),
            (prayed, Ticked, prayed),
            (prayed, on_time, prayed),
            (prayed, made_up, row(Status::Late, Some(t0), Some(now))),
            (prayed, Unticked, none),
            (prayed, Set(Some(Status::Prayed)), prayed),
            (
                prayed,
                Set(Some(Status::Late)),
                row(Status::Late, Some(t0), Some(now)),
            ),
            (
                prayed,
                Set(Some(Status::Missed)),
                row(Status::Missed, None, Some(now)),
            ),
            (prayed, Set(None), none),
            // Made up.
            (late, Missed, late),
            (late, Ticked, late),
            (late, on_time, row(Status::Prayed, Some(t0), None)),
            (late, made_up, late),
            (late, Unticked, missed),
            (
                late,
                Set(Some(Status::Prayed)),
                row(Status::Prayed, Some(t0), None),
            ),
            (late, Set(Some(Status::Late)), late),
            (late, Set(Some(Status::Missed)), missed),
            (late, Set(None), none),
        ];
        for (before, event, after) in rows {
            // What's stored afterwards: the write, or (`None`) unchanged.
            let stored = match next(before, event, now) {
                Some(Write::Store(new)) => Some(new),
                Some(Write::Delete) => None,
                None => before,
            };
            assert_eq!(stored, after, "{event:?} from {before:?}");
        }
    }

    /// `None` means "nothing changed", which the callers rely on to reply
    /// only once. So a repeat is `None`, never a write of the same thing.
    #[test]
    fn a_repeat_changes_nothing() {
        let now = Utc.with_ymd_and_hms(2026, 10, 27, 20, 0, 0).unwrap();
        let prayed = Some(Logged::prayed(now));
        assert_eq!(next(prayed, Event::Ticked, now), None);
        assert_eq!(next(prayed, Event::Corrected { on_time: true }, now), None);
        assert_eq!(next(None, Event::Unticked, now), None);
        assert_eq!(next(None, Event::Set(None), now), None);
        assert_eq!(
            next(None, Event::Ticked, now),
            Some(Write::Store(Logged::prayed(now)))
        );
    }
}
