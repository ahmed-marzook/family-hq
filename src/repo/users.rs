//! People who use the bot. Anyone who messages it is registered the first
//! time, named after their WhatsApp profile.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::person::Person;

/// A row from `users`, as much as the bot needs.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: i64,
    /// The name used in `+todo @name`.
    pub display_name: String,
    /// Phone-number ID, e.g. `447700900001@s.whatsapp.net`. Used to
    /// @mention them (a LID can't be mentioned, so it's never that).
    pub wa_jid: String,
}

/// Finds the user behind `sender`, registering them if they're new.
///
/// Returns `None` only if they're new and we have no phone-number ID for
/// them (just a LID), because `users.wa_jid` needs one.
pub async fn find_or_register(
    conn: &mut SqliteConnection,
    sender: &Person,
    now: DateTime<Utc>,
) -> Result<Option<User>, sqlx::Error> {
    if let Some(user) = find(conn, sender).await? {
        // We may be seeing their LID for the first time: remember it, so a
        // later message that only has the LID still finds them.
        if let Some(lid) = &sender.lid {
            sqlx::query!(
                "UPDATE users SET wa_lid = ? WHERE id = ? AND wa_lid IS NULL",
                lid,
                user.id
            )
            .execute(&mut *conn)
            .await?;
        }
        return Ok(Some(user));
    }

    if !sender.jid.ends_with("@s.whatsapp.net") {
        return Ok(None);
    }

    let phone = sender.phone_number();
    let mut display_name = base_display_name(&sender.name, phone);
    if name_taken(conn, &display_name).await? {
        display_name = with_phone_suffix(&display_name, phone);
    }

    // `RETURNING id` hands back the new row's id from the same statement.
    let id = sqlx::query_scalar!(
        "INSERT INTO users (display_name, wa_jid, wa_lid, created_at)
         VALUES (?, ?, ?, ?)
         RETURNING id",
        display_name,
        sender.jid,
        sender.lid,
        now
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(Some(User {
        id,
        display_name,
        wa_jid: sender.jid.clone(),
    }))
}

/// Looks a sender up by phone-number ID or LID. In a group, `from` may be
/// the LID itself, so it's compared against `wa_lid` too.
async fn find(conn: &mut SqliteConnection, sender: &Person) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as!(
        User,
        "SELECT id, display_name, wa_jid FROM users
         WHERE wa_jid = ?1 OR wa_lid = ?1 OR wa_lid = ?2",
        sender.jid,
        sender.lid
    )
    .fetch_optional(conn)
    .await
}

async fn name_taken(conn: &mut SqliteConnection, name: &str) -> Result<bool, sqlx::Error> {
    // `as "taken: bool"` tells the macro which Rust type to use for the
    // column; SQLite has no real boolean, `EXISTS` gives 0 or 1.
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM users WHERE display_name = ?) AS "taken: bool""#,
        name
    )
    .fetch_one(conn)
    .await
}

/// Finds someone in this chat by the name typed after `@`.
/// `display_name` is `COLLATE NOCASE`, so `@khadija` finds "Khadija".
pub async fn find_member_by_name(
    conn: &mut SqliteConnection,
    chat_id: i64,
    name: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as!(
        User,
        "SELECT u.id, u.display_name, u.wa_jid FROM users u
         JOIN chat_members m ON m.user_id = u.id
         WHERE m.chat_id = ? AND u.display_name = ?",
        chat_id,
        name
    )
    .fetch_optional(conn)
    .await
}

/// Everyone the bot has seen in this chat, for "who can I assign to?".
pub async fn member_names(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT u.display_name FROM users u
         JOIN chat_members m ON m.user_id = u.id
         WHERE m.chat_id = ?
         ORDER BY u.display_name",
        chat_id
    )
    .fetch_all(conn)
    .await
}

/// The name someone gets on registration: the first word of their profile
/// name, letters and digits only, so it can be typed after `@`.
/// "Bilal Rahman" → "Bilal", "😊Maryam❤️" → "Maryam". Falls back to
/// "User" + the end of their number if nothing usable is left.
fn base_display_name(profile_name: &str, phone: &str) -> String {
    let first_word = profile_name.split_whitespace().next().unwrap_or("");
    let cleaned: String = first_word.chars().filter(|c| c.is_alphanumeric()).collect();
    if cleaned.is_empty() {
        format!("User{}", last_digits(phone))
    } else {
        cleaned
    }
}

/// "Bilal" → "Bilal-0001" when another Bilal already exists.
fn with_phone_suffix(name: &str, phone: &str) -> String {
    format!("{name}-{}", last_digits(phone))
}

/// The last 4 characters of the number (all of it if shorter).
fn last_digits(phone: &str) -> &str {
    // `char_indices().nth_back(3)` finds where the 4th-last character
    // starts, so slicing never cuts a multi-byte character in half.
    match phone.char_indices().nth_back(3) {
        Some((start, _)) => &phone[start..],
        None => phone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    fn now() -> DateTime<Utc> {
        "2026-10-27T18:00:00Z".parse().unwrap()
    }

    fn person(name: &str, jid: &str, lid: Option<&str>) -> Person {
        Person {
            name: name.to_string(),
            jid: jid.to_string(),
            lid: lid.map(str::to_string),
        }
    }

    #[test]
    fn display_name_is_the_cleaned_first_word() {
        assert_eq!(base_display_name("Bilal Rahman", "447700900001"), "Bilal");
        assert_eq!(base_display_name("😊Maryam❤️", "447700900001"), "Maryam");
        assert_eq!(base_display_name("عائشة", "447700900001"), "عائشة");
        assert_eq!(base_display_name("😊", "447700900001"), "User0001");
        assert_eq!(base_display_name("", "447700900001"), "User0001");
    }

    #[test]
    fn suffix_uses_the_end_of_the_number() {
        assert_eq!(with_phone_suffix("Bilal", "447700900042"), "Bilal-0042");
        assert_eq!(with_phone_suffix("Bilal", "12"), "Bilal-12");
    }

    #[tokio::test]
    async fn registers_once_then_finds() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let bilal = person("Bilal Rahman", "447700900001@s.whatsapp.net", None);

        let first = find_or_register(&mut conn, &bilal, now())
            .await
            .unwrap()
            .unwrap();
        let again = find_or_register(&mut conn, &bilal, now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(first.display_name, "Bilal");
    }

    #[tokio::test]
    async fn learns_the_lid_and_finds_by_it_later() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let jid = "447700900001@s.whatsapp.net";

        let without_lid = person("Bilal", jid, None);
        let user = find_or_register(&mut conn, &without_lid, now())
            .await
            .unwrap()
            .unwrap();

        let with_lid = person("Bilal", jid, Some("111@lid"));
        find_or_register(&mut conn, &with_lid, now()).await.unwrap();

        // A group message where `from` is only the LID.
        let lid_only = person("Bilal", "111@lid", Some("111@lid"));
        let found = find_or_register(&mut conn, &lid_only, now()).await.unwrap();
        assert_eq!(found, Some(user));
    }

    #[tokio::test]
    async fn unknown_lid_only_sender_is_not_registered() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let lid_only = person("Bilal", "111@lid", Some("111@lid"));
        assert_eq!(
            find_or_register(&mut conn, &lid_only, now()).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn same_first_name_gets_a_suffix() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let first = person("Bilal A", "447700900001@s.whatsapp.net", None);
        let second = person("bilal B", "447700900002@s.whatsapp.net", None);

        find_or_register(&mut conn, &first, now()).await.unwrap();
        let user = find_or_register(&mut conn, &second, now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.display_name, "bilal-0002");
    }
}
