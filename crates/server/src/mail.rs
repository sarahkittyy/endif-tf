//! Outgoing mail: verification and password reset codes, sent through the Resend HTTP API
//! (`POST https://api.resend.com/emails`). `MAIL_MODE=log` writes the codes to the log instead,
//! which is what development uses.

use crate::config::{MailConfig, MailMode};
use serde_json::json;
use tracing::{info, warn};

const RESEND_URL: &str = "https://api.resend.com/emails";

pub enum Mailer {
    Log,
    Resend { client: reqwest::Client, api_key: String, from: String, reply_to: Option<String> },
}

impl Mailer {
    pub fn new(cfg: &MailConfig) -> Result<Mailer, String> {
        if cfg.mode == MailMode::Log {
            warn!("MAIL_MODE=log: e-mail codes are printed to this log, nothing is sent");
            return Ok(Mailer::Log);
        }
        if cfg.resend_api_key.is_empty() {
            return Err("MAIL_MODE=resend needs RESEND_API_KEY".into());
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        info!("mail via Resend from {}", cfg.from);
        Ok(Mailer::Resend { client, api_key: cfg.resend_api_key.clone(), from: cfg.from.clone(), reply_to: cfg.reply_to.clone() })
    }

    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), String> {
        match self {
            Mailer::Log => {
                info!("[mail to {to}] {subject}\n{body}");
                Ok(())
            }
            Mailer::Resend { client, api_key, from, reply_to } => {
                let mut payload = json!({ "from": from, "to": [to], "subject": subject, "text": body });
                if let Some(r) = reply_to {
                    payload["reply_to"] = json!(r);
                }
                let res = client.post(RESEND_URL).bearer_auth(api_key).json(&payload).send().await.map_err(|e| format!("resend request: {e}"))?;
                let status = res.status();
                let text = res.text().await.unwrap_or_default();
                if !status.is_success() {
                    return Err(format!("resend answered {status}: {text}"));
                }
                let id = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["id"].as_str().map(str::to_string)).unwrap_or_default();
                info!("mail to {to} accepted by Resend ({id})");
                Ok(())
            }
        }
    }

    /// The verification / reset mail: addressed to the account's username so someone whose address
    /// was typed by a stranger can tell it is not their account.
    pub async fn send_code(&self, to: &str, username: &str, purpose: &str, code: &str) -> Result<(), String> {
        let (subject, what) = match purpose {
            "verify" => ("endif.tf: verify your e-mail", "verify your endif.tf account"),
            _ => ("endif.tf: password reset", "reset your endif.tf password"),
        };
        let body = format!("Hello, {username}

If this is not you, ignore this.

Your code is {code}.

Enter it within 15 minutes to {what}.
");
        self.send(to, subject, &body).await
    }
}
