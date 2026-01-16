use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Instant;
use tracing::{debug, info, instrument, warn};

use super::Notifier;
use crate::models::ScrapeDiff;

pub struct SmsNotifier {
    client: reqwest::Client,
    account_sid: String,
    auth_token: String,
    from: String,
    to: Vec<String>,
}

impl SmsNotifier {
    pub fn new(account_sid: String, auth_token: String, from: String, to: Vec<String>) -> Self {
        let client = reqwest::Client::new();
        Self {
            client,
            account_sid,
            auth_token,
            from,
            to,
        }
    }

    fn build_sms_content(&self, diff: &ScrapeDiff) -> String {
        let mut message = String::new();

        message.push_str("UiO Emnevarsel\n");

        if !diff.added.is_empty() {
            message.push_str(&format!("\nNye ({}):\n", diff.added.len()));
            for course in &diff.added {
                message.push_str(&format!("• {} - {}\n", course.code, course.name));
            }
        }

        if !diff.removed.is_empty() {
            message.push_str(&format!("\nFjernet ({}):\n", diff.removed.len()));
            for course in &diff.removed {
                message.push_str(&format!("• {} - {}\n", course.code, course.name));
            }
        }

        message
    }

    async fn send_sms(&self, to: &str, body: &str) -> Result<()> {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.account_sid
        );

        let params = [
            ("From", self.from.as_str()),
            ("To", to),
            ("Body", body),
        ];

        debug!(
            to = %to,
            from = %self.from,
            body_len = body.len(),
            "Sending SMS via Twilio"
        );

        let response = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await
            .context("Failed to send SMS request to Twilio API")?;

        let status = response.status();
        let status_code = status.as_u16();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            warn!(
                status_code = status_code,
                error = %error_text,
                to = %to,
                "Twilio API request failed"
            );
            anyhow::bail!(
                "Twilio API error (HTTP {}): {}\n\
                 Check that TWILIO_ACCOUNT_SID and TWILIO_AUTH_TOKEN are valid.",
                status,
                error_text
            );
        }

        let response_body = response.text().await.unwrap_or_default();
        debug!(
            status_code = status_code,
            to = %to,
            response = %response_body,
            "SMS sent successfully"
        );

        Ok(())
    }
}

#[async_trait]
impl Notifier for SmsNotifier {
    fn name(&self) -> &'static str {
        "sms"
    }

    #[instrument(skip(self, diff), fields(
        notifier = "sms",
        recipients = ?self.to,
        added = diff.added.len(),
        removed = diff.removed.len()
    ))]
    async fn notify(&self, diff: &ScrapeDiff) -> Result<()> {
        if diff.is_empty() {
            debug!("No changes to notify, skipping SMS");
            return Ok(());
        }

        let start = Instant::now();
        let body = self.build_sms_content(diff);
        let recipients_str = self.to.join(", ");

        info!(
            from = %self.from,
            to = %recipients_str,
            recipient_count = self.to.len(),
            body_len = body.len(),
            added_courses = diff.added.len(),
            removed_courses = diff.removed.len(),
            "Preparing to send SMS"
        );

        // Send to each recipient
        let mut success_count = 0;
        let mut failure_count = 0;

        for recipient in &self.to {
            match self.send_sms(recipient, &body).await {
                Ok(_) => {
                    success_count += 1;
                    info!(
                        to = %recipient,
                        "SMS sent successfully"
                    );
                }
                Err(e) => {
                    failure_count += 1;
                    warn!(
                        to = %recipient,
                        error = %e,
                        "Failed to send SMS"
                    );
                }
            }
        }

        info!(
            success_count = success_count,
            failure_count = failure_count,
            total_duration_ms = start.elapsed().as_millis(),
            "SMS notification completed"
        );

        // Return error only if all sends failed
        if success_count == 0 && failure_count > 0 {
            anyhow::bail!("Failed to send SMS to any recipient");
        }

        Ok(())
    }
}
