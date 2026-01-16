use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use tracing::{debug, info};

use super::Notifier;
use crate::models::{Course, ScrapeDiff};

const RESEND_API_URL: &str = "https://api.resend.com/emails";

pub struct EmailNotifier {
    client: reqwest::Client,
    api_key: String,
    from: String,
    to: Vec<String>,
}

impl EmailNotifier {
    pub fn new(api_key: String, from: String, to: Vec<String>) -> Self {
        let client = reqwest::Client::new();
        Self {
            client,
            api_key,
            from,
            to,
        }
    }

    fn build_email_content(&self, diff: &ScrapeDiff) -> (String, String) {
        let subject = format!(
            "UiO Course Alert: {} new, {} removed",
            diff.added.len(),
            diff.removed.len()
        );

        let mut html = String::new();
        html.push_str(r#"<!DOCTYPE html><html><head><style>"#);
        html.push_str(r#"
            body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; max-width: 600px; margin: 0 auto; padding: 20px; }
            h1 { color: #333; border-bottom: 2px solid #0066cc; padding-bottom: 10px; }
            h2 { color: #0066cc; margin-top: 30px; }
            .course { background: #f5f5f5; border-left: 4px solid #0066cc; padding: 15px; margin: 10px 0; }
            .course.removed { border-left-color: #cc3333; }
            .course-code { font-weight: bold; font-size: 1.1em; }
            .course-name { color: #333; margin: 5px 0; }
            .course-meta { color: #666; font-size: 0.9em; }
            a { color: #0066cc; }
            .footer { margin-top: 40px; padding-top: 20px; border-top: 1px solid #ddd; color: #666; font-size: 0.85em; }
        "#);
        html.push_str("</style></head><body>");

        html.push_str("<h1>UiO Course Availability Changes</h1>");

        if !diff.added.is_empty() {
            html.push_str(&format!("<h2>New Courses Available ({})</h2>", diff.added.len()));
            for course in &diff.added {
                html.push_str(&format_course_html(course, false));
            }
        }

        if !diff.removed.is_empty() {
            html.push_str(&format!(
                "<h2>Courses No Longer Available ({})</h2>",
                diff.removed.len()
            ));
            for course in &diff.removed {
                html.push_str(&format_course_html(course, true));
            }
        }

        html.push_str(r#"<div class="footer">"#);
        html.push_str("This notification was sent by UiOBot - Course Availability Monitor.<br>");
        html.push_str(r#"<a href="https://www.uio.no/studier/emner/ledige-plasser/">View all available courses</a>"#);
        html.push_str("</div>");
        html.push_str("</body></html>");

        (subject, html)
    }
}

fn format_course_html(course: &Course, is_removed: bool) -> String {
    let class = if is_removed { "course removed" } else { "course" };
    let mut html = format!(r#"<div class="{}">"#, class);

    if !course.url.is_empty() {
        html.push_str(&format!(
            r#"<div class="course-code"><a href="{}">{}</a></div>"#,
            course.url, course.code
        ));
    } else {
        html.push_str(&format!(
            r#"<div class="course-code">{}</div>"#,
            course.code
        ));
    }

    html.push_str(&format!(
        r#"<div class="course-name">{}</div>"#,
        course.name
    ));
    html.push_str(&format!(
        r#"<div class="course-meta">{} points | {}</div>"#,
        course.points, course.faculty
    ));
    html.push_str("</div>");
    html
}

#[derive(Serialize)]
struct ResendEmail {
    from: String,
    to: Vec<String>,
    subject: String,
    html: String,
}

#[async_trait]
impl Notifier for EmailNotifier {
    fn name(&self) -> &'static str {
        "email"
    }

    async fn notify(&self, diff: &ScrapeDiff) -> Result<()> {
        if diff.is_empty() {
            return Ok(());
        }

        let (subject, html) = self.build_email_content(diff);
        let recipients_str = self.to.join(", ");

        debug!("Sending email from '{}' to [{}]", self.from, recipients_str);

        let email = ResendEmail {
            from: self.from.clone(),
            to: self.to.clone(),
            subject,
            html,
        };

        let response = self
            .client
            .post(RESEND_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&email)
            .send()
            .await
            .context("Failed to send email request to Resend API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Resend API error (HTTP {}): {}\n\
                 Check that your RESEND_API_KEY is valid and --email-from uses a verified domain.",
                status,
                error_text
            );
        }

        info!(
            "Email sent successfully to {} recipient(s): {}",
            self.to.len(),
            recipients_str
        );
        Ok(())
    }
}
