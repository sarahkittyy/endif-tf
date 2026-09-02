//! The account / matchmaking HTTP API, mounted under `/api` next to the signaling websocket.
//!
//! Every response is JSON; errors are `{"error": "..."}` with a 4xx/5xx status. Logged-in calls
//! carry `Authorization: Bearer <token>` (the token from `/api/login` etc.).

use crate::auth::{self, AuthUser, Jwt};
use crate::limits::{self, Group, Limiter};
use crate::mail::Mailer;
use crate::matches::{self, Report};
use crate::queue::{Matched, PollResult, Queue};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::mysql::MySqlRow;
use sqlx::{MySqlConnection, MySqlPool, Row};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

pub type AppState = Arc<Inner>;

pub struct Inner {
    pub db: MySqlPool,
    pub jwt: Jwt,
    pub mailer: Mailer,
    pub queue: Mutex<Queue>,
    pub limits: Limiter,
    /// Signaling room occupancy (`rooms.rs`), for `GET /api/room/{code}`.
    pub rooms: crate::rooms::SharedRooms,
}

// ------------------------------------------------------------------------------------ errors

#[derive(Debug)]
pub enum ApiError {
    Bad(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Internal(String),
    /// Rate limited: the ready-made 429 from `limits`.
    Limited(Response),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        error!("database error: {e}");
        ApiError::Internal("database error".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::Bad(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            ApiError::Limited(res) => return res,
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

// ------------------------------------------------------------------------------------ router

/// Routes are grouped by how much a request costs (see `limits`): mailing routes and password
/// hashing routes sit behind tighter buckets than the rest, and everything shares the general one.
pub fn router(state: AppState) -> Router {
    let limited = {
        let state = state.clone();
        move |group: Group| from_fn_with_state((state.clone(), group), limits::limit)
    };
    let register = Router::new().route("/register", post(register)).route_layer(limited(Group::Register));
    let mail = Router::new().route("/forgot", post(forgot)).route("/resend", post(resend)).route_layer(limited(Group::Mail));
    let auth = Router::new()
        .route("/verify", post(verify))
        .route("/login", post(login))
        .route("/reset", post(reset))
        .route("/account/username", post(change_username))
        .route("/account/password", post(change_password))
        .route_layer(limited(Group::Auth));
    let rest = Router::new()
        .route("/me", get(me))
        .route("/profile/{username}", get(profile))
        .route("/stats", get(stats))
        .route("/queue/join", post(queue_join))
        .route("/queue/poll", post(queue_poll))
        .route("/queue/leave", post(queue_leave))
        .route("/match/{id}", get(match_status))
        .route("/match/{id}/report", post(match_report))
        .route("/match/casual", post(match_casual))
        .route("/room/{code}", get(room_info));
    Router::new()
        .merge(register)
        .merge(mail)
        .merge(auth)
        .merge(rest)
        .layer(limited(Group::General))
        // The web client runs on the site's origin, the API on the server's. Outermost, so the
        // 429s from the limiter carry the CORS headers too (the browser hides the body otherwise).
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}

// ------------------------------------------------------------------------------------ accounts

#[derive(Serialize, Debug)]
pub struct UserInfo {
    pub id: u64,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub elo: i32,
    pub wins: i32,
    pub losses: i32,
}

/// Loads an account's public fields (plus the e-mail when `private`).
async fn load_user(db: &MySqlPool, id: u64, private: bool) -> Result<UserInfo, ApiError> {
    let row = sqlx::query("SELECT id, username, email, elo, wins, losses FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such account".into()))?;
    Ok(UserInfo {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        email: if private { Some(row.try_get("email")?) } else { None },
        elo: row.try_get("elo")?,
        wins: row.try_get("wins")?,
        losses: row.try_get("losses")?,
    })
}

async fn session_response(state: &AppState, id: u64) -> ApiResult {
    let user = load_user(&state.db, id, true).await?;
    let version: i32 = sqlx::query("SELECT token_version FROM accounts WHERE id = ?").bind(id).fetch_one(&state.db).await?.try_get("token_version")?;
    let token = state.jwt.issue(id, &user.username, version).map_err(ApiError::Internal)?;
    Ok(Json(json!({ "token": token, "user": user })))
}

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 20;
pub const PASSWORD_MIN: usize = 8;

fn validate_username(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    let n = name.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&n) {
        return Err(ApiError::Bad(format!("username must be {USERNAME_MIN} to {USERNAME_MAX} characters")));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(ApiError::Bad("username: letters, digits, _ - . only".into()));
    }
    Ok(name.to_string())
}

fn validate_email(email: &str) -> Result<String, ApiError> {
    let email = email.trim().to_lowercase();
    let ok = email.len() <= 254 && email.split_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.'));
    if !ok || email.chars().any(char::is_whitespace) {
        return Err(ApiError::Bad("that does not look like an e-mail address".into()));
    }
    Ok(email)
}

fn validate_password(pw: &str) -> Result<(), ApiError> {
    if pw.chars().count() < PASSWORD_MIN {
        return Err(ApiError::Bad(format!("password must be at least {PASSWORD_MIN} characters")));
    }
    if pw.len() > 256 {
        return Err(ApiError::Bad("password is too long".into()));
    }
    Ok(())
}

/// A `x IS NOT NULL AS flag` column, which MariaDB and MySQL type differently.
fn flag(row: &MySqlRow, col: &str) -> Result<bool, ApiError> {
    Ok(row.try_get::<i64, _>(col).or_else(|_| row.try_get::<bool, _>(col).map(i64::from))? != 0)
}

/// A unique-key violation on `accounts` (two registrations racing for one name or address) as
/// the player should read it; anything else stays a database error.
fn duplicate(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::Bad(if d.message().contains("uq_accounts_email") { "that e-mail already has an account" } else { "that username is taken" }.into())
        }
        _ => e.into(),
    }
}

async fn hash_password(pw: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || auth::hash_password(&pw)).await.map_err(|e| ApiError::Internal(e.to_string()))?.map_err(ApiError::Internal)
}

async fn verify_password(pw: String, hash: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || auth::verify_password(&pw, &hash)).await.map_err(|e| ApiError::Internal(e.to_string()))
}

/// Minimum spacing between two codes to the same account.
const CODE_COOLDOWN_SECS: i64 = 60;
const CODE_TTL_MINUTES: i64 = 15;
const CODE_MAX_ATTEMPTS: i32 = 5;

/// Mails a fresh code to the account and records it (replacing any older one). The record is
/// written after the mail has gone out, so a failed send leaves nothing behind: no cooldown holds
/// the next request back, and no code exists that never reached anybody.
async fn send_code(state: &AppState, account_id: u64, email: &str, username: &str, purpose: &str) -> Result<(), ApiError> {
    let recent = sqlx::query("SELECT created_at FROM email_codes WHERE account_id = ? AND purpose = ? AND created_at > UTC_TIMESTAMP() - INTERVAL ? SECOND")
        .bind(account_id)
        .bind(purpose)
        .bind(CODE_COOLDOWN_SECS)
        .fetch_optional(&state.db)
        .await?;
    if recent.is_some() {
        return Err(ApiError::Bad("a code was sent a moment ago; check your mail or wait a minute".into()));
    }
    let code = auth::generate_code();
    state.mailer.send_code(email, username, purpose, &code).await.map_err(|e| {
        error!("could not send {purpose} mail to {email}: {e}");
        ApiError::Internal("could not send the e-mail; try again later".into())
    })?;
    sqlx::query("DELETE FROM email_codes WHERE account_id = ? AND purpose = ?").bind(account_id).bind(purpose).execute(&state.db).await?;
    sqlx::query("INSERT INTO email_codes (account_id, purpose, code_hash, expires_at) VALUES (?, ?, ?, UTC_TIMESTAMP() + INTERVAL ? MINUTE)")
        .bind(account_id)
        .bind(purpose)
        .bind(auth::hash_code(&code))
        .bind(CODE_TTL_MINUTES)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Checks a code against the account's current one for `purpose` and consumes it when it matches.
/// Runs on the caller's transaction, so the code only disappears together with whatever it
/// unlocks; a wrong guess is counted through the pool, so it sticks when the caller rolls back.
async fn check_code(state: &AppState, tx: &mut MySqlConnection, account_id: u64, purpose: &str, code: &str) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT id, code_hash, attempts, expires_at < UTC_TIMESTAMP() AS expired FROM email_codes WHERE account_id = ? AND purpose = ? ORDER BY id DESC LIMIT 1 FOR UPDATE")
        .bind(account_id)
        .bind(purpose)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::Bad("no code was requested; ask for a new one".into()))?;
    let id: u64 = row.try_get("id")?;
    let attempts: i32 = row.try_get("attempts")?;
    if flag(&row, "expired")? {
        return Err(ApiError::Bad("that code has expired; ask for a new one".into()));
    }
    if attempts >= CODE_MAX_ATTEMPTS {
        return Err(ApiError::Bad("too many wrong codes; ask for a new one".into()));
    }
    let stored: String = row.try_get("code_hash")?;
    if stored != auth::hash_code(code) {
        sqlx::query("UPDATE email_codes SET attempts = attempts + 1 WHERE id = ?").bind(id).execute(&state.db).await?;
        return Err(ApiError::Bad("wrong code".into()));
    }
    sqlx::query("DELETE FROM email_codes WHERE id = ?").bind(id).execute(&mut *tx).await?;
    Ok(())
}

/// Unverified accounts older than this are dropped, codes and all.
const UNVERIFIED_MAX_DAYS: i64 = 30;

/// Drops registrations that were never verified. One small DELETE; run it every hour or so.
pub async fn sweep_unverified(db: &MySqlPool) -> Result<u64, ApiError> {
    let res = sqlx::query("DELETE FROM accounts WHERE verified_at IS NULL AND created_at < UTC_TIMESTAMP() - INTERVAL ? DAY")
        .bind(UNVERIFIED_MAX_DAYS)
        .execute(db)
        .await?;
    Ok(res.rows_affected())
}

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    username: String,
    password: String,
}

/// Creates an unverified account and mails the verification code. Unverified accounts holding the
/// same e-mail or username are replaced (someone typed a wrong address, or never finished).
async fn register(State(state): State<AppState>, Json(req): Json<RegisterReq>) -> ApiResult {
    let email = validate_email(&req.email)?;
    let username = validate_username(&req.username)?;
    validate_password(&req.password)?;
    // Hashed before the transaction so no connection sits idle through the argon2 work.
    let hash = hash_password(req.password).await?;
    let mut tx = state.db.begin().await?;
    let clashes = sqlx::query("SELECT id, email, username, verified_at IS NOT NULL AS verified FROM accounts WHERE email = ? OR username = ?")
        .bind(&email)
        .bind(&username)
        .fetch_all(&mut *tx)
        .await?;
    for row in clashes {
        let id: u64 = row.try_get("id")?;
        let taken_email: String = row.try_get("email")?;
        if flag(&row, "verified")? {
            let what = if taken_email == email { "that e-mail already has an account" } else { "that username is taken" };
            return Err(ApiError::Bad(what.into()));
        }
        sqlx::query("DELETE FROM accounts WHERE id = ? AND verified_at IS NULL").bind(id).execute(&mut *tx).await?;
    }
    // Two registrations racing for one name: the unique keys decide, and the loser reads "taken".
    let res = sqlx::query("INSERT INTO accounts (email, username, password_hash, elo) VALUES (?, ?, ?, ?)")
        .bind(&email)
        .bind(&username)
        .bind(&hash)
        .bind(crate::elo::START)
        .execute(&mut *tx)
        .await
        .map_err(duplicate)?;
    let id = res.last_insert_id();
    tx.commit().await?;
    info!(id, %username, %email, "account registered, awaiting verification");
    send_code(&state, id, &email, &username, "verify").await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct VerifyReq {
    email: String,
    code: String,
}

/// Turns the code from the mail into a verified account and logs it in.
async fn verify(State(state): State<AppState>, Json(req): Json<VerifyReq>) -> ApiResult {
    let email = validate_email(&req.email)?;
    let row = sqlx::query("SELECT id, verified_at IS NOT NULL AS verified FROM accounts WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Bad("no account with that e-mail".into()))?;
    let id: u64 = row.try_get("id")?;
    if flag(&row, "verified")? {
        return Err(ApiError::Bad("that account is already verified; log in".into()));
    }
    // The code is consumed and the account activated together, or neither.
    let mut tx = state.db.begin().await?;
    check_code(&state, &mut tx, id, "verify", &req.code).await?;
    sqlx::query("UPDATE accounts SET verified_at = UTC_TIMESTAMP() WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    info!(id, %email, "account verified");
    session_response(&state, id).await
}

#[derive(Deserialize)]
struct ResendReq {
    email: String,
}

/// Mails a fresh verification code to an account still waiting for one.
async fn resend(State(state): State<AppState>, Json(req): Json<ResendReq>) -> ApiResult {
    let email = validate_email(&req.email)?;
    let row = sqlx::query("SELECT id, username, verified_at IS NOT NULL AS verified FROM accounts WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Bad("no account with that e-mail; create it again".into()))?;
    if flag(&row, "verified")? {
        return Err(ApiError::Bad("that account is already verified; log in".into()));
    }
    let id: u64 = row.try_get("id")?;
    let username: String = row.try_get("username")?;
    send_code(&state, id, &email, &username, "verify").await?;
    info!(id, %email, "verification code re-sent");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoginReq {
    /// Username or e-mail.
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> ApiResult {
    let who = req.username.trim();
    // Per target as well as per address: a password guesser cannot spread one account's attempts
    // over many machines. The key is capped so a giant "username" cannot bloat the bucket table.
    let target: String = who.to_lowercase().chars().take(254).collect();
    state
        .limits
        .check(&format!("login:{target}"), limits::LOGIN_TARGET, &format!("login {target}"), "too many login attempts for that account; try again in {wait}")
        .map_err(ApiError::Limited)?;
    let row = sqlx::query("SELECT id, password_hash, verified_at IS NOT NULL AS verified FROM accounts WHERE username = ? OR email = ?")
        .bind(who)
        .bind(who.to_lowercase())
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        // Verify against a real hash anyway, so a missing account takes as long as a wrong password.
        let _ = verify_password(req.password, auth::dummy_hash().to_string()).await;
        return Err(ApiError::Unauthorized("wrong username or password".into()));
    };
    let id: u64 = row.try_get("id")?;
    let hash: String = row.try_get("password_hash")?;
    let verified = flag(&row, "verified")?;
    if !verify_password(req.password, hash).await? {
        return Err(ApiError::Unauthorized("wrong username or password".into()));
    }
    if !verified {
        return Err(ApiError::Unauthorized("this account is not verified yet: create it again to get a new code".into()));
    }
    session_response(&state, id).await
}

#[derive(Deserialize)]
struct ForgotReq {
    email: String,
}

/// Mails a password reset code. Always answers ok so addresses cannot be probed.
async fn forgot(State(state): State<AppState>, Json(req): Json<ForgotReq>) -> ApiResult {
    let email = validate_email(&req.email)?;
    let row = sqlx::query("SELECT id, username FROM accounts WHERE email = ? AND verified_at IS NOT NULL").bind(&email).fetch_optional(&state.db).await?;
    match row {
        Some(row) => {
            let id: u64 = row.try_get("id")?;
            let username: String = row.try_get("username")?;
            match send_code(&state, id, &email, &username, "reset").await {
                Ok(()) | Err(ApiError::Bad(_)) => {}
                Err(e) => return Err(e),
            }
        }
        None => info!(%email, "password reset requested for an unknown address"),
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ResetReq {
    email: String,
    code: String,
    password: String,
}

/// Sets a new password with the code from the mail and logs in. Older sessions are logged out.
async fn reset(State(state): State<AppState>, Json(req): Json<ResetReq>) -> ApiResult {
    let email = validate_email(&req.email)?;
    validate_password(&req.password)?;
    let row = sqlx::query("SELECT id FROM accounts WHERE email = ? AND verified_at IS NOT NULL")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Bad("no account with that e-mail".into()))?;
    let id: u64 = row.try_get("id")?;
    // Hashed first (a wrong code then costs one hash, which the auth limiter keeps in check) so
    // the transaction stays short: the code goes and the password changes together, or neither.
    let hash = hash_password(req.password).await?;
    let mut tx = state.db.begin().await?;
    check_code(&state, &mut tx, id, "reset", &req.code).await?;
    sqlx::query("UPDATE accounts SET password_hash = ?, token_version = token_version + 1 WHERE id = ?").bind(&hash).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    info!(id, %email, "password reset");
    session_response(&state, id).await
}

async fn me(State(state): State<AppState>, user: AuthUser) -> ApiResult {
    Ok(Json(json!({ "user": load_user(&state.db, user.id, true).await? })))
}

/// Occupancy of a signaling room: `{"peers": n, "max": m}`. A browser cannot see why a websocket
/// handshake was refused, so the web client asks this after a failed join to tell a full room
/// from a rate limit or an unreachable server.
async fn room_info(State(state): State<AppState>, Path(code): Path<String>) -> ApiResult {
    if code.is_empty() || code.len() > 32 || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::Bad("invalid room code".into()));
    }
    let rooms = state.rooms.lock().unwrap();
    Ok(Json(json!({ "peers": rooms.occupancy(&format!("endif-{code}")), "max": rooms.max() })))
}

#[derive(Deserialize)]
struct UsernameReq {
    username: String,
}

/// Renames the account; returns a fresh token carrying the new name.
async fn change_username(State(state): State<AppState>, user: AuthUser, Json(req): Json<UsernameReq>) -> ApiResult {
    let username = validate_username(&req.username)?;
    let taken = sqlx::query("SELECT id FROM accounts WHERE username = ? AND id <> ?").bind(&username).bind(user.id).fetch_optional(&state.db).await?;
    if taken.is_some() {
        return Err(ApiError::Bad("that username is taken".into()));
    }
    sqlx::query("UPDATE accounts SET username = ? WHERE id = ?").bind(&username).bind(user.id).execute(&state.db).await.map_err(duplicate)?;
    info!(id = user.id, from = %user.username, to = %username, "username changed");
    session_response(&state, user.id).await
}

#[derive(Deserialize)]
struct PasswordReq {
    current: String,
    password: String,
}

/// Changes the password; every other session is logged out and a fresh token is returned.
async fn change_password(State(state): State<AppState>, user: AuthUser, Json(req): Json<PasswordReq>) -> ApiResult {
    validate_password(&req.password)?;
    let hash: String = sqlx::query("SELECT password_hash FROM accounts WHERE id = ?").bind(user.id).fetch_one(&state.db).await?.try_get("password_hash")?;
    if !verify_password(req.current, hash).await? {
        return Err(ApiError::Unauthorized("current password is wrong".into()));
    }
    let hash = hash_password(req.password).await?;
    sqlx::query("UPDATE accounts SET password_hash = ?, token_version = token_version + 1 WHERE id = ?").bind(&hash).bind(user.id).execute(&state.db).await?;
    info!(id = user.id, "password changed");
    session_response(&state, user.id).await
}

/// Public profile: rating, record and the last matches.
async fn profile(State(state): State<AppState>, Path(username): Path<String>) -> ApiResult {
    let row = sqlx::query("SELECT id FROM accounts WHERE username = ? AND verified_at IS NOT NULL")
        .bind(username.trim())
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("no such player".into()))?;
    let id: u64 = row.try_get("id")?;
    let user = load_user(&state.db, id, false).await?;
    let matches = matches::history(&state.db, id, 50).await?;
    Ok(Json(json!({ "user": user, "matches": matches })))
}

// ------------------------------------------------------------------------------------ matchmaking

/// How many players are in a game and how many are searching, for the main menu. No login needed.
async fn stats(State(state): State<AppState>) -> ApiResult {
    let playing = state.rooms.lock().unwrap().playing();
    let waiting = state.queue.lock().unwrap().len();
    Ok(Json(json!({ "playing": playing, "waiting": waiting })))
}

/// Enters the queue (or pairs with whoever is waiting). Poll `/queue/poll` with the ticket.
async fn queue_join(State(state): State<AppState>, user: AuthUser) -> ApiResult {
    let me = load_user(&state.db, user.id, false).await?;
    let (ticket, opponent) = state.queue.lock().unwrap().join(me.id, &me.username, me.elo);
    if let Some(opp) = opponent {
        let room = crate::queue::random_room_code();
        match matches::create(&state.db, &room, (opp.account_id, &opp.username, opp.elo), (me.id, &me.username, me.elo)).await {
            Ok(match_id) => {
                info!(match_id, %room, a = %opp.username, b = %me.username, "matched");
                let now = Instant::now();
                let for_opp = Matched { match_id, room: room.clone(), slot: 0, opponent: me.username.clone(), opponent_elo: me.elo, account_id: opp.account_id, created: now };
                let for_me = Matched { match_id, room, slot: 1, opponent: opp.username.clone(), opponent_elo: opp.elo, account_id: me.id, created: now };
                state.queue.lock().unwrap().pair(&opp.ticket, for_opp, &ticket, for_me);
            }
            Err(e) => {
                warn!("could not create the match record: {e:?}");
                state.queue.lock().unwrap().requeue(opp);
                return Err(e);
            }
        }
    }
    let waiting = state.queue.lock().unwrap().len();
    Ok(Json(json!({ "ticket": ticket, "waiting": waiting })))
}

#[derive(Deserialize)]
struct TicketReq {
    ticket: String,
}

async fn queue_poll(State(state): State<AppState>, _user: AuthUser, Json(req): Json<TicketReq>) -> ApiResult {
    let (result, waiting) = {
        let mut queue = state.queue.lock().unwrap();
        let result = queue.poll(&req.ticket);
        (result, queue.len())
    };
    let playing = state.rooms.lock().unwrap().playing();
    Ok(Json(match result {
        PollResult::Waiting { position } => json!({ "status": "waiting", "position": position, "waiting": waiting, "playing": playing }),
        PollResult::Matched(m) => json!({ "status": "matched", "match": m }),
        PollResult::Expired => json!({ "status": "expired" }),
    }))
}

async fn queue_leave(State(state): State<AppState>, _user: AuthUser, Json(req): Json<TicketReq>) -> ApiResult {
    state.queue.lock().unwrap().leave(&req.ticket);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ReportReq {
    /// `[score_a, score_b]` in match-record order.
    score: [i32; 2],
    /// 0 = player_a won, 1 = player_b won.
    winner: u8,
}

async fn match_report(State(state): State<AppState>, user: AuthUser, Path(id): Path<u64>, Json(req): Json<ReportReq>) -> ApiResult {
    matches::report(&state.db, id, user.id, Report { score_a: req.score[0], score_b: req.score[1], winner: req.winner }).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CasualReq {
    room: String,
    opponent: String,
    /// `[mine, theirs]`.
    score: [i32; 2],
    won: bool,
}

/// A round of a private room reached the frag limit: kept for the caller's history only.
async fn match_casual(State(state): State<AppState>, user: AuthUser, Json(req): Json<CasualReq>) -> ApiResult {
    let id = matches::casual(&state.db, (user.id, &user.username), req.room.trim(), req.opponent.trim(), req.score, req.won).await?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// Where one of the caller's matches stands, for the result popup after a game.
async fn match_status(State(state): State<AppState>, user: AuthUser, Path(id): Path<u64>) -> ApiResult {
    let outcome = matches::status(&state.db, id, user.id).await?;
    Ok(Json(serde_json::to_value(outcome).unwrap_or(Value::Null)))
}
