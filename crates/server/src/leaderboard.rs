//! Standings by rating. Only accounts with at least one ranked game count: a fresh account sits
//! at the starting rating without having earned it, and casual rounds never touch the win / loss
//! counts, so `wins + losses > 0` means "has played ranked". Ties are broken by wins, then by
//! account age (older first), so every player has one definite place.

use crate::api::ApiError;
use serde::Serialize;
use sqlx::{MySqlPool, Row};

/// One line of the leaderboard.
#[derive(Serialize, Debug)]
pub struct Entry {
    /// 1 for the top player.
    pub rank: u32,
    pub username: String,
    pub elo: i32,
    pub wins: i32,
    pub losses: i32,
}

/// One page of the standings.
#[derive(Serialize, Debug)]
pub struct Page {
    pub players: Vec<Entry>,
    /// This page, counted from 1, and how many there are (at least 1, even with nobody ranked).
    pub page: u32,
    pub pages: u32,
    /// Ranked players in all.
    pub total: u32,
}

/// The players on page `page` (from 1; out-of-range pages are clamped), `per_page` to a page.
pub async fn page(db: &MySqlPool, page: u32, per_page: u32) -> Result<Page, ApiError> {
    let total: i64 = sqlx::query("SELECT COUNT(*) AS n FROM accounts WHERE verified_at IS NOT NULL AND wins + losses > 0").fetch_one(db).await?.try_get("n")?;
    let total = total as u32;
    let pages = total.div_ceil(per_page).max(1);
    let page = page.clamp(1, pages);
    let offset = (page - 1) * per_page;
    let rows = sqlx::query("SELECT username, elo, wins, losses FROM accounts WHERE verified_at IS NOT NULL AND wins + losses > 0 ORDER BY elo DESC, wins DESC, id ASC LIMIT ? OFFSET ?")
        .bind(per_page)
        .bind(offset)
        .fetch_all(db)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        out.push(Entry {
            rank: offset + i as u32 + 1,
            username: row.try_get("username")?,
            elo: row.try_get("elo")?,
            wins: row.try_get("wins")?,
            losses: row.try_get("losses")?,
        });
    }
    Ok(Page { players: out, page, pages, total })
}

/// Where one account stands: 1 + the number of players ordered before it. `None` until it has
/// played a ranked game.
pub async fn rank(db: &MySqlPool, account_id: u64) -> Result<Option<u32>, ApiError> {
    let me = sqlx::query("SELECT elo, wins, losses FROM accounts WHERE id = ?").bind(account_id).fetch_one(db).await?;
    let (elo, wins, losses): (i32, i32, i32) = (me.try_get("elo")?, me.try_get("wins")?, me.try_get("losses")?);
    if wins + losses == 0 {
        return Ok(None);
    }
    let ahead: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM accounts WHERE verified_at IS NOT NULL AND wins + losses > 0 \
         AND (elo > ? OR (elo = ? AND wins > ?) OR (elo = ? AND wins = ? AND id < ?))",
    )
    .bind(elo)
    .bind(elo)
    .bind(wins)
    .bind(elo)
    .bind(wins)
    .bind(account_id)
    .fetch_one(db)
    .await?
    .try_get("n")?;
    Ok(Some(ahead as u32 + 1))
}
