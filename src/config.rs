use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_URL: &str = "https://www.uio.no/studier/emner/ledige-plasser/";

#[derive(Parser, Debug, Clone)]
#[command(name = "uiobot")]
#[command(about = "UiO Course Availability Scraper - monitors course availability and notifies on changes")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Run a single check and exit
    Check {
        #[command(flatten)]
        config: Config,
    },
    /// Start the bot and run continuously
    Start {
        #[command(flatten)]
        config: Config,

        /// Scrape interval in seconds (minimum 10)
        #[arg(short, long, default_value = "60")]
        interval: u64,
    },
}

#[derive(Parser, Debug, Clone)]
pub struct Config {
    /// URL to scrape
    #[arg(short, long, default_value = DEFAULT_URL)]
    pub url: String,

    /// Database file path
    #[arg(short, long, default_value = "uiobot.db")]
    pub db: PathBuf,

    /// Filter: exact points value (e.g., 2.5)
    #[arg(long, value_name = "POINTS")]
    pub points_exact: Option<f32>,

    /// Filter: maximum points (inclusive)
    #[arg(long, value_name = "POINTS")]
    pub points_max: Option<f32>,

    /// Filter: minimum points (inclusive)
    #[arg(long, value_name = "POINTS")]
    pub points_min: Option<f32>,

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Email addresses to send notifications to (comma-separated)
    /// Example: --email-to "user1@example.com,user2@example.com"
    #[arg(long, env = "UIOBOT_EMAIL_TO", value_name = "EMAILS")]
    pub email_to: Option<String>,

    /// Email address to send from (must be verified domain in Resend)
    /// Example: --email-from "UiOBot <notifications@yourdomain.com>"
    #[arg(long, env = "UIOBOT_EMAIL_FROM")]
    pub email_from: Option<String>,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

impl Config {
    /// Parse the comma-separated email_to string into a list of emails
    pub fn email_recipients(&self) -> Vec<String> {
        self.email_to
            .as_ref()
            .map(|s| {
                s.split(',')
                    .map(|e| e.trim().to_string())
                    .filter(|e| !e.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if email notifications are enabled
    pub fn email_enabled(&self) -> bool {
        self.email_to.is_some() && !self.email_recipients().is_empty()
    }

    /// Validate the configuration and return errors if invalid
    pub fn validate(&self) -> Result<()> {
        // Validate URL
        if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
            bail!(
                "Invalid URL '{}': must start with http:// or https://",
                self.url
            );
        }

        // Validate points filter
        if let (Some(min), Some(max)) = (self.points_min, self.points_max) {
            if min > max {
                bail!(
                    "Invalid points filter: --points-min ({}) cannot be greater than --points-max ({})",
                    min,
                    max
                );
            }
        }

        if let Some(exact) = self.points_exact {
            if exact < 0.0 {
                bail!("Invalid points filter: --points-exact cannot be negative");
            }
        }

        // Validate email configuration
        if self.email_enabled() {
            // Validate email_from is set
            if self.email_from.is_none() {
                bail!(
                    "Email notifications require --email-from to be set.\n\
                     Set it via CLI flag or UIOBOT_EMAIL_FROM environment variable.\n\
                     Example: --email-from \"UiOBot <noreply@yourdomain.com>\""
                );
            }

            // Validate email formats
            let recipients = self.email_recipients();
            for email in &recipients {
                if !is_valid_email(email) {
                    bail!(
                        "Invalid email address in --email-to: '{}'\n\
                         Expected format: user@domain.com",
                        email
                    );
                }
            }

            // Validate from address (can be "Name <email>" or just "email")
            if let Some(ref from) = self.email_from {
                let email_part = extract_email_from_address(from);
                if !is_valid_email(&email_part) {
                    bail!(
                        "Invalid email address in --email-from: '{}'\n\
                         Expected format: \"Name <email@domain.com>\" or \"email@domain.com\"",
                        from
                    );
                }
            }
        }

        Ok(())
    }

    pub fn points_filter(&self) -> PointsFilter {
        if let Some(exact) = self.points_exact {
            PointsFilter::Exact(exact)
        } else if self.points_min.is_some() || self.points_max.is_some() {
            PointsFilter::Range {
                min: self.points_min,
                max: self.points_max,
            }
        } else {
            PointsFilter::None
        }
    }
}

/// Simple email validation (not RFC 5322 compliant but good enough)
fn is_valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() {
        return false;
    }

    // Must contain exactly one @
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return false;
    }

    let local = parts[0];
    let domain = parts[1];

    // Local part must not be empty
    if local.is_empty() {
        return false;
    }

    // Domain must contain at least one dot and not be empty
    if domain.is_empty() || !domain.contains('.') {
        return false;
    }

    true
}

/// Extract email from "Name <email>" format, or return as-is if just email
fn extract_email_from_address(address: &str) -> String {
    let address = address.trim();
    if let Some(start) = address.find('<') {
        if let Some(end) = address.find('>') {
            return address[start + 1..end].trim().to_string();
        }
    }
    address.to_string()
}

/// Validate the interval for the start command
pub fn validate_interval(interval: u64) -> Result<()> {
    if interval < 10 {
        bail!(
            "Invalid interval: {} seconds is too short. Minimum is 10 seconds.\n\
             The UiO website updates approximately every minute, so shorter intervals are not useful.",
            interval
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum PointsFilter {
    None,
    Exact(f32),
    Range { min: Option<f32>, max: Option<f32> },
}

impl PointsFilter {
    pub fn matches(&self, points: f32) -> bool {
        match self {
            PointsFilter::None => true,
            PointsFilter::Exact(exact) => (points - exact).abs() < 0.01,
            PointsFilter::Range { min, max } => {
                let above_min = min.map_or(true, |m| points >= m);
                let below_max = max.map_or(true, |m| points <= m);
                above_min && below_max
            }
        }
    }

    pub fn description(&self) -> String {
        match self {
            PointsFilter::None => "all courses".to_string(),
            PointsFilter::Exact(v) => format!("courses with exactly {} points", v),
            PointsFilter::Range { min, max } => match (min, max) {
                (Some(min), Some(max)) => format!("courses with {}-{} points", min, max),
                (Some(min), None) => format!("courses with >= {} points", min),
                (None, Some(max)) => format!("courses with <= {} points", max),
                (None, None) => "all courses".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_email() {
        assert!(is_valid_email("user@example.com"));
        assert!(is_valid_email("user.name@example.co.uk"));
        assert!(is_valid_email("user+tag@example.com"));
        assert!(!is_valid_email("invalid"));
        assert!(!is_valid_email("@example.com"));
        assert!(!is_valid_email("user@"));
        assert!(!is_valid_email("user@localhost"));
        assert!(!is_valid_email(""));
    }

    #[test]
    fn test_extract_email_from_address() {
        assert_eq!(
            extract_email_from_address("UiOBot <bot@example.com>"),
            "bot@example.com"
        );
        assert_eq!(
            extract_email_from_address("bot@example.com"),
            "bot@example.com"
        );
        assert_eq!(
            extract_email_from_address("  Name <email@test.com>  "),
            "email@test.com"
        );
    }

    #[test]
    fn test_email_recipients() {
        let config = Config {
            url: "https://example.com".to_string(),
            db: PathBuf::from("test.db"),
            points_exact: None,
            points_max: None,
            points_min: None,
            verbose: false,
            email_to: Some("a@b.com, c@d.com, e@f.com".to_string()),
            email_from: None,
        };

        let recipients = config.email_recipients();
        assert_eq!(recipients.len(), 3);
        assert_eq!(recipients[0], "a@b.com");
        assert_eq!(recipients[1], "c@d.com");
        assert_eq!(recipients[2], "e@f.com");
    }
}
