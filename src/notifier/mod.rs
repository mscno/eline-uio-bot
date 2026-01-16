mod console;
mod email;

pub use console::ConsoleNotifier;
pub use email::EmailNotifier;

use anyhow::Result;
use async_trait::async_trait;

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
        self.notifiers.push(Box::new(notifier));
    }

    pub async fn notify_all(&self, diff: &ScrapeDiff) -> Vec<(&'static str, Result<()>)> {
        let mut results = Vec::new();
        for notifier in &self.notifiers {
            let result = notifier.notify(diff).await;
            results.push((notifier.name(), result));
        }
        results
    }
}

impl Default for NotifierChain {
    fn default() -> Self {
        Self::new()
    }
}
