mod config;
mod course_scraper;
mod db;
mod diff;
mod models;
mod notifier;

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::interval;
use tracing::{error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use config::{validate_interval, Cli, Command, Config, PointsFilter};
use course_scraper::CourseScraper;
use db::Database;
use diff::filter_changes;
use notifier::{ConsoleNotifier, EmailNotifier, NotifierChain};

#[tokio::main]
async fn main() -> ExitCode {
    // Load .env file if it exists
    dotenvy::dotenv().ok();

    let cli = Cli::parse_args();

    let result = match cli.command {
        Command::Check { config } => run_check(config).await,
        Command::Start { config, interval } => run_start(config, interval).await,
    };

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run_check(config: Config) -> Result<()> {
    init_logging(config.verbose);

    // Validate configuration
    config.validate()?;

    info!("Running single check");
    log_config(&config);

    let scraper = CourseScraper::new(config.url.clone());
    let db = Database::open(&config.db)?;
    let filter = config.points_filter();
    let notifiers = build_notifiers(&config)?;

    run_scrape_cycle(&scraper, &db, &filter, &notifiers).await
}

async fn run_start(config: Config, interval_secs: u64) -> Result<()> {
    init_logging(config.verbose);

    // Validate configuration
    config.validate()?;
    validate_interval(interval_secs)?;

    info!("Starting UiO Course Availability Bot");
    log_config(&config);
    info!("Interval: {} seconds", interval_secs);

    let scraper = CourseScraper::new(config.url.clone());
    let db = Database::open(&config.db)?;
    let filter = config.points_filter();
    let notifiers = build_notifiers(&config)?;

    let mut ticker = interval(Duration::from_secs(interval_secs));

    info!("Starting scrape loop (Ctrl+C to stop)");

    loop {
        ticker.tick().await;

        if let Err(e) = run_scrape_cycle(&scraper, &db, &filter, &notifiers).await {
            error!("Scrape cycle failed: {:#}", e);
        }
    }
}

fn init_logging(verbose: bool) {
    let log_level = if verbose { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn log_config(config: &Config) {
    info!("URL: {}", config.url);
    info!("Database: {}", config.db.display());
    info!("Filter: {}", config.points_filter().description());

    if config.email_enabled() {
        let recipients = config.email_recipients();
        info!(
            "Email notifications: enabled ({} recipient{})",
            recipients.len(),
            if recipients.len() == 1 { "" } else { "s" }
        );
        info!("  From: {}", config.email_from.as_deref().unwrap_or("not set"));
        info!("  To: {}", recipients.join(", "));
    } else {
        info!("Email notifications: disabled");
    }
}

fn build_notifiers(config: &Config) -> Result<NotifierChain> {
    let mut notifiers = NotifierChain::new();

    // Always add console notifier
    notifiers.add(ConsoleNotifier::new());

    // Add email notifier if configured
    if config.email_enabled() {
        let api_key = env::var("RESEND_API_KEY").context(
            "RESEND_API_KEY environment variable not set.\n\
             To enable email notifications:\n\
             1. Get an API key from https://resend.com\n\
             2. Add RESEND_API_KEY=re_xxxxx to your .env file\n\
             3. Or export RESEND_API_KEY=re_xxxxx in your shell",
        )?;

        let from = config
            .email_from
            .clone()
            .context("--email-from is required when using email notifications")?;

        let recipients = config.email_recipients();

        info!(
            "Email notifier configured: from='{}', to={:?}",
            from, recipients
        );

        notifiers.add(EmailNotifier::new(api_key, from, recipients));
    }

    Ok(notifiers)
}

async fn run_scrape_cycle(
    scraper: &CourseScraper,
    db: &Database,
    filter: &PointsFilter,
    notifiers: &NotifierChain,
) -> Result<()> {
    info!("Starting scrape cycle");

    // Fetch courses
    let courses = match scraper.fetch_courses().await {
        Ok(courses) => courses,
        Err(e) => {
            error!("Failed to fetch courses: {:#}", e);
            return Err(e);
        }
    };

    info!("Fetched {} courses", courses.len());

    // Sync with database
    let sync_result = db.sync_courses(&courses)?;

    if sync_result.is_first_run {
        info!(
            "First run completed, database initialized with {} courses",
            courses.len()
        );
        return Ok(());
    }

    if !sync_result.has_changes() {
        info!("No changes detected");
        return Ok(());
    }

    info!(
        "Changes detected: {} added, {} removed",
        sync_result.added.len(),
        sync_result.removed.len()
    );

    // Apply filter
    let filtered_diff = filter_changes(&sync_result, filter);

    if filtered_diff.is_empty() {
        info!("No changes match the filter criteria");
        return Ok(());
    }

    info!(
        "After filtering: {} added, {} removed",
        filtered_diff.added.len(),
        filtered_diff.removed.len()
    );

    // Send notifications
    let results = notifiers.notify_all(&filtered_diff).await;
    for (name, result) in results {
        match result {
            Ok(_) => info!("Notifier '{}' succeeded", name),
            Err(e) => warn!("Notifier '{}' failed: {:#}", name, e),
        }
    }

    Ok(())
}
