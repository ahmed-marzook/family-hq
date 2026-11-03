//! A daily copy of the database, keeping the newest `BACKUP_KEEP`.
//!
//! On the first check of each day (in the server's default timezone), the
//! whole database is copied into `BACKUP_DIR` as `family-hq-2026-11-03.db`,
//! and older copies beyond the newest `BACKUP_KEEP` are deleted. A server
//! that was down at midnight makes the day's copy when it starts. Off
//! unless `BACKUP_DIR` is set.
//!
//! Why not just back up `/data`: the live database is two files (the
//! database and its WAL) that change all the time, and a copy taken at the
//! wrong moment may not open. These copies are one file each and never
//! change once written, so Kopia (or anything else) can back up the folder
//! safely.
//!
//! The rules (file names, which copies to delete) are pure functions; the
//! SQL is `repo::backup`, and `check` runs from the reminder loop.

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::repo::backup as repo;

const PREFIX: &str = "family-hq-";
const SUFFIX: &str = ".db";
/// Added to the name while a copy is being written. It's renamed once it's
/// finished, so a crash halfway never leaves a file that looks like a
/// whole backup.
const PARTIAL: &str = ".partial";

/// The backup for `date`: `family-hq-2026-11-03.db`.
pub fn file_name(date: NaiveDate) -> String {
    format!("{PREFIX}{date}{SUFFIX}")
}

/// The date in a backup's name, or `None` if the name isn't a backup's
/// (including one still being written).
pub fn date_of(name: &str) -> Option<NaiveDate> {
    name.strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .parse()
        .ok()
}

/// Which of `names` to delete so only the newest `keep` backups remain
/// (always at least one). Anything that isn't a backup is left alone.
pub fn to_delete(names: &[String], keep: usize) -> Vec<String> {
    let mut backups: Vec<(NaiveDate, &String)> = names
        .iter()
        .filter_map(|name| Some((date_of(name)?, name)))
        .collect();
    // Newest first, so everything after the first `keep` goes. `Reverse`
    // flips the order a key sorts in.
    backups.sort_by_key(|&(date, _)| std::cmp::Reverse(date));
    backups
        .into_iter()
        .skip(keep.max(1))
        .map(|(_, name)| name.clone())
        .collect()
}

/// Makes today's backup if it isn't there yet. Runs every minute from the
/// reminder loop; most minutes it only finds the file already there.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    // `let ... else`: take the folder out of the `Option`, or, with no
    // folder set, stop here.
    let Some(dir) = &state.config.backup_dir else {
        return Ok(());
    };
    let today = state.config.defaults.today(now);
    let mut conn = state.db.acquire().await?;
    back_up(&mut conn, dir, today, state.config.backup_keep).await?;
    Ok(())
}

/// Writes `today`'s backup into `dir` unless it's already there, then
/// deletes the old ones. Returns whether it wrote one.
///
/// `tokio::fs` is `std::fs` for async code: each call runs on a separate
/// thread pool, so a slow disk doesn't hold up the webhooks.
pub async fn back_up(
    conn: &mut SqliteConnection,
    dir: &Path,
    today: NaiveDate,
    keep: usize,
) -> Result<bool, AppError> {
    let name = file_name(today);
    let path = dir.join(&name);
    // `try_exists`, unlike `exists`, reports a folder it can't read as an
    // error, rather than "no file", which would mean a new copy every
    // minute.
    if tokio::fs::try_exists(&path).await? {
        return Ok(false);
    }
    tokio::fs::create_dir_all(dir).await?;
    let partial = dir.join(format!("{name}{PARTIAL}"));
    // Left over from a crash: `VACUUM INTO` won't write over it.
    if tokio::fs::try_exists(&partial).await? {
        tokio::fs::remove_file(&partial).await?;
    }
    // `BACKUP_DIR` came from an environment variable, which is always
    // UTF-8, so `to_string_lossy` never actually loses anything here.
    repo::vacuum_into(conn, &partial.to_string_lossy()).await?;
    tokio::fs::rename(&partial, &path).await?;
    let size = tokio::fs::metadata(&path).await?.len();
    info!("wrote {} ({} KB)", path.display(), size.div_ceil(1024));
    prune(dir, keep).await?;
    Ok(true)
}

/// Deletes the backups in `dir` beyond the newest `keep`.
async fn prune(dir: &Path, keep: usize) -> Result<(), AppError> {
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        // A name that isn't UTF-8 can't be one of ours.
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    for name in to_delete(&names, keep) {
        tokio::fs::remove_file(dir.join(&name)).await?;
        info!("deleted {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::path::PathBuf;
    use std::str::FromStr;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(file_name(day(3)), "family-hq-2026-11-03.db");
        assert_eq!(date_of("family-hq-2026-11-03.db"), Some(day(3)));
    }

    #[test]
    fn other_files_are_not_backups() {
        assert_eq!(date_of("family-hq-2026-11-03.db.partial"), None);
        assert_eq!(date_of("family-hq.db"), None);
        assert_eq!(date_of("family-hq-2026-13-01.db"), None);
        assert_eq!(date_of("notes.txt"), None);
    }

    #[test]
    fn keeps_the_newest() {
        // Out of order on purpose: `read_dir` promises no order.
        let all = names(&[
            "family-hq-2026-11-02.db",
            "family-hq-2026-10-30.db",
            "family-hq-2026-11-03.db",
            "family-hq-2026-11-01.db",
        ]);
        assert_eq!(
            to_delete(&all, 2),
            names(&["family-hq-2026-11-01.db", "family-hq-2026-10-30.db"])
        );
        assert!(to_delete(&all, 4).is_empty());
        assert!(to_delete(&all, 10).is_empty());
    }

    #[test]
    fn keep_zero_still_keeps_one() {
        let all = names(&["family-hq-2026-11-02.db", "family-hq-2026-11-03.db"]);
        assert_eq!(to_delete(&all, 0), names(&["family-hq-2026-11-02.db"]));
    }

    #[test]
    fn other_files_are_never_deleted() {
        let all = names(&[
            "family-hq-2026-11-03.db",
            "family-hq-2026-11-02.db",
            "family-hq-2026-11-03.db.partial",
            "README.txt",
        ]);
        assert_eq!(to_delete(&all, 1), names(&["family-hq-2026-11-02.db"]));
    }

    /// A fresh, empty folder for one test.
    fn scratch_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("family-hq-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real database file in `dir`, opened the way the server opens it
    /// (WAL on). Not `test_pool`: SQLite opens `VACUUM INTO`'s file the
    /// same way as the database it copies, so from an in-memory database
    /// the "copy" would only be in memory too.
    async fn file_db(dir: &Path) -> sqlx::SqlitePool {
        let url = format!("sqlite:{}", dir.join("live.db").display());
        crate::db::connect(&url).await.unwrap()
    }

    #[tokio::test]
    async fn writes_one_copy_a_day_and_prunes() {
        let root = scratch_dir("backup");
        let pool = file_db(&root).await;
        let mut conn = pool.acquire().await.unwrap();
        let amina = crate::person::Person {
            name: "Amina".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        crate::repo::users::find_or_register(&mut conn, &amina, Utc::now())
            .await
            .unwrap();
        // Not created yet: `back_up` makes the folder.
        let dir = root.join("backups");

        for d in 1..=3 {
            assert!(back_up(&mut conn, &dir, day(d), 2).await.unwrap());
        }
        // The same day again: already there.
        assert!(!back_up(&mut conn, &dir, day(3), 2).await.unwrap());

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            names(&["family-hq-2026-11-02.db", "family-hq-2026-11-03.db"])
        );

        // The copy is a whole database that opens on its own.
        let copy = dir.join(file_name(day(3)));
        let options =
            SqliteConnectOptions::from_str(&format!("sqlite:{}", copy.display())).unwrap();
        let mut restored = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        let (name,): (String,) = sqlx::query_as("SELECT display_name FROM users")
            .fetch_one(&mut restored)
            .await
            .unwrap();
        assert_eq!(name, "Amina");
        restored.close().await.unwrap();

        drop(conn);
        pool.close().await;
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_leftover_partial_file_is_replaced() {
        let dir = scratch_dir("partial");
        let pool = file_db(&dir).await;
        let mut conn = pool.acquire().await.unwrap();
        let partial = dir.join(format!("{}{PARTIAL}", file_name(day(3))));
        std::fs::write(&partial, "half a database").unwrap();

        assert!(back_up(&mut conn, &dir, day(3), 7).await.unwrap());
        assert!(!partial.exists());
        assert!(dir.join(file_name(day(3))).exists());

        drop(conn);
        pool.close().await;
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
