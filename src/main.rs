mod config;
mod course_scraper;
mod db;
mod diff;
mod models;
mod notifier;

use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::time::interval;
use tracing::{debug, error, info, warn, Level};
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

    info!(
        version = env!("CARGO_PKG_VERSION"),
        mode = "check",
        "Running single check"
    );
    log_config(&config);

    let scraper = CourseScraper::new(config.url.clone());
    let db = Database::open(&config.db)?;
    let filter = config.points_filter();
    let notifiers = build_notifiers(&config)?;

    info!(
        notifier_count = notifiers.len(),
        "Configuration loaded, starting check"
    );

    run_scrape_cycle(&scraper, &db, &filter, &notifiers).await
}

async fn run_start(config: Config, interval_secs: u64) -> Result<()> {
    init_logging(config.verbose);

    // Validate configuration
    config.validate()?;
    validate_interval(interval_secs)?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting UiO Course Availability Bot"
    );
    log_config(&config);
    info!(
        interval_secs = interval_secs,
        interval_human = format!("{}m {}s", interval_secs / 60, interval_secs % 60),
        "Scrape interval configured"
    );

    let scraper = CourseScraper::new(config.url.clone());
    let db = Database::open(&config.db)?;
    let filter = config.points_filter();
    let notifiers = build_notifiers(&config)?;

    let mut ticker = interval(Duration::from_secs(interval_secs));

    info!(
        interval_secs = interval_secs,
        notifier_count = notifiers.len(),
        "Entering scrape loop (Ctrl+C to stop)"
    );

    loop {
        ticker.tick().await;

        debug!("Ticker fired, starting new cycle");

        if let Err(e) = run_scrape_cycle(&scraper, &db, &filter, &notifiers).await {
            error!(
                error = %e,
                "Scrape cycle failed - will retry next interval"
            );
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
    info!(
        url = %config.url,
        db_path = %config.db.display(),
        filter = %config.points_filter().description(),
        "Core configuration"
    );

    if config.email_enabled() {
        let recipients = config.email_recipients();
        info!(
            email_enabled = true,
            email_from = %config.email_from.as_deref().unwrap_or("not set"),
            email_recipients = ?recipients,
            recipient_count = recipients.len(),
            "Email notification configuration"
        );
    } else {
        info!(
            email_enabled = false,
            "Email notifications disabled"
        );
    }
}

fn build_notifiers(config: &Config) -> Result<NotifierChain> {
    let mut notifiers = NotifierChain::new();

    // Always add console notifier
    notifiers.add(ConsoleNotifier::new());
    debug!(notifier = "console", "Added console notifier");

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
            notifier = "email",
            from = %from,
            recipients = ?recipients,
            recipient_count = recipients.len(),
            api_key_prefix = %api_key.chars().take(10).collect::<String>(),
            "Added email notifier"
        );

        notifiers.add(EmailNotifier::new(api_key, from, recipients));
    }

    info!(
        total_notifiers = notifiers.len(),
        "Notifier chain built"
    );

    Ok(notifiers)
}

async fn run_scrape_cycle(
    scraper: &CourseScraper,
    db: &Database,
    filter: &PointsFilter,
    notifiers: &NotifierChain,
) -> Result<()> {
    let cycle_start = Instant::now();
    static CYCLE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let cycle_number = CYCLE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

    info!(
        cycle_number = cycle_number,
        filter = %filter.description(),
        "Starting scrape cycle"
    );

    // Fetch courses
    let fetch_start = Instant::now();
    let courses = match scraper.fetch_courses().await {
        Ok(courses) => {
            info!(
                cycle_number = cycle_number,
                courses_fetched = courses.len(),
                fetch_duration_ms = fetch_start.elapsed().as_millis(),
                "Fetch phase completed"
            );
            courses
        }
        Err(e) => {
            error!(
                cycle_number = cycle_number,
                error = %e,
                fetch_duration_ms = fetch_start.elapsed().as_millis(),
                "Fetch phase failed"
            );
            return Err(e);
        }
    };

    // Sync with database
    let sync_start = Instant::now();
    let sync_result = db.sync_courses(&courses)?;

    info!(
        cycle_number = cycle_number,
        sync_duration_ms = sync_start.elapsed().as_millis(),
        is_first_run = sync_result.is_first_run,
        total_courses = sync_result.total_courses,
        raw_added = sync_result.added.len(),
        raw_removed = sync_result.removed.len(),
        "Sync phase completed"
    );

    if sync_result.is_first_run {
        info!(
            cycle_number = cycle_number,
            courses_stored = courses.len(),
            total_duration_ms = cycle_start.elapsed().as_millis(),
            "First run completed - database initialized, no notifications sent"
        );
        return Ok(());
    }

    if !sync_result.has_changes() {
        info!(
            cycle_number = cycle_number,
            total_courses = sync_result.total_courses,
            total_duration_ms = cycle_start.elapsed().as_millis(),
            "No changes detected"
        );
        return Ok(());
    }

    debug!(
        cycle_number = cycle_number,
        added_courses = ?sync_result.added.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
        removed_courses = ?sync_result.removed.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
        "Raw changes before filtering"
    );

    // Apply filter
    let filtered_diff = filter_changes(&sync_result, filter);

    if filtered_diff.is_empty() {
        info!(
            cycle_number = cycle_number,
            filter = %filter.description(),
            raw_added = sync_result.added.len(),
            raw_removed = sync_result.removed.len(),
            total_duration_ms = cycle_start.elapsed().as_millis(),
            "No changes match filter criteria - no notifications sent"
        );
        return Ok(());
    }

    info!(
        cycle_number = cycle_number,
        filtered_added = filtered_diff.added.len(),
        filtered_removed = filtered_diff.removed.len(),
        filter = %filter.description(),
        "Changes passed filter - sending notifications"
    );

    // Send notifications
    let notify_start = Instant::now();
    let results = notifiers.notify_all(&filtered_diff).await;

    let mut success_count = 0;
    let mut failure_count = 0;

    for (name, result) in &results {
        match result {
            Ok(_) => {
                success_count += 1;
                info!(
                    cycle_number = cycle_number,
                    notifier = %name,
                    added_count = filtered_diff.added.len(),
                    removed_count = filtered_diff.removed.len(),
                    "Notification sent successfully"
                );
            }
            Err(e) => {
                failure_count += 1;
                warn!(
                    cycle_number = cycle_number,
                    notifier = %name,
                    error = %e,
                    "Notification failed"
                );
            }
        }
    }

    info!(
        cycle_number = cycle_number,
        notify_duration_ms = notify_start.elapsed().as_millis(),
        total_duration_ms = cycle_start.elapsed().as_millis(),
        notifiers_success = success_count,
        notifiers_failed = failure_count,
        changes_added = filtered_diff.added.len(),
        changes_removed = filtered_diff.removed.len(),
        "Scrape cycle completed"
    );

    Ok(())
}
