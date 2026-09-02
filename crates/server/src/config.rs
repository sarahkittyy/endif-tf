//! Server configuration, read from the environment (`.env` in the working directory is loaded
//! first, see `.env.example` at the repository root).

use crate::limits::LimitConfig;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub max_room_size: usize,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_days: i64,
    pub mail: MailConfig,
    pub limits: LimitConfig,
}

#[derive(Clone, Debug)]
pub struct MailConfig {
    /// Sender, `Name <address@domain>`; the domain must be verified in Resend.
    pub from: String,
    /// Optional Reply-To address.
    pub reply_to: Option<String>,
    pub mode: MailMode,
    pub resend_api_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailMode {
    /// Codes are written to the server log (development).
    Log,
    /// Codes are mailed through the Resend API.
    Resend,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn parsed<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match var(name) {
        Some(v) => v.parse().map_err(|_| format!("{name}={v:?} is not valid")),
        None => Ok(default),
    }
}

fn flag(name: &str, default: bool) -> Result<bool, String> {
    match var(name).map(|v| v.to_ascii_lowercase()).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "on" | "yes") => Ok(true),
        Some("0" | "false" | "off" | "no") => Ok(false),
        Some(v) => Err(format!("{name}={v:?} must be true or false")),
    }
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let host: IpAddr = parsed("ENDIF_HOST", IpAddr::V4(Ipv4Addr::UNSPECIFIED))?;
        let port: u16 = parsed("ENDIF_PORT", 3536)?;
        let jwt_secret = match var("JWT_SECRET") {
            Some(s) if s != "change-me-to-a-long-random-string" => s,
            _ => {
                tracing::warn!("JWT_SECRET is unset (or still the example value): using a random one, logins will not survive a restart");
                use rand::Rng;
                (0..48).map(|_| rand::thread_rng().gen_range(b'a'..=b'z') as char).collect()
            }
        };
        let resend_api_key = var("RESEND_API_KEY").unwrap_or_default();
        let mode = match var("MAIL_MODE").as_deref() {
            Some("resend") => MailMode::Resend,
            Some("log") => MailMode::Log,
            None if !resend_api_key.is_empty() => MailMode::Resend,
            None => MailMode::Log,
            Some(other) => return Err(format!("MAIL_MODE={other:?} must be log or resend")),
        };
        Ok(Config {
            bind: SocketAddr::new(host, port),
            max_room_size: parsed("ENDIF_MAX_ROOM_SIZE", 2)?,
            database_url: var("DATABASE_URL").ok_or("DATABASE_URL is not set (see .env.example)")?,
            jwt_secret,
            jwt_days: parsed("JWT_DAYS", 30)?,
            mail: MailConfig {
                from: var("MAIL_FROM").unwrap_or_else(|| "endif.tf <game@endif.tf>".to_string()),
                reply_to: var("MAIL_REPLY_TO"),
                mode,
                resend_api_key,
            },
            limits: LimitConfig {
                enabled: flag("ENDIF_RATE_LIMITS", true)?,
                trust_proxy: flag("ENDIF_TRUST_PROXY", false)?,
                mail_per_day: parsed("ENDIF_MAIL_PER_DAY", 90)?,
            },
        })
    }
}
