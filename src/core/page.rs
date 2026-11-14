//! Playwright-like page and locator API on top of `chromiumoxide`.
//!
//! The Python code base relies heavily on Playwright locators (`:has-text()`,
//! `xpath=ancestor::…`, `> *`, `.first`, `.all()`, auto-waiting…). Rust has no
//! maintained Playwright binding, so this module re-implements the subset that
//! is needed: a [`Locator`] is a lazy chain of selectors that is resolved inside
//! the page by a small JS engine (`locator.js`) every time an action runs —
//! exactly like Playwright, so locators never go stale.

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::layout::Point;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

const ENGINE: &str = include_str!("locator.js");
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Playwright's default timeout for actions.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Load condition for navigation (Playwright's `wait_until`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitUntil {
    #[default]
    DomContentLoaded,
    Load,
    /// Approximated as `load` followed by a 500 ms quiet period.
    NetworkIdle,
}

/// Element state for [`Locator::wait_for`] (Playwright's `state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElementState {
    Attached,
    Detached,
    #[default]
    Visible,
    Hidden,
}

impl ElementState {
    fn as_str(self) -> &'static str {
        match self {
            ElementState::Attached => "attached",
            ElementState::Detached => "detached",
            ElementState::Visible => "visible",
            ElementState::Hidden => "hidden",
        }
    }
}

/// A browser tab. Cheap to clone (it is a handle).
#[derive(Debug, Clone)]
pub struct Page {
    inner: chromiumoxide::Page,
    slow_mo: Duration,
    default_timeout: Duration,
}

impl Page {
    pub fn new(inner: chromiumoxide::Page) -> Self {
        Self {
            inner,
            slow_mo: Duration::ZERO,
            default_timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Delay applied before every action (Playwright's `slow_mo`).
    pub fn with_slow_mo(mut self, slow_mo: Duration) -> Self {
        self.slow_mo = slow_mo;
        self
    }

    pub fn set_default_timeout(&mut self, timeout: Duration) {
        self.default_timeout = timeout;
    }

    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// The underlying `chromiumoxide` page, for anything not covered here.
    pub fn inner(&self) -> &chromiumoxide::Page {
        &self.inner
    }

    async fn slow(&self) {
        if !self.slow_mo.is_zero() {
            tokio::time::sleep(self.slow_mo).await;
        }
    }

    /// Navigates to `url`.
    pub async fn goto(&self, url: &str, wait_until: WaitUntil, timeout: Duration) -> Result<()> {
        self.slow().await;
        tokio::time::timeout(timeout, self.inner.goto(url))
            .await
            .map_err(|_| {
                Error::Timeout(format!(
                    "page.goto: Timeout {}ms exceeded navigating to \"{url}\"",
                    timeout.as_millis()
                ))
            })??;
        if wait_until == WaitUntil::NetworkIdle {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(())
    }

    /// Current URL (follows client-side navigations).
    pub async fn url(&self) -> String {
        if let Ok(v) = self.evaluate::<String>("location.href").await {
            return v;
        }
        self.inner.url().await.ok().flatten().unwrap_or_default()
    }

    /// Evaluates a JS expression or function source and deserializes the result.
    /// `undefined` deserializes as `null` (so `()` and `Option<T>` work).
    ///
    /// Like Playwright, a function source (`() => …`, `function …`,
    /// `async …`) is *called*; anything else is evaluated as an expression.
    pub async fn evaluate<T: DeserializeOwned>(&self, script: &str) -> Result<T> {
        let res = if is_function_source(script) {
            self.inner.evaluate_function(script).await?
        } else {
            self.inner.evaluate(script).await?
        };
        let value = res.value().cloned().unwrap_or(Value::Null);
        Ok(serde_json::from_value(value)?)
    }

    /// Evaluates a script for its side effects and ignores the result
    /// (recent Chrome versions make e.g. `window.scrollTo()` return a value).
    pub async fn run_js(&self, script: &str) -> Result<()> {
        self.evaluate::<Value>(script).await.map(drop)
    }

    /// Creates a locator rooted at the document.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator {
            page: self.clone(),
            steps: vec![Step::Sel(selector.to_string())],
            timeout: None,
        }
    }

    /// Waits for the first element matching `selector` to reach `state`.
    pub async fn wait_for_selector(
        &self,
        selector: &str,
        timeout: Duration,
        state: ElementState,
    ) -> Result<()> {
        self.locator(selector)
            .first()
            .timeout(timeout)
            .wait_for(state)
            .await
    }

    /// Polls until `predicate(current_url)` returns `true`.
    pub async fn wait_for_url(
        &self,
        predicate: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        loop {
            if predicate(&self.url().await) {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(Error::Timeout(format!(
                    "page.wait_for_url: Timeout {}ms exceeded",
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Waits for `document.readyState` to reach the given load state.
    pub async fn wait_for_load_state(&self, state: WaitUntil, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            let ready: String = self
                .evaluate("document.readyState")
                .await
                .unwrap_or_default();
            let ok = match state {
                WaitUntil::DomContentLoaded => ready == "interactive" || ready == "complete",
                WaitUntil::Load | WaitUntil::NetworkIdle => ready == "complete",
            };
            if ok {
                if state == WaitUntil::NetworkIdle {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(Error::Timeout(format!(
                    "page.wait_for_load_state: Timeout {}ms exceeded",
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Plain sleep (Playwright's `wait_for_timeout`).
    pub async fn wait_for_timeout(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    /// Fills an input: focus, clear, then insert text like a paste.
    pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        self.locator(selector).first().fill(value).await
    }

    /// Clicks the first element matching `selector`.
    pub async fn click(&self, selector: &str) -> Result<()> {
        self.locator(selector).first().click().await
    }

    /// Presses a named key (`End`, `Enter`, `Escape`, …) on the keyboard.
    pub async fn press_key(&self, key: &str) -> Result<()> {
        self.slow().await;
        let (code, vk) = key_code(key);
        for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
            let mut b = DispatchKeyEventParams::builder()
                .r#type(kind.clone())
                .key(key)
                .code(code)
                .windows_virtual_key_code(vk)
                .native_virtual_key_code(vk);
            if key == "Enter" && kind == DispatchKeyEventType::KeyDown {
                b = b.text("\r");
            }
            let params = b.build().map_err(Error::Runtime)?;
            self.inner.execute(params).await?;
        }
        Ok(())
    }

    pub async fn bring_to_front(&self) -> Result<()> {
        self.inner.bring_to_front().await?;
        Ok(())
    }

    /// Runs `op` of the JS engine on the given locator chain.
    async fn run(
        &self,
        steps: &[Step],
        op: &str,
        arg: Value,
        user_fn: Option<&str>,
    ) -> Result<Value> {
        let declaration = match user_fn {
            None => ENGINE.to_string(),
            Some(f) => format!(
                "function(steps, op, _arg) {{ const __engine = {ENGINE}; return __engine(steps, op, ({f})); }}"
            ),
        };
        let params = CallFunctionOnParams::builder()
            .function_declaration(declaration)
            .argument(
                CallArgument::builder()
                    .value(serde_json::to_value(steps)?)
                    .build(),
            )
            .argument(
                CallArgument::builder()
                    .value(Value::String(op.into()))
                    .build(),
            )
            .argument(CallArgument::builder().value(arg).build())
            .build()
            .map_err(Error::Runtime)?;
        let res = self.inner.evaluate_function(params).await?;
        Ok(res.value().cloned().unwrap_or(Value::Null))
    }
}

/// Playwright's heuristic: does `script` look like a function declaration?
fn is_function_source(script: &str) -> bool {
    let s = script.trim_start();
    if s.starts_with("function") || s.starts_with("async ") || s.starts_with("async(") {
        return true;
    }
    // Arrow functions: `x => …` or `(a, b) => …`
    let head = match s.find("=>") {
        Some(i) => s[..i].trim(),
        None => return false,
    };
    let params = head
        .strip_prefix('(')
        .and_then(|h| h.strip_suffix(')'))
        .unwrap_or(head);
    params
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == ',' || c.is_whitespace())
}

/// Maps a key name to its DOM `code` and Windows virtual key code.
fn key_code(key: &str) -> (&'static str, i64) {
    match key {
        "End" => ("End", 35),
        "Home" => ("Home", 36),
        "PageDown" => ("PageDown", 34),
        "PageUp" => ("PageUp", 33),
        "Enter" => ("Enter", 13),
        "Escape" => ("Escape", 27),
        "Tab" => ("Tab", 9),
        "ArrowDown" => ("ArrowDown", 40),
        "ArrowUp" => ("ArrowUp", 38),
        _ => ("", 0),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
enum Step {
    Sel(String),
    Nth(i64),
}

#[derive(Debug, Deserialize)]
struct Found<T> {
    found: bool,
    v: Option<T>,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
}

/// Lazy element handle, re-resolved on every action (Playwright semantics).
///
/// Unlike Playwright, there is no "strict mode": actions on a locator matching
/// several elements use the first one.
#[derive(Debug, Clone)]
pub struct Locator {
    page: Page,
    steps: Vec<Step>,
    timeout: Option<Duration>,
}

impl Locator {
    /// Sub-locator scoped to the elements matched so far.
    pub fn locator(&self, selector: &str) -> Locator {
        let mut steps = self.steps.clone();
        steps.push(Step::Sel(selector.to_string()));
        Locator {
            page: self.page.clone(),
            steps,
            timeout: self.timeout,
        }
    }

    fn with_step(&self, step: Step) -> Locator {
        let mut steps = self.steps.clone();
        steps.push(step);
        Locator {
            page: self.page.clone(),
            steps,
            timeout: self.timeout,
        }
    }

    pub fn first(&self) -> Locator {
        self.with_step(Step::Nth(0))
    }

    pub fn last(&self) -> Locator {
        self.with_step(Step::Nth(-1))
    }

    pub fn nth(&self, index: i64) -> Locator {
        self.with_step(Step::Nth(index))
    }

    /// Overrides the timeout used by waiting actions on this locator.
    pub fn timeout(&self, timeout: Duration) -> Locator {
        Locator {
            page: self.page.clone(),
            steps: self.steps.clone(),
            timeout: Some(timeout),
        }
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    fn effective_timeout(&self) -> Duration {
        self.timeout.unwrap_or(self.page.default_timeout)
    }

    fn describe(&self) -> String {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Sel(s) => format!("locator('{s}')"),
                Step::Nth(0) => "first".to_string(),
                Step::Nth(-1) => "last".to_string(),
                Step::Nth(i) => format!("nth({i})"),
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    async fn run(&self, op: &str, arg: Value) -> Result<Value> {
        self.page.run(&self.steps, op, arg, None).await
    }

    /// Polls `op` until the element is found (and visible when required).
    async fn wait_found<T: DeserializeOwned>(
        &self,
        op: &str,
        arg: Value,
        require_visible: bool,
        action: &str,
    ) -> Result<Found<T>> {
        let timeout = self.effective_timeout();
        let start = Instant::now();
        loop {
            let raw = self.run(op, arg.clone()).await?;
            let found: Found<T> = serde_json::from_value(raw)?;
            if found.found && (!require_visible || found.visible.unwrap_or(true)) {
                return Ok(found);
            }
            if start.elapsed() >= timeout {
                return Err(Error::Timeout(format!(
                    "{action}: Timeout {}ms exceeded waiting for {}",
                    timeout.as_millis(),
                    self.describe()
                )));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Number of matching elements (no waiting).
    pub async fn count(&self) -> Result<usize> {
        Ok(serde_json::from_value(
            self.run("count", Value::Null).await?,
        )?)
    }

    /// One locator per currently matching element.
    pub async fn all(&self) -> Result<Vec<Locator>> {
        let n = self.count().await?;
        Ok((0..n as i64).map(|i| self.nth(i)).collect())
    }

    /// `textContent` of the first match (waits until attached).
    pub async fn text_content(&self) -> Result<Option<String>> {
        let f: Found<String> = self
            .wait_found("textContent", Value::Null, false, "locator.text_content")
            .await?;
        Ok(f.v)
    }

    /// `innerText` of the first match (waits until attached).
    pub async fn inner_text(&self) -> Result<String> {
        let f: Found<String> = self
            .wait_found("innerText", Value::Null, false, "locator.inner_text")
            .await?;
        Ok(f.v.unwrap_or_default())
    }

    /// Attribute of the first match (waits until attached).
    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let f: Found<String> = self
            .wait_found(
                "getAttribute",
                Value::String(name.into()),
                false,
                "locator.get_attribute",
            )
            .await?;
        Ok(f.v)
    }

    /// `innerText` of every match, in document order (no waiting).
    pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
        Ok(serde_json::from_value(
            self.run("allInnerTexts", Value::Null).await?,
        )?)
    }

    /// `textContent` of every match, in document order (no waiting).
    pub async fn all_text_contents(&self) -> Result<Vec<String>> {
        Ok(serde_json::from_value(
            self.run("allTextContents", Value::Null).await?,
        )?)
    }

    /// Runs `function` (JS source taking the array of matched elements) in the page.
    pub async fn evaluate_all<T: DeserializeOwned>(&self, function: &str) -> Result<T> {
        let v = self
            .page
            .run(&self.steps, "evaluateAll", Value::Null, Some(function))
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Whether the first match is visible right now (no waiting).
    pub async fn is_visible(&self) -> Result<bool> {
        Ok(serde_json::from_value(
            self.run("isVisible", Value::Null).await?,
        )?)
    }

    /// Waits until the first match reaches `state`.
    pub async fn wait_for(&self, state: ElementState) -> Result<()> {
        let timeout = self.effective_timeout();
        let start = Instant::now();
        loop {
            let ok: bool = serde_json::from_value(
                self.run("state", Value::String(state.as_str().into()))
                    .await?,
            )?;
            if ok {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(Error::Timeout(format!(
                    "locator.wait_for: Timeout {}ms exceeded waiting for {} to be {}",
                    timeout.as_millis(),
                    self.describe(),
                    state.as_str()
                )));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
        self.wait_found::<Value>(
            "textContent",
            Value::Null,
            false,
            "locator.scroll_into_view",
        )
        .await?;
        self.run("scrollIntoView", Value::Null).await?;
        Ok(())
    }

    /// Scrolls the element into view and clicks its center with the mouse.
    pub async fn click(&self) -> Result<()> {
        let f: Found<Value> = self
            .wait_found("clickPoint", Value::Null, true, "locator.click")
            .await?;
        self.page.slow().await;
        self.page.inner.click(Point { x: f.x, y: f.y }).await?;
        Ok(())
    }

    /// Focuses and clears the element, then inserts `value`.
    pub async fn fill(&self, value: &str) -> Result<()> {
        self.wait_found::<Value>("focusClear", Value::Null, true, "locator.fill")
            .await?;
        self.page.slow().await;
        if !value.is_empty() {
            self.page
                .inner
                .execute(InsertTextParams::new(value))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_function_source;

    #[test]
    fn function_detection() {
        assert!(is_function_source("() => 1"));
        assert!(is_function_source("\n  () => {\n return 1; }"));
        assert!(is_function_source("(a, b) => a + b"));
        assert!(is_function_source("els => els.length"));
        assert!(is_function_source("async () => 1"));
        assert!(is_function_source("function () { return 1 }"));
        assert!(!is_function_source("document.body.scrollHeight"));
        assert!(!is_function_source("window.scrollTo(0, 400)"));
        assert!(!is_function_source("[1,2].map(x => x * 2)"));
        assert!(!is_function_source("location.href"));
    }
}
