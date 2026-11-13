//! Saving the points something earned. Each feature works out what it's
//! worth with a pure function of its own (`points::for_todo` for a todo;
//! habits will have theirs) and hands the `Award` here, so the saving is
//! the same for all of them.

use chrono::NaiveDate;
use sqlx::SqliteConnection;

use crate::bot::Context;
use crate::error::AppError;
use crate::points::{Award, Source};
use crate::repo::points as stored_points;

/// Records `award` for the sender of `ctx`, earned in that chat on their
/// own `today`, for `source_id` (the todo's id, say). Returns the points,
/// for the reply.
///
/// A source awards once: if it already has, nothing changes (see
/// `repo::points::award`). No transaction of its own: the webhook runs
/// each command in one, so the thing closed and its points are saved
/// together or not at all.
pub async fn award(
    conn: &mut SqliteConnection,
    ctx: &Context,
    source: Source,
    source_id: i64,
    award: Award,
    today: NaiveDate,
) -> Result<i64, AppError> {
    award_to(conn, ctx, ctx.sender.id, source, source_id, award, today).await
}

/// `award`, for `user_id` rather than the sender: someone else's todo
/// cleared with `/todo clear` costs *them*. Still in `ctx`'s chat, at
/// `ctx.now`; `today` is `user_id`'s own local day.
pub async fn award_to(
    conn: &mut SqliteConnection,
    ctx: &Context,
    user_id: i64,
    source: Source,
    source_id: i64,
    award: Award,
    today: NaiveDate,
) -> Result<i64, AppError> {
    let (chat, now) = (ctx.chat_id, ctx.now);
    stored_points::award(conn, user_id, chat, source, source_id, award, today, now).await?;
    Ok(award.points)
}
