mod config;
mod course_scraper;
mod db;
mod diff;
mod models;
mod notifier;
mod web;

use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::time::interval;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use config::{validate_interval, Cli, Command, Config, PointsFilter};
use course_scraper::CourseScraper;
use db::{Database, RunLog};
use diff::filter_changes;
use models::{Course, ScrapeDiff};
use notifier::{ConsoleNotifier, EmailNotifier, Notifier, NotifierChain};

#[tokio::main]
async fn main() -> ExitCode {
    // Load .env file if it exists
    dotenvy::dotenv().ok();

    let cli = Cli::parse_args();

    let result = match cli.command {
        Command::Check { config } => run_check(config).await,
        Command::Start { config, interval } => run_start(config, interval).await,
        Command::TestEmail { to, from } => run_test_email(to, from).await,
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
    let db = open_database(&config).await?;
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
    let db = open_database(&config).await?;
    let filter = config.points_filter();
    let notifiers = build_notifiers(&config)?;
    let port = config.port;

    // Start web server in background
    let web_router = web::create_router(db);
    tokio::spawn(async move {
        if let Err(e) = web::start_server(web_router, port).await {
            error!(error = %e, "Web server failed");
        }
    });

    // Re-open database for scrape loop (web server took ownership)
    let db = open_database(&config).await?;

    let mut ticker = interval(Duration::from_secs(interval_secs));

    info!(
        interval_secs = interval_secs,
        notifier_count = notifiers.len(),
        db_type = %db.db_type(),
        port = port,
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

async fn run_test_email(to: String, from: String) -> Result<()> {
    // Initialize minimal logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Get API key from environment
    let api_key = env::var("RESEND_API_KEY").context(
        "RESEND_API_KEY environment variable not set.\n\
         Get an API key from https://resend.com and set it in your .env file.",
    )?;

    // Parse recipients
    let recipients: Vec<String> = to
        .split(',')
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect();

    if recipients.is_empty() {
        anyhow::bail!("No valid email recipients provided");
    }

    info!(
        from = %from,
        to = ?recipients,
        "Sending test email notification"
    );

    // Create sample courses for demo
    let demo_diff = ScrapeDiff::new(
        vec![
            Course::new(
                "TEST1000".to_string(),
                "Introduction to Test Notifications".to_string(),
                2.5,
                "https://www.uio.no/studier/emner/ledige-plasser/".to_string(),
                "Test Faculty".to_string(),
            ),
            Course::new(
                "TEST2000".to_string(),
                "Advanced Email Testing".to_string(),
                5.0,
                "https://www.uio.no/studier/emner/ledige-plasser/".to_string(),
                "Test Faculty".to_string(),
            ),
        ],
        vec![Course::new(
            "OLD1000".to_string(),
            "Previously Available Course".to_string(),
            10.0,
            "https://www.uio.no/studier/emner/ledige-plasser/".to_string(),
            "Test Faculty".to_string(),
        )],
    );

    // Send the test email
    let notifier = EmailNotifier::new(api_key, from, recipients);
    notifier.notify(&demo_diff).await?;

    info!("Test email sent successfully!");
    Ok(())
}

/// Open database based on configuration (local SQLite or Turso)
async fn open_database(config: &Config) -> Result<Database> {
    if let Some(ref db_url) = config.database_url {
        let auth_token = config
            .database_auth_token
            .as_ref()
            .context("DATABASE_AUTH_TOKEN is required when using DATABASE_URL")?;

        info!(
            db_url = %db_url,
            "Using Turso remote database"
        );

        Database::open_turso(db_url, auth_token).await
    } else {
        info!(
            db_path = %config.db.display(),
            "Using local SQLite database"
        );

        Database::open(&config.db).await
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
    // Log database configuration
    if config.uses_turso() {
        info!(
            url = %config.url,
            db_type = "turso",
            db_url = %config.database_url.as_deref().unwrap_or("not set"),
            filter = %config.points_filter().description(),
            "Core configuration"
        );
    } else {
        info!(
            url = %config.url,
            db_type = "sqlite",
            db_path = %config.db.display(),
            filter = %config.points_filter().description(),
            "Core configuration"
        );
    }

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
        db_type = %db.db_type(),
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
    let sync_result = db.sync_courses(&courses).await?;

    info!(
        cycle_number = cycle_number,
        sync_duration_ms = sync_start.elapsed().as_millis(),
        is_first_run = sync_result.is_first_run,
        total_courses = sync_result.total_courses,
        raw_added = sync_result.added.len(),
        raw_removed = sync_result.removed.len(),
        "Sync phase completed"
    );

    // Apply filter (even on first run, to track what would have been notified)
    let filtered_diff = filter_changes(&sync_result, filter);

    // Prepare notification tracking
    let mut notification_sent = false;

    if sync_result.is_first_run {
        info!(
            cycle_number = cycle_number,
            courses_stored = courses.len(),
            total_duration_ms = cycle_start.elapsed().as_millis(),
            "First run completed - database initialized, no notifications sent"
        );
    } else if !sync_result.has_changes() {
        info!(
            cycle_number = cycle_number,
            total_courses = sync_result.total_courses,
            total_duration_ms = cycle_start.elapsed().as_millis(),
            "No changes detected"
        );
    } else {
        debug!(
            cycle_number = cycle_number,
            added_courses = ?sync_result.added.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
            removed_courses = ?sync_result.removed.iter().map(|c| format!("{}({:.1}pts)", c.code, c.points)).collect::<Vec<_>>(),
            "Raw changes before filtering"
        );

        if filtered_diff.is_empty() {
            info!(
                cycle_number = cycle_number,
                filter = %filter.description(),
                raw_added = sync_result.added.len(),
                raw_removed = sync_result.removed.len(),
                total_duration_ms = cycle_start.elapsed().as_millis(),
                "No changes match filter criteria - no notifications sent"
            );
        } else {
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

            // Consider notification sent if at least one succeeded
            notification_sent = success_count > 0;

            info!(
                cycle_number = cycle_number,
                notify_duration_ms = notify_start.elapsed().as_millis(),
                notifiers_success = success_count,
                notifiers_failed = failure_count,
                "Notification phase completed"
            );
        }
    }

    // Log this run to the database
    let run_log = RunLog {
        total_courses_fetched: courses.len(),
        raw_added_count: sync_result.added.len(),
        raw_removed_count: sync_result.removed.len(),
        filtered_added_count: filtered_diff.added.len(),
        filtered_removed_count: filtered_diff.removed.len(),
        filter_used: filter.description(),
        notification_sent,
        is_first_run: sync_result.is_first_run,
        added_courses: filtered_diff.added.iter().map(|c| c.code.clone()).collect(),
        removed_courses: filtered_diff.removed.iter().map(|c| c.code.clone()).collect(),
        duration_ms: cycle_start.elapsed().as_millis() as u64,
    };

    if let Err(e) = db.log_run(&run_log).await {
        warn!(
            cycle_number = cycle_number,
            error = %e,
            "Failed to log run to database"
        );
    }

    info!(
        cycle_number = cycle_number,
        total_duration_ms = cycle_start.elapsed().as_millis(),
        notification_sent = notification_sent,
        changes_added = filtered_diff.added.len(),
        changes_removed = filtered_diff.removed.len(),
        "Scrape cycle completed"
    );

    Ok(())
}
