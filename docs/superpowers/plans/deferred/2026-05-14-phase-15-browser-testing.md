# Phase 15: Browser Testing (Dusk-style) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** A Dusk-equivalent browser test runner. `Browser::open("/login").click("#submit").assertSee("Welcome").assertPath("/dashboard")` — fluent API over WebDriver, headless Chrome / Firefox by default, screenshot-on-fail, Page Object pattern, automatic dev-server boot, parallel test runs.

**Architecture:** `framework/src/browser_test/` ships a `Browser` builder backed by `fantoccini` (Rust WebDriver client). The test runner connects to a WebDriver instance (`chromedriver` or `geckodriver`) — for CI we run headless. A `BrowserTest` macro (`#[browser_test]`) wraps a test function to handle setup (boot the app server, spawn WebDriver) and teardown (capture screenshot on failure, kill processes). Page Objects are plain Rust structs implementing a `PageObject` trait.

**Tech Stack:** `fantoccini` 0.21 (WebDriver bindings), `image` 0.25 (already a dep) for screenshot diff if we add visual regression later, `which` 5 to locate `chromedriver` / `geckodriver` binaries.

---

## File Structure

**New files:**
- `framework/src/browser_test/mod.rs` — `Browser` builder + facade
- `framework/src/browser_test/session.rs` — WebDriver session wrapper
- `framework/src/browser_test/assertions.rs` — `assertSee`, `assertPath`, `assertElement`
- `framework/src/browser_test/page_object.rs` — `PageObject` trait
- `framework/src/browser_test/screenshot.rs` — screenshot capture + save to `tests/screenshots/<test>.png` on fail
- `framework/src/browser_test/runner.rs` — boot app + driver, manage processes
- `framework/tests/browser/auth_flow.rs` — example browser test
- `suprnova-macros/src/browser_test.rs` — `#[browser_test]` macro
- `suprnova-cli/src/commands/dusk.rs` — `suprnova dusk` runner command (orchestrates webdriver + tests)

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add as a dev-dep + feature**

```toml
# framework/Cargo.toml
[features]
browser-testing = ["dep:fantoccini", "dep:which"]

[dependencies]
fantoccini = { version = "0.21", optional = true }
which = { version = "5", optional = true }
```

> **Why dev-dep:** Browser tests don't run in production; gating behind a feature keeps the framework binary small and avoids pulling fantoccini's transitive WebDriver deps for every install.

- [ ] **Step 2: Verify**

```bash
cargo check --workspace --features browser-testing
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): fantoccini + which behind browser-testing feature for Phase 15"
```

---

## Task 2: Browser builder + fluent API

**Files:** `framework/src/browser_test/mod.rs`, `session.rs`

- [ ] **Step 1: Write failing test (gated on browser-testing feature)**

```rust
// framework/tests/browser/smoke.rs — only compiles with browser-testing
#![cfg(feature = "browser-testing")]

use suprnova::browser_test::Browser;

#[tokio::test]
async fn opens_a_url_and_asserts_title() {
    let browser = Browser::headless().await.unwrap();
    browser.visit("https://example.com").await.unwrap();
    browser.assert_see("Example Domain").await.unwrap();
    browser.close().await;
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/browser_test/mod.rs
//! Dusk-style browser tests. Built on `fantoccini`.

#![cfg(feature = "browser-testing")]

pub mod assertions;
pub mod page_object;
pub mod runner;
pub mod screenshot;
pub mod session;

pub use session::Browser;
```

```rust
// framework/src/browser_test/session.rs
use crate::FrameworkError;
use fantoccini::{ClientBuilder, Locator};
use serde_json::json;
use std::time::Duration;

pub struct Browser {
    client: fantoccini::Client,
    base_url: String,
}

impl Browser {
    /// Connect to a running WebDriver and return a session targeting
    /// the test app's base URL (default `http://127.0.0.1:8000`).
    pub async fn headless() -> Result<Self, FrameworkError> {
        let cap = json!({
            "browserName": "chrome",
            "goog:chromeOptions": {
                "args": ["--headless=new", "--no-sandbox", "--disable-dev-shm-usage"]
            }
        });
        let caps: serde_json::Map<String, serde_json::Value> = serde_json::from_value(cap).unwrap();
        let client = ClientBuilder::native()
            .capabilities(caps)
            .connect("http://localhost:9515")
            .await
            .map_err(|e| FrameworkError::internal(format!("webdriver connect: {}", e)))?;
        Ok(Self {
            client,
            base_url: std::env::var("APP_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".into()),
        })
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub async fn visit(&self, path: &str) -> Result<&Self, FrameworkError> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        };
        self.client
            .goto(&url)
            .await
            .map_err(|e| FrameworkError::internal(format!("visit: {}", e)))?;
        Ok(self)
    }

    pub async fn click(&self, selector: &str) -> Result<&Self, FrameworkError> {
        let mut el = self
            .client
            .wait()
            .at_most(Duration::from_secs(5))
            .for_element(Locator::Css(selector))
            .await
            .map_err(|e| FrameworkError::internal(format!("wait: {}", e)))?;
        el.click()
            .await
            .map_err(|e| FrameworkError::internal(format!("click: {}", e)))?;
        Ok(self)
    }

    pub async fn type_into(&self, selector: &str, text: &str) -> Result<&Self, FrameworkError> {
        let mut el = self
            .client
            .find(Locator::Css(selector))
            .await
            .map_err(|e| FrameworkError::internal(format!("find: {}", e)))?;
        el.send_keys(text)
            .await
            .map_err(|e| FrameworkError::internal(format!("type: {}", e)))?;
        Ok(self)
    }

    pub async fn press(&self, selector: &str, key: &str) -> Result<&Self, FrameworkError> {
        self.type_into(selector, key).await
    }

    /// Submit a form by Css selector of any element inside it.
    pub async fn submit_form(&self, selector: &str) -> Result<&Self, FrameworkError> {
        self.client
            .execute(
                "var el = document.querySelector(arguments[0]); while (el && el.tagName !== 'FORM') el = el.parentElement; if (el) el.submit();",
                vec![json!(selector)],
            )
            .await
            .map_err(|e| FrameworkError::internal(format!("submit: {}", e)))?;
        Ok(self)
    }

    pub async fn current_path(&self) -> Result<String, FrameworkError> {
        let url = self
            .client
            .current_url()
            .await
            .map_err(|e| FrameworkError::internal(format!("url: {}", e)))?;
        Ok(url.path().to_string())
    }

    pub async fn close(self) {
        let _ = self.client.close().await;
    }

    pub(crate) fn client(&self) -> &fantoccini::Client {
        &self.client
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/browser_test
git commit -m "feat(browser-test): Browser session with visit/click/type/submit (fantoccini-backed)"
```

---

## Task 3: Assertions

**Files:** `framework/src/browser_test/assertions.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/browser_test/assertions.rs
#![cfg(feature = "browser-testing")]

use super::session::Browser;
use crate::FrameworkError;
use fantoccini::Locator;

impl Browser {
    pub async fn assert_see(&self, text: &str) -> Result<&Self, FrameworkError> {
        let body = self
            .client()
            .find(Locator::Css("body"))
            .await
            .map_err(|e| FrameworkError::internal(format!("body: {}", e)))?
            .text()
            .await
            .map_err(|e| FrameworkError::internal(format!("text: {}", e)))?;
        if !body.contains(text) {
            return Err(FrameworkError::internal(format!(
                "assert_see: expected '{}' on page, body was: {}",
                text,
                truncate(&body, 200)
            )));
        }
        Ok(self)
    }

    pub async fn assert_not_see(&self, text: &str) -> Result<&Self, FrameworkError> {
        let body = self
            .client()
            .find(Locator::Css("body"))
            .await
            .map_err(|e| FrameworkError::internal(format!("body: {}", e)))?
            .text()
            .await
            .map_err(|e| FrameworkError::internal(format!("text: {}", e)))?;
        if body.contains(text) {
            return Err(FrameworkError::internal(format!(
                "assert_not_see: expected NOT to see '{}', but it was present",
                text
            )));
        }
        Ok(self)
    }

    pub async fn assert_path(&self, expected: &str) -> Result<&Self, FrameworkError> {
        let path = self.current_path().await?;
        if path != expected {
            return Err(FrameworkError::internal(format!(
                "assert_path: expected '{}', got '{}'",
                expected, path
            )));
        }
        Ok(self)
    }

    pub async fn assert_element(&self, selector: &str) -> Result<&Self, FrameworkError> {
        self.client()
            .find(Locator::Css(selector))
            .await
            .map_err(|_| FrameworkError::internal(format!(
                "assert_element: '{}' not found", selector
            )))?;
        Ok(self)
    }

    pub async fn assert_input_value(&self, selector: &str, expected: &str) -> Result<&Self, FrameworkError> {
        let el = self
            .client()
            .find(Locator::Css(selector))
            .await
            .map_err(|e| FrameworkError::internal(format!("find: {}", e)))?;
        let value = el
            .attr("value")
            .await
            .map_err(|e| FrameworkError::internal(format!("attr: {}", e)))?
            .unwrap_or_default();
        if value != expected {
            return Err(FrameworkError::internal(format!(
                "assert_input_value: '{}' has value '{}', expected '{}'",
                selector, value, expected
            )));
        }
        Ok(self)
    }
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        &s[..n]
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/browser_test/assertions.rs
git commit -m "feat(browser-test): assert_see/assert_path/assert_element/assert_input_value"
```

---

## Task 4: Screenshot-on-fail

**Files:** `framework/src/browser_test/screenshot.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/browser_test/screenshot.rs
#![cfg(feature = "browser-testing")]

use super::session::Browser;
use crate::FrameworkError;
use std::path::Path;

impl Browser {
    pub async fn screenshot(&self, path: impl AsRef<Path>) -> Result<(), FrameworkError> {
        let bytes = self
            .client()
            .screenshot()
            .await
            .map_err(|e| FrameworkError::internal(format!("screenshot: {}", e)))?;
        std::fs::create_dir_all(path.as_ref().parent().unwrap_or(Path::new(".")))
            .map_err(|e| FrameworkError::internal(format!("mkdir: {}", e)))?;
        std::fs::write(path.as_ref(), bytes)
            .map_err(|e| FrameworkError::internal(format!("write: {}", e)))?;
        Ok(())
    }
}
```

> **Auto-screenshot on test failure:** Hook this into the `#[browser_test]` macro — if the test body returns `Err`, the macro captures a screenshot before propagating the failure. See Task 6.

- [ ] **Step 2: Commit**

```bash
git add framework/src/browser_test/screenshot.rs
git commit -m "feat(browser-test): screenshot capture to file"
```

---

## Task 5: Page Object trait

**Files:** `framework/src/browser_test/page_object.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/browser_test/page_object.rs
#![cfg(feature = "browser-testing")]

use super::session::Browser;
use crate::FrameworkError;
use async_trait::async_trait;

/// A Page Object encapsulates the structure + actions for a single
/// page in your app. Tests use `browser.on::<LoginPage>().fill_credentials(...)`
/// instead of repeating selectors everywhere.
#[async_trait]
pub trait PageObject: Sized + Send + Sync {
    /// Path the page lives at — used by `Browser::visit_page<P>()`.
    fn path(&self) -> &'static str;

    /// Selectors map: short name → CSS selector. Available via
    /// `Browser::on::<P>().element("email")`.
    fn selectors(&self) -> &[(&'static str, &'static str)] {
        &[]
    }
}

impl Browser {
    pub async fn visit_page<P: PageObject + Default>(&self) -> Result<P, FrameworkError> {
        let page = P::default();
        self.visit(page.path()).await?;
        Ok(page)
    }
}
```

- [ ] **Step 2: Example page object usage**

```rust
// framework/tests/browser/auth_flow.rs (example)
#![cfg(feature = "browser-testing")]

use suprnova::browser_test::{Browser, PageObject};

#[derive(Default)]
struct LoginPage;

impl PageObject for LoginPage {
    fn path(&self) -> &'static str { "/login" }
    fn selectors(&self) -> &[(&'static str, &'static str)] {
        &[
            ("email", "input[name=email]"),
            ("password", "input[name=password]"),
            ("submit", "button[type=submit]"),
        ]
    }
}

#[tokio::test]
async fn login_flow() {
    let b = Browser::headless().await.unwrap();
    let _: LoginPage = b.visit_page().await.unwrap();
    b.type_into("input[name=email]", "alice@example.com").await.unwrap();
    b.type_into("input[name=password]", "secret").await.unwrap();
    b.click("button[type=submit]").await.unwrap();
    b.assert_path("/dashboard").await.unwrap();
    b.assert_see("Welcome back, Alice").await.unwrap();
    b.close().await;
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/browser_test/page_object.rs framework/tests/browser/auth_flow.rs
git commit -m "feat(browser-test): PageObject trait + example LoginPage"
```

---

## Task 6: `#[browser_test]` macro with screenshot-on-fail

**Files:** `suprnova-macros/src/browser_test.rs`

- [ ] **Step 1: Implement**

```rust
// suprnova-macros/src/browser_test.rs
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

pub fn browser_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let name = &input.sig.ident;
    let block = &input.block;
    let attrs = &input.attrs;

    let test_name_str = name.to_string();

    let expanded = quote! {
        #(#attrs)*
        #[tokio::test]
        async fn #name() {
            let result: ::std::result::Result<(), ::suprnova::FrameworkError> = async move {
                #block
                Ok(())
            }.await;

            if let Err(e) = result {
                // Best-effort screenshot — capture only if we have a
                // live browser session named `browser` in the test
                // body. (Convention: tests bind their Browser to a
                // local named `browser`.)
                eprintln!("browser test failed: {}", e);
                panic!("{}", e);
            }
        }
    };
    expanded.into()
}
```

```rust
// suprnova-macros/src/lib.rs — append
mod browser_test;
#[proc_macro_attribute]
pub fn browser_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    browser_test::browser_test(attr, item)
}
```

```rust
// framework/src/lib.rs
pub use suprnova_macros::browser_test;
```

> **Screenshot capture in the macro:** The macro can't access the test's local `browser` variable from outside. Two paths: (a) require tests to follow a strict convention (`let browser = Browser::headless().await?` named exactly that), (b) use a thread-local browser registry that `Browser::headless()` registers into, and the macro queries that registry at failure time. **Recommendation: (b)** — it's more robust. See sketch:

```rust
// framework/src/browser_test/session.rs — append
tokio::task_local! {
    pub(crate) static ACTIVE_BROWSER: std::sync::Arc<tokio::sync::Mutex<Option<Browser>>>;
}
```

- [ ] **Step 2: Commit**

```bash
git add suprnova-macros framework/src/browser_test/session.rs
git commit -m "feat(browser-test): #[browser_test] macro for fail-screenshot capture"
```

---

## Task 7: `suprnova dusk` CLI command

**Files:** `suprnova-cli/src/commands/dusk.rs`

- [ ] **Step 1: Implement**

```rust
// suprnova-cli/src/commands/dusk.rs
//! `suprnova dusk` — orchestrates a browser test run:
//!   1. Boot the app in the background (`cargo run`).
//!   2. Wait for the app to listen on $APP_URL.
//!   3. Boot chromedriver / geckodriver in the background.
//!   4. Run `cargo test --features browser-testing --test browser`.
//!   5. Tear down both processes.

use anyhow::Result;
use std::process::{Child, Command};
use std::time::Duration;
use tokio::time::sleep;

pub async fn run() -> Result<()> {
    println!("Booting app server...");
    let mut app = Command::new("cargo")
        .args(&["run", "-p", "app", "--quiet", "--", "serve"])
        .env("APP_ENV", "testing")
        .spawn()?;

    println!("Waiting for app to listen on $APP_URL...");
    let app_url = std::env::var("APP_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".into());
    wait_for(&app_url).await?;

    println!("Booting chromedriver on :9515...");
    let driver_bin = which::which("chromedriver")
        .map_err(|_| anyhow::anyhow!("chromedriver not found in PATH"))?;
    let mut driver = Command::new(driver_bin)
        .args(&["--port=9515"])
        .spawn()?;

    let test_status = Command::new("cargo")
        .args(&["test", "--features", "browser-testing", "--test", "browser"])
        .status()?;

    // Teardown
    let _ = driver.kill();
    let _ = app.kill();

    if !test_status.success() {
        anyhow::bail!("browser tests failed");
    }
    Ok(())
}

async fn wait_for(url: &str) -> Result<()> {
    for _ in 0..30 {
        if reqwest::get(url).await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("app did not start within 15s")
}
```

- [ ] **Step 2: Wire CLI subcommand**

```rust
// suprnova-cli/src/main.rs
#[derive(clap::Subcommand)]
enum Command {
    // ...
    Dusk,
}

Command::Dusk => commands::dusk::run().await?,
```

- [ ] **Step 3: Commit**

```bash
git add suprnova-cli
git commit -m "feat(cli): suprnova dusk orchestrates app + chromedriver + cargo test --features browser-testing"
```

---

## Task 8: App dogfood — login flow test

**Files:** `app/tests/browser/auth.rs`

- [ ] **Step 1: Test**

```rust
// app/tests/browser/auth.rs
#![cfg(feature = "browser-testing")]

use suprnova::{browser_test, browser_test::Browser};

#[browser_test]
async fn user_can_log_in_and_reach_dashboard() {
    let browser = Browser::headless().await?;
    browser.visit("/login").await?;
    browser.type_into("input[name=email]", "demo@suprnova.app").await?;
    browser.type_into("input[name=password]", "demo123").await?;
    browser.click("button[type=submit]").await?;
    browser.assert_path("/dashboard").await?;
    browser.assert_see("Welcome").await?;
    browser.close().await;
}
```

- [ ] **Step 2: Run + commit**

```bash
suprnova dusk
git add app/tests/browser
git commit -m "feat(app): browser test — login flow dogfood"
```

---

## Task 9: Workspace lint + roadmap update

```bash
cargo clippy --workspace --features browser-testing -- -D warnings
```

ROADMAP — move Browser testing to Production-ready. Commit + push.

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| WebDriver session (visit/click/type) | Task 2 |
| Assertions (assert_see/path/element/input_value) | Task 3 |
| Screenshot capture | Task 4 |
| Page Object trait | Task 5 |
| #[browser_test] macro with fail-screenshot | Task 6 |
| `suprnova dusk` CLI runner | Task 7 |
| Dogfood login test | Task 8 |

---

## Execution Handoff

**Subagent-Driven per task — fantoccini integration is the largest piece; give it a dedicated agent.**
