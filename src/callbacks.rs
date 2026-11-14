//! Progress callback system (port of `callbacks.py`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::Error;

/// Progress tracking hooks. Every method has a no-op default.
#[async_trait]
pub trait ProgressCallback: Send + Sync {
    /// Called when scraping starts.
    async fn on_start(&self, _scraper_type: &str, _url: &str) {}

    /// Called during scraping; `percent` is in `0..=100`.
    async fn on_progress(&self, _message: &str, _percent: u8) {}

    /// Called on success with the scraped result serialized as JSON.
    async fn on_complete(&self, _scraper_type: &str, _result: &Value) {}

    /// Called when an error occurs.
    async fn on_error(&self, _error: &Error) {}
}

/// Does nothing (default callback).
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentCallback;

#[async_trait]
impl ProgressCallback for SilentCallback {}

/// Prints progress to stdout.
#[derive(Debug, Clone, Copy)]
pub struct ConsoleCallback {
    /// If `false`, only every 20% milestone is printed.
    pub verbose: bool,
}

impl ConsoleCallback {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }
}

impl Default for ConsoleCallback {
    fn default() -> Self {
        Self { verbose: true }
    }
}

#[async_trait]
impl ProgressCallback for ConsoleCallback {
    async fn on_start(&self, scraper_type: &str, url: &str) {
        println!("🚀 Starting {scraper_type} scraping: {url}");
    }

    async fn on_progress(&self, message: &str, percent: u8) {
        if self.verbose || percent.is_multiple_of(20) {
            println!("{}", progress_line(message, percent));
        }
    }

    async fn on_complete(&self, scraper_type: &str, _result: &Value) {
        println!("✅ Completed {scraper_type} scraping successfully!");
    }

    async fn on_error(&self, error: &Error) {
        println!("❌ Error: {error}");
    }
}

pub(crate) fn progress_line(message: &str, percent: u8) -> String {
    const BAR_LENGTH: usize = 30;
    let filled = (BAR_LENGTH * usize::from(percent.min(100))) / 100;
    format!(
        "[{}{}] {percent}% - {message}",
        "█".repeat(filled),
        "░".repeat(BAR_LENGTH - filled)
    )
}

/// Appends one JSON object per event to a file (JSON Lines).
#[derive(Debug)]
pub struct JsonLogCallback {
    log_file: PathBuf,
    logs: Mutex<Vec<Value>>,
}

impl JsonLogCallback {
    pub fn new(log_file: impl Into<PathBuf>) -> Self {
        Self {
            log_file: log_file.into(),
            logs: Mutex::new(Vec::new()),
        }
    }

    /// All entries logged so far.
    pub fn logs(&self) -> Vec<Value> {
        self.logs.lock().expect("log mutex poisoned").clone()
    }

    fn log(&self, event_type: &str, data: Value) {
        let mut entry = json!({
            "timestamp": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
            "event_type": event_type,
        });
        if let (Some(obj), Value::Object(extra)) = (entry.as_object_mut(), data) {
            obj.extend(extra);
        }

        let line = entry.to_string();
        self.logs.lock().expect("log mutex poisoned").push(entry);

        let res = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::warn!("Could not write to {}: {e}", self.log_file.display());
        }
    }
}

#[async_trait]
impl ProgressCallback for JsonLogCallback {
    async fn on_start(&self, scraper_type: &str, url: &str) {
        self.log("start", json!({ "scraper_type": scraper_type, "url": url }));
    }

    async fn on_progress(&self, message: &str, percent: u8) {
        self.log(
            "progress",
            json!({ "message": message, "percent": percent }),
        );
    }

    async fn on_complete(&self, scraper_type: &str, _result: &Value) {
        self.log("complete", json!({ "scraper_type": scraper_type }));
    }

    async fn on_error(&self, error: &Error) {
        self.log(
            "error",
            json!({ "error": error.to_string(), "error_type": error.type_name() }),
        );
    }
}

/// Fans every event out to several callbacks.
#[derive(Default)]
pub struct MultiCallback {
    callbacks: Vec<Box<dyn ProgressCallback>>,
}

impl MultiCallback {
    pub fn new(callbacks: Vec<Box<dyn ProgressCallback>>) -> Self {
        Self { callbacks }
    }

    pub fn push(mut self, callback: impl ProgressCallback + 'static) -> Self {
        self.callbacks.push(Box::new(callback));
        self
    }
}

#[async_trait]
impl ProgressCallback for MultiCallback {
    async fn on_start(&self, scraper_type: &str, url: &str) {
        for cb in &self.callbacks {
            cb.on_start(scraper_type, url).await;
        }
    }

    async fn on_progress(&self, message: &str, percent: u8) {
        for cb in &self.callbacks {
            cb.on_progress(message, percent).await;
        }
    }

    async fn on_complete(&self, scraper_type: &str, result: &Value) {
        for cb in &self.callbacks {
            cb.on_complete(scraper_type, result).await;
        }
    }

    async fn on_error(&self, error: &Error) {
        for cb in &self.callbacks {
            cb.on_error(error).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_matches_python() {
        assert_eq!(
            progress_line("x", 50),
            format!("[{}{}] 50% - x", "█".repeat(15), "░".repeat(15))
        );
        assert_eq!(
            progress_line("done", 100),
            format!("[{}] 100% - done", "█".repeat(30))
        );
    }

    #[tokio::test]
    async fn json_log_writes_lines() {
        let path = std::env::temp_dir().join(format!("lsrs-log-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cb = JsonLogCallback::new(&path);
        cb.on_start("person", "https://x").await;
        cb.on_error(&Error::Authentication("nope".into())).await;
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event_type"], "start");
        assert_eq!(lines[0]["scraper_type"], "person");
        assert_eq!(lines[1]["error_type"], "AuthenticationError");
        assert_eq!(cb.logs().len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
