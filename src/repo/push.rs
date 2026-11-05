//! Who has push notifications on, and which pushes went out
//! (`crate::push`).

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::push::Feature;

/// Someone's ntfy topic (their phone subscribes to it) and the token the
/// buttons on their pushes send back.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub topic: String,
    pub token: String,
}

/// `user_id`'s subscription, if push is on for them.
pub async fn subscription(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<Subscription>, sqlx::Error> {
    sqlx::query_as!(
        Subscription,
        "SELECT topic, token FROM push_subscriptions WHERE user_id = ?",
        user_id
    )
    .fetch_optional(conn)
    .await
}

/// Turns push on for `user_id` with this new topic and token. If it was
/// already on, keeps the existing ones (their phone is subscribed to that
/// topic) and returns those instead.
pub async fn subscribe(
    conn: &mut SqliteConnection,
    user_id: i64,
    new: &Subscription,
    now: DateTime<Utc>,
) -> Result<Subscription, sqlx::Error> {
    sqlx::query!(
        "INSERT INTO push_subscriptions (user_id, topic, token, created_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id) DO NOTHING",
        user_id,
        new.topic,
        new.token,
        now
    )
    .execute(&mut *conn)
    .await?;
    // Just inserted or already there: either way there's a row now.
    Ok(subscription(conn, user_id)
        .await?
        .expect("a subscription row exists after the insert"))
}

/// Turns push off. Returns whether it was on. The topic and token are
/// forgotten, so turning it on again gives new ones.
pub async fn unsubscribe(conn: &mut SqliteConnection, user_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM push_subscriptions WHERE user_id = ?", user_id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Whose button this is: the user whose token it is.
pub async fn user_for_token(
    conn: &mut SqliteConnection,
    token: &str,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT user_id FROM push_subscriptions WHERE token = ?",
        token
    )
    .fetch_optional(conn)
    .await
}

/// Claims `feature`'s push `key` for `user_id`. `false` if they've had it
/// already (from another chat, or before a restart).
pub async fn claim(
    conn: &mut SqliteConnection,
    user_id: i64,
    feature: Feature,
    key: &str,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let feature = feature.as_db_str();
    let result = sqlx::query!(
        "INSERT INTO pushes_sent (user_id, feature, key, sent_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, feature, key) DO NOTHING",
        user_id,
        feature,
        key,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::repo::users;

    async fn someone(conn: &mut SqliteConnection) -> i64 {
        let person = Person {
            name: "Amina".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        users::find_or_register(conn, &person, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .id
    }

    fn sub(topic: &str, token: &str) -> Subscription {
        Subscription {
            topic: topic.repeat(24),
            token: token.repeat(32),
        }
    }

    #[tokio::test]
    async fn on_keeps_the_first_topic_and_off_forgets_it() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = someone(&mut conn).await;
        let now = Utc::now();

        assert_eq!(subscription(&mut conn, id).await.unwrap(), None);
        let first = subscribe(&mut conn, id, &sub("a", "b"), now).await.unwrap();
        assert_eq!(first, sub("a", "b"));
        // On again: still the first one, so their phone keeps working.
        let again = subscribe(&mut conn, id, &sub("c", "d"), now).await.unwrap();
        assert_eq!(again, first);
        assert_eq!(
            user_for_token(&mut conn, &first.token).await.unwrap(),
            Some(id)
        );

        assert!(unsubscribe(&mut conn, id).await.unwrap());
        assert!(!unsubscribe(&mut conn, id).await.unwrap());
        assert_eq!(user_for_token(&mut conn, &first.token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn short_secrets_are_refused() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = someone(&mut conn).await;
        let short = Subscription {
            topic: "guessable".to_string(),
            token: "x".repeat(32),
        };
        assert!(subscribe(&mut conn, id, &short, Utc::now()).await.is_err());
    }

    #[tokio::test]
    async fn each_push_is_claimed_once() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = someone(&mut conn).await;
        let now = Utc::now();
        let p = Feature::Prayers;
        assert!(
            claim(&mut conn, id, p, "2026-11-05 asr start", now)
                .await
                .unwrap()
        );
        // The same one again (say from a group as well as the DM): no.
        assert!(
            !claim(&mut conn, id, p, "2026-11-05 asr start", now)
                .await
                .unwrap()
        );
        // A different key is a different push.
        assert!(
            claim(&mut conn, id, p, "2026-11-05 asr ending", now)
                .await
                .unwrap()
        );
    }
}
