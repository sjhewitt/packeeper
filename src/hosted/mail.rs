//! Email for a hosted server: sign-up checks, invitations, and password resets.
//! With PAC_SMTP_URL set, mail goes out over SMTP. Without it, each message is
//! written to an outbox folder, for testing.

use std::path::PathBuf;

use anyhow::Context;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

pub enum Mailer {
    Smtp { transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox },
    Outbox(PathBuf),
}

impl Mailer {
    /// PAC_SMTP_URL, such as smtps://user:password@smtp.example.com:465, and
    /// PAC_MAIL_FROM, such as "PAC Keeper <hello@packeeper.ca>".
    pub fn from_env(outbox: PathBuf) -> anyhow::Result<Mailer> {
        let Ok(url) = std::env::var("PAC_SMTP_URL") else {
            return Ok(Mailer::Outbox(outbox));
        };
        let from = std::env::var("PAC_MAIL_FROM").context("PAC_MAIL_FROM is needed with PAC_SMTP_URL")?;
        Ok(Mailer::Smtp {
            transport: AsyncSmtpTransport::<Tokio1Executor>::from_url(&url).context("PAC_SMTP_URL")?.build(),
            from: from.parse().context("PAC_MAIL_FROM")?,
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Mailer::Smtp { from, .. } => format!("SMTP, from {from}"),
            Mailer::Outbox(dir) => format!("no SMTP; messages go to {}", dir.display()),
        }
    }

    pub async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        match self {
            Mailer::Smtp { transport, from } => {
                let message = Message::builder()
                    .from(from.clone())
                    .to(to.parse().context("recipient address")?)
                    .subject(subject)
                    .header(ContentType::TEXT_PLAIN)
                    .body(body.to_string())?;
                transport.send(message).await?;
            }
            Mailer::Outbox(dir) => {
                std::fs::create_dir_all(dir)?;
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
                let safe: String = to.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                let path = dir.join(format!("{stamp}-{safe}.txt"));
                std::fs::write(&path, format!("To: {to}\nSubject: {subject}\n\n{body}\n"))?;
                tracing::info!("email to {to} written to {}", path.display());
            }
        }
        Ok(())
    }
}
