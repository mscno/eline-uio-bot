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
    /// Send a test email notification to verify email configuration
    TestEmail {
        /// Email addresses to send to (comma-separated)
        #[arg(short, long, env = "UIOBOT_EMAIL_TO", required = true)]
        to: String,

        /// Email address to send from (must be verified domain in Resend)
        #[arg(short, long, env = "UIOBOT_EMAIL_FROM", required = true)]
        from: String,
    },
    /// Send a test SMS notification to verify Twilio configuration
    TestSms {
        /// Phone numbers to send to (comma-separated Norwegian numbers)
        #[arg(short, long, env = "UIOBOT_SMS_TO", required = true)]
        to: String,

        /// Twilio phone number to send SMS from
        #[arg(short, long, env = "TWILIO_FROM_NUMBER", required = true)]
        from: String,
    },
}

#[derive(Parser, Debug, Clone)]
pub struct Config {
    /// URL to scrape
    #[arg(short, long, default_value = DEFAULT_URL)]
    pub url: String,

    /// Database file path (for local SQLite, ignored if --database-url is set)
    #[arg(short, long, default_value = "uiobot.db")]
    pub db: PathBuf,

    /// Turso/LibSQL database URL (e.g., libsql://your-db.turso.io)
    /// When set, uses remote database instead of local SQLite file
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// Turso/LibSQL authentication token (required when using --database-url)
    #[arg(long, env = "DATABASE_AUTH_TOKEN")]
    pub database_auth_token: Option<String>,

    /// Filter: exact points value (e.g., 2.5)
    #[arg(long, env = "UIOBOT_POINTS_EXACT", value_name = "POINTS")]
    pub points_exact: Option<f32>,

    /// Filter: maximum points (inclusive)
    #[arg(long, env = "UIOBOT_POINTS_MAX", value_name = "POINTS")]
    pub points_max: Option<f32>,

    /// Filter: minimum points (inclusive)
    #[arg(long, env = "UIOBOT_POINTS_MIN", value_name = "POINTS")]
    pub points_min: Option<f32>,

    /// Filter: points filter expression (alternative to individual flags)
    /// Formats: "2.5" (exact), ">=5" (min), "<=10" (max), "5-10" (range)
    #[arg(long, env = "UIOBOT_POINTS_FILTER", value_name = "FILTER")]
    pub points_filter_expr: Option<String>,

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

    /// Web server port (only used in start mode)
    #[arg(long, env = "UIOBOT_PORT", default_value = "3000")]
    pub port: u16,

    /// SMS phone numbers to send notifications to (comma-separated Norwegian numbers)
    /// Example: --sms-to "+4712345678,+4787654321"
    #[arg(long, env = "UIOBOT_SMS_TO", value_name = "PHONES")]
    pub sms_to: Option<String>,

    /// Twilio phone number to send SMS from
    #[arg(long, env = "TWILIO_FROM_NUMBER")]
    pub sms_from: Option<String>,
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

    /// Check if using Turso/remote database
    pub fn uses_turso(&self) -> bool {
        self.database_url.is_some()
    }

    /// Parse the comma-separated sms_to string into a list of normalized phone numbers
    pub fn sms_recipients(&self) -> Vec<String> {
        self.sms_to
            .as_ref()
            .map(|s| {
                s.split(',')
                    .filter_map(|p| normalize_norwegian_phone(p.trim()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if SMS notifications are enabled
    pub fn sms_enabled(&self) -> bool {
        self.sms_to.is_some() && !self.sms_recipients().is_empty()
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

        // Validate database configuration
        if let Some(ref db_url) = self.database_url {
            // Validate database URL format
            if !db_url.starts_with("libsql://") && !db_url.starts_with("https://") {
                bail!(
                    "Invalid database URL '{}': must start with libsql:// or https://\n\
                     Example: libsql://your-database.turso.io",
                    db_url
                );
            }

            // Require auth token for remote databases
            if self.database_auth_token.is_none() {
                bail!(
                    "Turso database URL requires --database-auth-token to be set.\n\
                     Set it via CLI flag or DATABASE_AUTH_TOKEN environment variable.\n\
                     You can get your token from: turso db tokens create <database-name>"
                );
            }
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

        // Validate SMS configuration
        if self.sms_enabled() {
            // Validate sms_from is set
            if self.sms_from.is_none() {
                bail!(
                    "SMS notifications require --sms-from to be set.\n\
                     Set it via CLI flag or TWILIO_FROM_NUMBER environment variable.\n\
                     Example: --sms-from \"+4712345678\""
                );
            }

            // Validate from number is a valid Twilio phone number (U.S. or Norwegian)
            if let Some(ref from) = self.sms_from {
                if normalize_twilio_phone(from).is_none() {
                    bail!(
                        "Invalid Twilio phone number in --sms-from: '{}'\n\
                         Expected format: +1XXXXXXXXXX (U.S.) or +47XXXXXXXX (Norwegian)",
                        from
                    );
                }
            }

            // Validate recipient numbers (invalid ones are filtered out, but warn if none are valid)
            if let Some(ref sms_to) = self.sms_to {
                let raw_numbers: Vec<&str> = sms_to.split(',').map(|s| s.trim()).collect();
                let valid_numbers = self.sms_recipients();

                if valid_numbers.is_empty() && !raw_numbers.is_empty() {
                    bail!(
                        "No valid Norwegian phone numbers in --sms-to: '{}'\n\
                         Expected format: +4712345678, 4712345678, or 12345678 (8 digits)",
                        sms_to
                    );
                }
            }
        }

        Ok(())
    }

    pub fn points_filter(&self) -> PointsFilter {
        // First check if points_filter_expr is set (takes precedence)
        if let Some(ref expr) = self.points_filter_expr {
            if let Some(filter) = parse_points_filter_expr(expr) {
                return filter;
            }
        }

        // Fall back to individual flags
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

/// Parse a points filter expression string
/// Formats:
/// - "2.5" -> exact match
/// - ">=5" or "5+" -> minimum
/// - "<=10" or "10-" -> maximum
/// - "5-10" -> range (min-max)
fn parse_points_filter_expr(expr: &str) -> Option<PointsFilter> {
    let expr = expr.trim();

    if expr.is_empty() {
        return Some(PointsFilter::None);
    }

    // Range format: "5-10" or "5.0-10.0"
    if let Some(dash_pos) = expr.find('-') {
        // Make sure it's not just a negative number or suffix like "10-"
        if dash_pos > 0 && dash_pos < expr.len() - 1 {
            let left = expr[..dash_pos].trim();
            let right = expr[dash_pos + 1..].trim();

            if let (Ok(min), Ok(max)) = (left.parse::<f32>(), right.parse::<f32>()) {
                return Some(PointsFilter::Range {
                    min: Some(min),
                    max: Some(max),
                });
            }
        }
    }

    // Minimum format: ">=5" or ">5"
    if expr.starts_with(">=") {
        if let Ok(min) = expr[2..].trim().parse::<f32>() {
            return Some(PointsFilter::Range { min: Some(min), max: None });
        }
    }
    if expr.starts_with('>') && !expr.starts_with(">=") {
        if let Ok(min) = expr[1..].trim().parse::<f32>() {
            // For ">5", we use 5.01 as a slight offset (not strictly greater, but close)
            return Some(PointsFilter::Range { min: Some(min), max: None });
        }
    }

    // Minimum format: "5+"
    if expr.ends_with('+') {
        if let Ok(min) = expr[..expr.len() - 1].trim().parse::<f32>() {
            return Some(PointsFilter::Range { min: Some(min), max: None });
        }
    }

    // Maximum format: "<=10" or "<10"
    if expr.starts_with("<=") {
        if let Ok(max) = expr[2..].trim().parse::<f32>() {
            return Some(PointsFilter::Range { min: None, max: Some(max) });
        }
    }
    if expr.starts_with('<') && !expr.starts_with("<=") {
        if let Ok(max) = expr[1..].trim().parse::<f32>() {
            return Some(PointsFilter::Range { min: None, max: Some(max) });
        }
    }

    // Maximum format: "10-" (trailing dash)
    if expr.ends_with('-') && expr.len() > 1 {
        if let Ok(max) = expr[..expr.len() - 1].trim().parse::<f32>() {
            return Some(PointsFilter::Range { min: None, max: Some(max) });
        }
    }

    // Exact format: just a number
    if let Ok(exact) = expr.parse::<f32>() {
        return Some(PointsFilter::Exact(exact));
    }

    // Couldn't parse
    None
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

/// Normalize a Twilio phone number (U.S. or Norwegian)
/// U.S. format: +1XXXXXXXXXX (10 digits after +1)
/// Norwegian format: +47XXXXXXXX (8 digits after +47)
/// Returns None if invalid
pub fn normalize_twilio_phone(phone: &str) -> Option<String> {
    // Remove all whitespace and dashes
    let cleaned: String = phone.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();

    // Must start with + for Twilio numbers
    if !cleaned.starts_with('+') {
        return None;
    }

    let digits = &cleaned[1..];

    // Must be all digits after +
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // U.S. number: +1 followed by 10 digits
    if digits.starts_with('1') && digits.len() == 11 {
        return Some(cleaned);
    }

    // Norwegian number: +47 followed by 8 digits
    if digits.starts_with("47") && digits.len() == 10 {
        let local = &digits[2..];
        let first_digit = local.chars().next()?;
        if matches!(first_digit, '2'..='9') {
            return Some(cleaned);
        }
    }

    None
}

/// Normalize a Norwegian phone number to +47XXXXXXXX format
/// Accepts: +4712345678, 4712345678, 12345678
/// Returns None if invalid
pub fn normalize_norwegian_phone(phone: &str) -> Option<String> {
    // Remove all whitespace and dashes
    let cleaned: String = phone.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();

    // Extract digits and optional leading +
    let (has_plus, digits): (bool, String) = if cleaned.starts_with('+') {
        (true, cleaned[1..].to_string())
    } else {
        (false, cleaned)
    };

    // Must be all digits after optional +
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Determine the 8-digit number
    let eight_digits = if has_plus && digits.starts_with("47") && digits.len() == 10 {
        // +4712345678 format
        digits[2..].to_string()
    } else if !has_plus && digits.starts_with("47") && digits.len() == 10 {
        // 4712345678 format
        digits[2..].to_string()
    } else if digits.len() == 8 {
        // 12345678 format (just 8 digits)
        digits
    } else {
        return None;
    };

    // Validate it's exactly 8 digits
    if eight_digits.len() != 8 || !eight_digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Norwegian mobile numbers start with 4 or 9
    // Norwegian landline numbers start with 2, 3, 5, 6, 7
    // All valid Norwegian numbers are 8 digits
    let first_digit = eight_digits.chars().next()?;
    if !matches!(first_digit, '2'..='9') {
        return None;
    }

    Some(format!("+47{}", eight_digits))
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
            database_url: None,
            database_auth_token: None,
            points_exact: None,
            points_max: None,
            points_min: None,
            points_filter_expr: None,
            verbose: false,
            email_to: Some("a@b.com, c@d.com, e@f.com".to_string()),
            email_from: None,
            port: 3000,
            sms_to: None,
            sms_from: None,
        };

        let recipients = config.email_recipients();
        assert_eq!(recipients.len(), 3);
        assert_eq!(recipients[0], "a@b.com");
        assert_eq!(recipients[1], "c@d.com");
        assert_eq!(recipients[2], "e@f.com");
    }

    #[test]
    fn test_parse_points_filter_expr_exact() {
        let filter = parse_points_filter_expr("2.5").unwrap();
        assert!(matches!(filter, PointsFilter::Exact(v) if (v - 2.5).abs() < 0.01));

        let filter = parse_points_filter_expr("10").unwrap();
        assert!(matches!(filter, PointsFilter::Exact(v) if (v - 10.0).abs() < 0.01));
    }

    #[test]
    fn test_parse_points_filter_expr_min() {
        let filter = parse_points_filter_expr(">=5").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: Some(v), max: None } if (v - 5.0).abs() < 0.01));

        let filter = parse_points_filter_expr("5+").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: Some(v), max: None } if (v - 5.0).abs() < 0.01));

        let filter = parse_points_filter_expr(">5").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: Some(v), max: None } if (v - 5.0).abs() < 0.01));
    }

    #[test]
    fn test_parse_points_filter_expr_max() {
        let filter = parse_points_filter_expr("<=10").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: None, max: Some(v) } if (v - 10.0).abs() < 0.01));

        let filter = parse_points_filter_expr("<10").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: None, max: Some(v) } if (v - 10.0).abs() < 0.01));

        let filter = parse_points_filter_expr("10-").unwrap();
        assert!(matches!(filter, PointsFilter::Range { min: None, max: Some(v) } if (v - 10.0).abs() < 0.01));
    }

    #[test]
    fn test_parse_points_filter_expr_range() {
        let filter = parse_points_filter_expr("5-10").unwrap();
        assert!(matches!(
            filter,
            PointsFilter::Range { min: Some(min), max: Some(max) }
            if (min - 5.0).abs() < 0.01 && (max - 10.0).abs() < 0.01
        ));

        let filter = parse_points_filter_expr("2.5-7.5").unwrap();
        assert!(matches!(
            filter,
            PointsFilter::Range { min: Some(min), max: Some(max) }
            if (min - 2.5).abs() < 0.01 && (max - 7.5).abs() < 0.01
        ));
    }

    #[test]
    fn test_parse_points_filter_expr_empty() {
        let filter = parse_points_filter_expr("").unwrap();
        assert!(matches!(filter, PointsFilter::None));

        let filter = parse_points_filter_expr("  ").unwrap();
        assert!(matches!(filter, PointsFilter::None));
    }

    #[test]
    fn test_points_filter_expr_takes_precedence() {
        let config = Config {
            url: "https://example.com".to_string(),
            db: PathBuf::from("test.db"),
            database_url: None,
            database_auth_token: None,
            points_exact: Some(10.0), // This should be ignored
            points_max: None,
            points_min: None,
            points_filter_expr: Some("2.5".to_string()), // This takes precedence
            verbose: false,
            email_to: None,
            email_from: None,
            port: 3000,
            sms_to: None,
            sms_from: None,
        };

        let filter = config.points_filter();
        assert!(matches!(filter, PointsFilter::Exact(v) if (v - 2.5).abs() < 0.01));
    }

    #[test]
    fn test_normalize_norwegian_phone() {
        // Valid formats (using numbers starting with 4 - mobile, or 2 - Oslo landline)
        assert_eq!(normalize_norwegian_phone("+4741234567"), Some("+4741234567".to_string()));
        assert_eq!(normalize_norwegian_phone("4741234567"), Some("+4741234567".to_string()));
        assert_eq!(normalize_norwegian_phone("41234567"), Some("+4741234567".to_string()));
        assert_eq!(normalize_norwegian_phone("+47 412 34 567"), Some("+4741234567".to_string()));
        assert_eq!(normalize_norwegian_phone("412-34-567"), Some("+4741234567".to_string()));

        // Valid first digits (2-9)
        assert_eq!(normalize_norwegian_phone("22345678"), Some("+4722345678".to_string()));
        assert_eq!(normalize_norwegian_phone("92345678"), Some("+4792345678".to_string()));
        assert_eq!(normalize_norwegian_phone("45678901"), Some("+4745678901".to_string()));

        // Invalid: wrong length
        assert_eq!(normalize_norwegian_phone("4234567"), None); // 7 digits
        assert_eq!(normalize_norwegian_phone("423456789"), None); // 9 digits
        assert_eq!(normalize_norwegian_phone("+474234567"), None); // +47 + 7 digits

        // Invalid: starts with 0 or 1
        assert_eq!(normalize_norwegian_phone("01234567"), None);
        assert_eq!(normalize_norwegian_phone("11234567"), None);

        // Invalid: non-Norwegian country code
        assert_eq!(normalize_norwegian_phone("+1234567890"), None);
        assert_eq!(normalize_norwegian_phone("+4641234567"), None);

        // Invalid: contains letters
        assert_eq!(normalize_norwegian_phone("+47412abc78"), None);
    }

    #[test]
    fn test_normalize_twilio_phone() {
        // Valid U.S. numbers
        assert_eq!(normalize_twilio_phone("+12025551234"), Some("+12025551234".to_string()));
        assert_eq!(normalize_twilio_phone("+1 202 555 1234"), Some("+12025551234".to_string()));
        assert_eq!(normalize_twilio_phone("+1-202-555-1234"), Some("+12025551234".to_string()));

        // Valid Norwegian numbers
        assert_eq!(normalize_twilio_phone("+4741234567"), Some("+4741234567".to_string()));
        assert_eq!(normalize_twilio_phone("+47 412 34 567"), Some("+4741234567".to_string()));

        // Invalid: missing + prefix
        assert_eq!(normalize_twilio_phone("12025551234"), None);
        assert_eq!(normalize_twilio_phone("4741234567"), None);

        // Invalid: wrong length
        assert_eq!(normalize_twilio_phone("+1202555123"), None); // U.S. too short
        assert_eq!(normalize_twilio_phone("+120255512345"), None); // U.S. too long
        assert_eq!(normalize_twilio_phone("+474123456"), None); // Norwegian too short

        // Invalid: other country codes
        assert_eq!(normalize_twilio_phone("+442071234567"), None); // UK
        assert_eq!(normalize_twilio_phone("+33123456789"), None); // France
    }
}
