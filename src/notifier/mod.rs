mod console;
mod email;
mod sms;

pub use console::ConsoleNotifier;
pub use email::EmailNotifier;
pub use sms::SmsNotifier;

use anyhow::Result;
use async_trait::async_trait;
use std::time::Instant;
use tracing::{debug, info, instrument};

use crate::models::ScrapeDiff;

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Get the name of this notifier for logging
    fn name(&self) -> &'static str;

    /// Send notification about course changes
    async fn notify(&self, diff: &ScrapeDiff) -> Result<()>;
}

/// Collection of notifiers that can be notified together
pub struct NotifierChain {
    notifiers: Vec<Box<dyn Notifier>>,
}

impl NotifierChain {
    pub fn new() -> Self {
        Self { notifiers: Vec::new() }
    }

    pub fn add<N: Notifier + 'static>(&mut self, notifier: N) {
        debug!(notifier = notifier.name(), "Adding notifier to chain");
        self.notifiers.push(Box::new(notifier));
    }

    pub fn len(&self) -> usize {
        self.notifiers.len()
    }

    #[instrument(skip(self, diff), fields(
        notifier_count = self.notifiers.len(),
        added = diff.added.len(),
        removed = diff.removed.len()
    ))]
    pub async fn notify_all(&self, diff: &ScrapeDiff) -> Vec<(&'static str, Result<()>)> {
        let start = Instant::now();
        let notifier_names: Vec<_> = self.notifiers.iter().map(|n| n.name()).collect();

        info!(
            notifiers = ?notifier_names,
            changes_added = diff.added.len(),
            changes_removed = diff.removed.len(),
            "Starting notification dispatch"
        );

        let mut results = Vec::new();
        for notifier in &self.notifiers {
            let notifier_start = Instant::now();
            let name = notifier.name();

            debug!(notifier = name, "Dispatching to notifier");

            let result = notifier.notify(diff).await;
            let success = result.is_ok();

            debug!(
                notifier = name,
                success = success,
                duration_ms = notifier_start.elapsed().as_millis(),
                "Notifier completed"
            );

            results.push((name, result));
        }

        let success_count = results.iter().filter(|(_, r)| r.is_ok()).count();
        let failure_count = results.len() - success_count;

        info!(
            total_notifiers = results.len(),
            success_count = success_count,
            failure_count = failure_count,
            total_duration_ms = start.elapsed().as_millis(),
            "Notification dispatch completed"
        );

        results
    }
}

impl Default for NotifierChain {
    fn default() -> Self {
        Self::new()
    }
}
