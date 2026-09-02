//! Match records: creation when the queue pairs two players, result reports from both clients,
//! settlement (Elo) and the history shown on profiles. Rounds played in private rooms are recorded
//! too (`casual`), for the history only: they are never rated and do not count as wins or losses.
//!
//! The game itself runs peer-to-peer, so the server only ever hears what the clients say. Each
//! player reports the final score; the match is settled when the two reports agree. A match with
//! a single report (the other player closed the game) is settled on that report after a grace
//! period by `sweep`, and contradicting reports void the match without touching anyone's rating.

use crate::api::ApiError;
use crate::elo;
use chrono::{NaiveDateTime, Utc};
use serde::Serialize;
use sqlx::{MySqlPool, Row};
use tracing::{info, warn};

/// How long a lone report waits for the other player's before it counts on its own. A report
/// that is coming at all arrives within seconds (it is sent as the match is torn down); the wait
/// is for a slow network, not a slow player.
const LONE_REPORT_GRACE_SECS: i64 = 15;
/// Matches nobody reported on are voided after this long.
const ABANDONED_SECS: i64 = 60 * 60;
/// A casual round reported by the second player this long after the first is paired with the
/// first report instead of making a row of its own.
const CASUAL_PAIR_SECS: i64 = 10 * 60;

/// Inserts a match between `a` and `b` and returns its id.
pub async fn create(db: &MySqlPool, room: &str, a: (u64, &str, i32), b: (u64, &str, i32)) -> Result<u64, ApiError> {
    let res = sqlx::query("INSERT INTO matches (room_code, player_a, player_b, name_a, name_b, elo_a, elo_b) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(room)
        .bind(a.0)
        .bind(b.0)
        .bind(a.1)
        .bind(b.1)
        .bind(a.2)
        .bind(b.2)
        .execute(db)
        .await?;
    Ok(res.last_insert_id())
}

/// A result as one player saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub score_a: i32,
    pub score_b: i32,
    /// 0 = a won, 1 = b won.
    pub winner: u8,
}

impl Report {
    fn encode(self) -> String {
        format!("{},{},{}", self.score_a, self.score_b, self.winner)
    }

    fn decode(s: &str) -> Option<Report> {
        let mut it = s.split(',');
        let score_a = it.next()?.parse().ok()?;
        let score_b = it.next()?.parse().ok()?;
        let winner = it.next()?.parse().ok()?;
        Some(Report { score_a, score_b, winner })
    }
}

/// Stores one player's report and settles the match if both agree.
///
/// The row stays locked while this report goes in and the other player's is read back. Both
/// clients see the deciding frag on the same tick and report within milliseconds of each other;
/// without the lock each request could read the row before the other's report had landed,
/// leaving a match with two reports that nobody settled.
pub async fn report(db: &MySqlPool, match_id: u64, reporter: u64, report: Report) -> Result<(), ApiError> {
    if report.winner > 1 || report.score_a < 0 || report.score_b < 0 {
        return Err(ApiError::Bad("malformed report".into()));
    }
    let mut tx = db.begin().await?;
    let row = sqlx::query("SELECT player_a, player_b, status, report_a, report_b FROM matches WHERE id = ? FOR UPDATE")
        .bind(match_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such match".into()))?;
    let (a, b): (u64, u64) = (row.try_get("player_a")?, row.try_get("player_b")?);
    let status: String = row.try_get("status")?;
    if status != "playing" {
        return Ok(()); // already settled: a late duplicate report
    }
    let (mine, theirs, update) = if reporter == a {
        ("report_a", "report_b", "UPDATE matches SET report_a = ?, report_a_at = UTC_TIMESTAMP() WHERE id = ?")
    } else if reporter == b {
        ("report_b", "report_a", "UPDATE matches SET report_b = ?, report_b_at = UTC_TIMESTAMP() WHERE id = ?")
    } else {
        return Err(ApiError::Forbidden("not your match".into()));
    };
    let already: Option<String> = row.try_get(mine)?;
    if already.is_none() {
        sqlx::query(update).bind(report.encode()).bind(match_id).execute(&mut *tx).await?;
    }
    let other: Option<String> = row.try_get(theirs)?;
    tx.commit().await?;
    match other.as_deref().and_then(Report::decode) {
        Some(other) if mine == "report_a" => resolve(db, match_id, report, other).await,
        Some(other) => resolve(db, match_id, other, report).await,
        None => Ok(()),
    }
}

/// Both reports are in: settle when they agree on the winner, void otherwise. Only the winner
/// affects the ratings, so scores that differ are tolerated: a forfeit is reported by the two
/// clients from their own rollback state, which can be a frame apart when someone scored as the
/// other left. The winner's score is the one recorded then.
async fn resolve(db: &MySqlPool, match_id: u64, report_a: Report, report_b: Report) -> Result<(), ApiError> {
    if report_a.winner != report_b.winner {
        warn!(match_id, "players disagree on the winner ({report_a:?} vs {report_b:?}); match voided");
        return void(db, match_id).await;
    }
    if report_a != report_b {
        info!(match_id, "reports differ on the score ({report_a:?} vs {report_b:?}); taking the winner's");
    }
    let r = if report_a.winner == 0 { report_a } else { report_b };
    settle(db, match_id, r).await
}

async fn void(db: &MySqlPool, match_id: u64) -> Result<(), ApiError> {
    sqlx::query("UPDATE matches SET status = 'void', finished_at = UTC_TIMESTAMP() WHERE id = ? AND status = 'playing'")
        .bind(match_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Applies the result: match row, both ratings and win/loss counts, in one transaction.
async fn settle(db: &MySqlPool, match_id: u64, r: Report) -> Result<(), ApiError> {
    let mut tx = db.begin().await?;
    let row = sqlx::query("SELECT player_a, player_b, status FROM matches WHERE id = ? FOR UPDATE").bind(match_id).fetch_one(&mut *tx).await?;
    let status: String = row.try_get("status")?;
    if status != "playing" {
        return Ok(());
    }
    let (a, b): (u64, u64) = (row.try_get("player_a")?, row.try_get("player_b")?);
    // Current ratings, not the ones copied at match time: a player may have finished another game since.
    let elo_a: i32 = sqlx::query("SELECT elo FROM accounts WHERE id = ? FOR UPDATE").bind(a).fetch_one(&mut *tx).await?.try_get("elo")?;
    let elo_b: i32 = sqlx::query("SELECT elo FROM accounts WHERE id = ? FOR UPDATE").bind(b).fetch_one(&mut *tx).await?.try_get("elo")?;
    let a_won = r.winner == 0;
    let (da, db_) = elo::deltas(elo_a, elo_b, a_won);
    sqlx::query(
        "UPDATE matches SET status = 'finished', score_a = ?, score_b = ?, winner = ?, delta_a = ?, delta_b = ?, elo_a = ?, elo_b = ?, finished_at = UTC_TIMESTAMP() WHERE id = ?",
    )
    .bind(r.score_a)
    .bind(r.score_b)
    .bind(r.winner as i8)
    .bind(da)
    .bind(db_)
    .bind(elo_a)
    .bind(elo_b)
    .bind(match_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE accounts SET elo = elo + ?, wins = wins + ?, losses = losses + ? WHERE id = ?")
        .bind(da)
        .bind(a_won as i32)
        .bind(!a_won as i32)
        .bind(a)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE accounts SET elo = elo + ?, wins = wins + ?, losses = losses + ? WHERE id = ?")
        .bind(db_)
        .bind(!a_won as i32)
        .bind(a_won as i32)
        .bind(b)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    info!(match_id, "settled {}-{}: {} {:+}, {} {:+}", r.score_a, r.score_b, a, da, b, db_);
    Ok(())
}

/// Records a round of a private room from the reporter's point of view. Rooms are anonymous to
/// the server, so this is the only thing it ever hears about them: the reporter's account, the
/// opponent's display name and the score. When the opponent has an account and reported the same
/// round first (mirrored names and score, same room), the reporter is attached to that row rather
/// than making a second one, so a round shows up once on each profile. Nothing else changes: no
/// rating, no win or loss count.
pub async fn casual(db: &MySqlPool, reporter: (u64, &str), room: &str, opponent: &str, score: [i32; 2], won: bool) -> Result<u64, ApiError> {
    if score[0] < 0 || score[1] < 0 || room.len() != 6 || opponent.is_empty() || opponent.len() > 24 {
        return Err(ApiError::Bad("malformed report".into()));
    }
    let (id, name) = reporter;
    let mut tx = db.begin().await?;
    let twin = sqlx::query(
        "SELECT id FROM matches WHERE ranked = 0 AND room_code = ? AND player_b IS NULL AND player_a <> ? \
         AND name_a = ? AND name_b = ? AND score_a = ? AND score_b = ? AND created_at >= UTC_TIMESTAMP() - INTERVAL ? SECOND \
         ORDER BY id DESC LIMIT 1 FOR UPDATE",
    )
    .bind(room)
    .bind(id)
    .bind(opponent)
    .bind(name)
    .bind(score[1])
    .bind(score[0])
    .bind(CASUAL_PAIR_SECS)
    .fetch_optional(&mut *tx)
    .await?;
    let match_id = match twin {
        Some(row) => {
            let match_id: u64 = row.try_get("id")?;
            sqlx::query("UPDATE matches SET player_b = ? WHERE id = ?").bind(id).bind(match_id).execute(&mut *tx).await?;
            info!(match_id, "casual round: {name} joins the opponent's report");
            match_id
        }
        None => {
            let res = sqlx::query(
                "INSERT INTO matches (room_code, ranked, player_a, player_b, name_a, name_b, elo_a, elo_b, score_a, score_b, winner, status, finished_at) \
                 VALUES (?, 0, ?, NULL, ?, ?, NULL, NULL, ?, ?, ?, 'finished', UTC_TIMESTAMP())",
            )
            .bind(room)
            .bind(id)
            .bind(name)
            .bind(opponent)
            .bind(score[0])
            .bind(score[1])
            .bind(if won { 0i8 } else { 1i8 })
            .execute(&mut *tx)
            .await?;
            info!("casual round in {room}: {name} {}-{} {opponent}", score[0], score[1]);
            res.last_insert_id()
        }
    };
    tx.commit().await?;
    Ok(match_id)
}

/// Applies the timeouts to one match that is still `playing`: a lone report past its grace period
/// settles, a match nobody reported on is voided once abandoned. A match holding both reports
/// (left behind by an older server) is resolved right away.
async fn expire(db: &MySqlPool, match_id: u64) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT status, report_a, report_b, report_a_at, report_b_at, created_at FROM matches WHERE id = ?")
        .bind(match_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such match".into()))?;
    let status: String = row.try_get("status")?;
    if status != "playing" {
        return Ok(());
    }
    let now = Utc::now().naive_utc();
    let ra: Option<String> = row.try_get("report_a")?;
    let rb: Option<String> = row.try_get("report_b")?;
    let at_a: Option<NaiveDateTime> = row.try_get("report_a_at")?;
    let at_b: Option<NaiveDateTime> = row.try_get("report_b_at")?;
    let created: NaiveDateTime = row.try_get("created_at")?;
    let lone = |r: Report, at: Option<NaiveDateTime>| at.filter(|at| (now - *at).num_seconds() >= LONE_REPORT_GRACE_SECS).map(|_| r);
    match (ra.as_deref().and_then(Report::decode), rb.as_deref().and_then(Report::decode)) {
        (Some(ra), Some(rb)) => {
            info!(match_id, "resolving a match left with both reports in");
            resolve(db, match_id, ra, rb).await
        }
        (Some(r), None) | (None, Some(r)) => {
            let ready = if rb.is_none() { lone(r, at_a) } else { lone(r, at_b) };
            match ready {
                Some(r) => {
                    info!(match_id, "settling on a single report (the other player never reported)");
                    settle(db, match_id, r).await
                }
                None => Ok(()),
            }
        }
        (None, None) if (now - created).num_seconds() >= ABANDONED_SECS => {
            info!(match_id, "voiding abandoned match");
            void(db, match_id).await
        }
        (None, None) => Ok(()),
    }
}

/// Settles matches whose second report never came, and voids abandoned ones. Run periodically.
pub async fn sweep(db: &MySqlPool) -> Result<(), ApiError> {
    let rows = sqlx::query("SELECT id FROM matches WHERE status = 'playing'").fetch_all(db).await?;
    for row in rows {
        expire(db, row.try_get("id")?).await?;
    }
    Ok(())
}

/// Where a match stands, from one of its players' point of view. Polled by the client after a
/// match for the result popup, so the timeouts are applied here too rather than left to the
/// next sweep.
#[derive(Serialize, Debug)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    Playing,
    Finished {
        won: bool,
        delta: i32,
        /// `[mine, theirs]`.
        score: [i32; 2],
    },
    Void,
}

pub async fn status(db: &MySqlPool, match_id: u64, viewer: u64) -> Result<Outcome, ApiError> {
    expire(db, match_id).await?;
    let row = sqlx::query("SELECT player_a, player_b, status, score_a, score_b, winner, delta_a, delta_b FROM matches WHERE id = ?")
        .bind(match_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such match".into()))?;
    let (a, b): (u64, u64) = (row.try_get("player_a")?, row.try_get("player_b")?);
    let i_am_a = if viewer == a {
        true
    } else if viewer == b {
        false
    } else {
        return Err(ApiError::Forbidden("not your match".into()));
    };
    let status: String = row.try_get("status")?;
    Ok(match status.as_str() {
        "finished" => {
            let (mine, theirs) = if i_am_a { ("a", "b") } else { ("b", "a") };
            let winner: i8 = row.try_get("winner")?;
            Outcome::Finished {
                won: (winner == 0) == i_am_a,
                delta: row.try_get(format!("delta_{mine}").as_str())?,
                score: [row.try_get(format!("score_{mine}").as_str())?, row.try_get(format!("score_{theirs}").as_str())?],
            }
        }
        "void" => Outcome::Void,
        _ => Outcome::Playing,
    })
}

/// One row of a player's match history, from that player's point of view.
#[derive(Serialize, Debug)]
pub struct HistoryEntry {
    pub id: u64,
    /// False for a private-room round: no ratings, and it counted for nothing.
    pub ranked: bool,
    pub opponent: String,
    pub my_score: i32,
    pub their_score: i32,
    pub won: bool,
    /// Ratings at match time and the change; absent on casual rounds.
    pub my_elo: Option<i32>,
    pub their_elo: Option<i32>,
    pub delta: Option<i32>,
    /// Unix seconds.
    pub played_at: i64,
}

pub async fn history(db: &MySqlPool, account_id: u64, limit: u32) -> Result<Vec<HistoryEntry>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, ranked, player_a, name_a, name_b, elo_a, elo_b, score_a, score_b, winner, delta_a, delta_b, finished_at \
         FROM matches WHERE status = 'finished' AND (player_a = ? OR player_b = ?) ORDER BY id DESC LIMIT ?",
    )
    .bind(account_id)
    .bind(account_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let i_am_a: u64 = row.try_get("player_a")?;
        let i_am_a = i_am_a == account_id;
        let (mine, theirs) = if i_am_a { ("a", "b") } else { ("b", "a") };
        let winner: i8 = row.try_get("winner")?;
        let finished: Option<NaiveDateTime> = row.try_get("finished_at")?;
        out.push(HistoryEntry {
            id: row.try_get("id")?,
            ranked: row.try_get::<bool, _>("ranked")?,
            opponent: row.try_get(format!("name_{theirs}").as_str())?,
            my_score: row.try_get(format!("score_{mine}").as_str())?,
            their_score: row.try_get(format!("score_{theirs}").as_str())?,
            won: (winner == 0) == i_am_a,
            my_elo: row.try_get(format!("elo_{mine}").as_str())?,
            their_elo: row.try_get(format!("elo_{theirs}").as_str())?,
            delta: row.try_get(format!("delta_{mine}").as_str())?,
            played_at: finished.map(|t| t.and_utc().timestamp()).unwrap_or(0),
        });
    }
    Ok(out)
}
