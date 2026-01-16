use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, instrument};

use super::Notifier;
use crate::models::{Course, ScrapeDiff};

pub struct ConsoleNotifier;

impl ConsoleNotifier {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConsoleNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Notifier for ConsoleNotifier {
    fn name(&self) -> &'static str {
        "console"
    }

    #[instrument(skip(self, diff), fields(
        notifier = "console",
        added = diff.added.len(),
        removed = diff.removed.len()
    ))]
    async fn notify(&self, diff: &ScrapeDiff) -> Result<()> {
        if diff.is_empty() {
            debug!("No changes to notify, skipping console output");
            return Ok(());
        }

        debug!(
            added_codes = ?diff.added.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(),
            removed_codes = ?diff.removed.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(),
            "Writing changes to console"
        );

        println!("\n{}", "=".repeat(60));
        println!("COURSE AVAILABILITY CHANGES");
        println!("{}", "=".repeat(60));

        if !diff.added.is_empty() {
            println!("\n[+] NEW COURSES AVAILABLE ({}):", diff.added.len());
            println!("{}", "-".repeat(40));
            for course in &diff.added {
                print_course(course, "+");
            }
        }

        if !diff.removed.is_empty() {
            println!("\n[-] COURSES NO LONGER AVAILABLE ({}):", diff.removed.len());
            println!("{}", "-".repeat(40));
            for course in &diff.removed {
                print_course(course, "-");
            }
        }

        println!("\n{}", "=".repeat(60));

        info!(
            notifier = "console",
            added_count = diff.added.len(),
            removed_count = diff.removed.len(),
            total_changes = diff.total_changes(),
            "Console notification displayed"
        );

        Ok(())
    }
}

fn print_course(course: &Course, prefix: &str) {
    println!(
        "[{}] {} - {}",
        prefix, course.code, course.name
    );
    println!(
        "    Points: {} | Faculty: {}",
        course.points, course.faculty
    );
    if !course.url.is_empty() {
        println!("    URL: {}", course.url);
    }
    println!();
}
