//! Password hashing (argon2id), login tokens (JWT, HS256) and the axum extractor that turns an
//! `Authorization: Bearer` header into the calling account.

use crate::api::{ApiError, AppState};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sqlx::Row;

pub fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default().hash_password(password.as_bytes()).map(|h| h.to_string()).map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else { return false };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

/// A real argon2 hash of a random password, verified against when a login names an account that
/// does not exist: the attempt then costs the same time as a wrong password on a real account, so
/// response times do not tell usernames apart. Computed once, on first use.
pub fn dummy_hash() -> &'static str {
    use std::sync::LazyLock;
    static DUMMY: LazyLock<String> = LazyLock::new(|| {
        let random: String = (0..32).map(|_| rand::random::<u8>() as char).collect();
        hash_password(&random).expect("argon2 hashing works")
    });
    &DUMMY
}

/// A 6 digit e-mail code.
pub fn generate_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

/// Codes are short-lived, so a plain SHA-256 is enough to keep them out of the database in clear.
pub fn hash_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(code.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Claims {
    /// Account id.
    pub sub: u64,
    pub name: String,
    /// Must match the account's `token_version`.
    pub ver: i32,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Clone)]
pub struct Jwt {
    enc: EncodingKey,
    dec: DecodingKey,
    days: i64,
}

impl Jwt {
    pub fn new(secret: &str, days: i64) -> Jwt {
        Jwt { enc: EncodingKey::from_secret(secret.as_bytes()), dec: DecodingKey::from_secret(secret.as_bytes()), days }
    }

    pub fn issue(&self, id: u64, username: &str, version: i32) -> Result<String, String> {
        let now = chrono::Utc::now().timestamp();
        let claims = Claims { sub: id, name: username.to_string(), ver: version, iat: now, exp: now + self.days * 86_400 };
        jsonwebtoken::encode(&Header::default(), &claims, &self.enc).map_err(|e| e.to_string())
    }

    pub fn verify(&self, token: &str) -> Option<Claims> {
        jsonwebtoken::decode::<Claims>(token, &self.dec, &Validation::new(Algorithm::HS256)).ok().map(|d| d.claims)
    }
}

/// The logged-in account making a request.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: u64,
    pub username: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts.headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default();
        let token = header.strip_prefix("Bearer ").ok_or_else(|| ApiError::Unauthorized("not logged in".into()))?;
        let claims = state.jwt.verify(token).ok_or_else(|| ApiError::Unauthorized("session expired, log in again".into()))?;
        // The token must still match the account (password changed = version bumped; renamed = fresh token).
        let row = sqlx::query("SELECT username, token_version FROM accounts WHERE id = ? AND verified_at IS NOT NULL")
            .bind(claims.sub)
            .fetch_optional(&state.db)
            .await?;
        let Some(row) = row else { return Err(ApiError::Unauthorized("account no longer exists".into())) };
        let version: i32 = row.try_get("token_version")?;
        if version != claims.ver {
            return Err(ApiError::Unauthorized("session expired, log in again".into()));
        }
        Ok(AuthUser { id: claims.sub, username: row.try_get("username")? })
    }
}

/// `Option<AuthUser>`: routes open to anonymous players too (quick play). No `Authorization`
/// header is nobody; a header that does not check out is still an error, so a stale token is
/// reported rather than silently played as anonymous.
impl OptionalFromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Option<Self>, Self::Rejection> {
        if !parts.headers.contains_key(axum::http::header::AUTHORIZATION) {
            return Ok(None);
        }
        <AuthUser as FromRequestParts<AppState>>::from_request_parts(parts, state).await.map(Some)
    }
}
