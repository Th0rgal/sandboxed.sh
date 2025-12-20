//! Browser automation tools using Chrome DevTools Protocol (CDP).
//!
//! These tools connect to a Chrome/Chromium browser running with remote debugging enabled.
//! Start Chrome with: `google-chrome --remote-debugging-port=9222`
//!
//! Environment variables:
//! - `BROWSER_CDP_URL`: CDP WebSocket URL (default: `http://127.0.0.1:9222`)
//! - `BROWSER_ENABLED`: Set to `true` to enable browser tools (default: false)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::Tool;

/// Default CDP endpoint
const DEFAULT_CDP_URL: &str = "http://127.0.0.1:9222";

/// Shared browser state (lazy initialization)
static BROWSER_STATE: std::sync::LazyLock<Arc<Mutex<Option<BrowserSession>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Browser session holding the browser and current page
struct BrowserSession {
    #[allow(dead_code)]
    browser: Browser,
    page: Page,
}

/// Get or create a browser session
async fn get_browser_session() -> anyhow::Result<Arc<Mutex<Option<BrowserSession>>>> {
    let state = BROWSER_STATE.clone();
    let mut guard = state.lock().await;

    if guard.is_none() {
        let cdp_url = std::env::var("BROWSER_CDP_URL").unwrap_or_else(|_| DEFAULT_CDP_URL.to_string());
        
        // Connect to existing Chrome with remote debugging
        let (browser, mut handler) = Browser::connect(&cdp_url).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to Chrome at {}. Make sure Chrome is running with --remote-debugging-port=9222. Error: {}",
                cdp_url,
                e
            )
        })?;

        // Spawn handler in background
        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    tracing::warn!("Browser event error: {}", e);
                }
            }
        });

        // Get or create a page
        let page = browser.new_page("about:blank").await?;

        *guard = Some(BrowserSession { browser, page });
    }

    drop(guard);
    Ok(state)
}

/// Get the current page, creating a new session if needed
async fn with_page<F, Fut, T>(f: F) -> anyhow::Result<T>
where
    F: FnOnce(Page) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let state = get_browser_session().await?;
    let guard = state.lock().await;
    
    if let Some(session) = guard.as_ref() {
        f(session.page.clone()).await
    } else {
        Err(anyhow::anyhow!("Browser session not initialized"))
    }
}

// ============================================================================
// Browser Navigate Tool
// ============================================================================

/// Navigate to a URL and wait for page load
pub struct BrowserNavigate;

#[async_trait]
impl Tool for BrowserNavigate {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Navigate the browser to a URL. Waits for the page to fully load. Use this to open websites for scraping, testing, or interaction."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to navigate to"
                },
                "wait_selector": {
                    "type": "string",
                    "description": "Optional CSS selector to wait for after navigation"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' argument"))?;
        let wait_selector = args["wait_selector"].as_str();

        with_page(|page| async move {
            // Navigate to URL
            page.goto(url).await?;
            
            // Wait for network idle or timeout
            tokio::time::sleep(Duration::from_millis(1000)).await;

            // Optionally wait for a specific element
            if let Some(selector) = wait_selector {
                page.wait_for_navigation().await.ok();
                page.find_element(selector)
                    .await
                    .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;
            }

            // Get page info
            let title = page.get_title().await?.unwrap_or_default();
            let current_url = page.url().await?.map(|u| u.to_string()).unwrap_or_default();

            Ok(format!(
                "Navigated to: {}\nTitle: {}\nCurrent URL: {}",
                url, title, current_url
            ))
        })
        .await
    }
}

// ============================================================================
// Browser Screenshot Tool
// ============================================================================

/// Take a screenshot of the current page
pub struct BrowserScreenshot;

/// Get Supabase configuration from environment (for auto-upload).
fn get_supabase_config() -> Option<(String, String)> {
    let url = std::env::var("SUPABASE_URL").ok()?;
    let key = std::env::var("SUPABASE_SERVICE_ROLE_KEY").ok()?;
    
    if url.is_empty() || key.is_empty() {
        return None;
    }
    
    Some((url, key))
}

/// Upload image bytes to Supabase Storage and return the public URL.
async fn upload_to_supabase(
    content: &[u8],
    supabase_url: &str,
    service_role_key: &str,
) -> anyhow::Result<String> {
    let file_id = uuid::Uuid::new_v4();
    let upload_path = format!("{}.png", file_id);
    
    let storage_url = format!(
        "{}/storage/v1/object/images/{}",
        supabase_url.trim_end_matches('/'),
        upload_path
    );
    
    let client = reqwest::Client::new();
    let resp = client
        .post(&storage_url)
        .header("apikey", service_role_key)
        .header("Authorization", format!("Bearer {}", service_role_key))
        .header("Content-Type", "image/png")
        .header("x-upsert", "true")
        .body(content.to_vec())
        .send()
        .await?;
    
    let status = resp.status();
    if !status.is_success() {
        let error_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to upload image: {} - {}", status, error_text);
    }
    
    // Return public URL
    Ok(format!(
        "{}/storage/v1/object/public/images/{}",
        supabase_url.trim_end_matches('/'),
        upload_path
    ))
}

#[async_trait]
impl Tool for BrowserScreenshot {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn description(&self) -> &str {
        "Take a screenshot of the current browser page. If cloud storage is configured, \
        automatically uploads and returns markdown you can include in your response. \
        Otherwise returns the local file path.\n\n\
        IMPORTANT: When a 'markdown' field is returned, you MUST include it in your response \
        for the user to see the image!"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Description of what the screenshot shows (used in alt text)"
                },
                "full_page": {
                    "type": "boolean",
                    "description": "Capture the full scrollable page (default: false, captures viewport only)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, workspace: &Path) -> anyhow::Result<String> {
        let description = args["description"].as_str().unwrap_or("screenshot");
        let full_page = args["full_page"].as_bool().unwrap_or(false);
        let filename = format!("screenshot_{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S"));

        // Take the screenshot
        let screenshot = with_page(|page| async move {
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(full_page)
                .build();
            Ok(page.screenshot(params).await?)
        })
        .await?;

        // Save locally first (for backup/debugging)
        let temp_dir = workspace.join("temp");
        std::fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join(&filename);
        std::fs::write(&file_path, &screenshot)?;

        // Try to upload to Supabase if configured
        if let Some((supabase_url, service_role_key)) = get_supabase_config() {
            match upload_to_supabase(&screenshot, &supabase_url, &service_role_key).await {
                Ok(public_url) => {
                    tracing::info!(
                        local_path = %file_path.display(),
                        public_url = %public_url,
                        size = screenshot.len(),
                        "Screenshot uploaded to Supabase"
                    );
                    
                    let markdown = format!("![{}]({})", description, public_url);
                    return Ok(json!({
                        "success": true,
                        "url": public_url,
                        "markdown": markdown,
                        "local_path": file_path.display().to_string(),
                        "size_bytes": screenshot.len(),
                        "message": "Screenshot uploaded! Include the 'markdown' value in your response for the user to see it."
                    }).to_string());
                }
                Err(e) => {
                    tracing::warn!("Failed to upload screenshot to Supabase: {}", e);
                    // Fall through to return local path
                }
            }
        }

        // No Supabase or upload failed - return local path
        Ok(format!(
            "Screenshot saved to: {}\nSize: {} bytes\n\nNote: Cloud storage not configured. Use upload_image tool to share this image.",
            file_path.display(),
            screenshot.len()
        ))
    }
}

// ============================================================================
// Browser Get Content Tool
// ============================================================================

/// Get the text content of the current page
pub struct BrowserGetContent;

#[async_trait]
impl Tool for BrowserGetContent {
    fn name(&self) -> &str {
        "browser_get_content"
    }

    fn description(&self) -> &str {
        "Extract the text content from the current page. Returns readable text with structure preserved. Use after navigating to read the page content."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector to get content from a specific element"
                },
                "include_html": {
                    "type": "boolean",
                    "description": "Return HTML instead of text (default: false)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"].as_str();
        let include_html = args["include_html"].as_bool().unwrap_or(false);

        with_page(|page| async move {
            let content: String = if let Some(sel) = selector {
                // Get content from specific element
                let element = page.find_element(sel).await
                    .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", sel, e))?;
                
                if include_html {
                    element.inner_html().await?.unwrap_or_default()
                } else {
                    element.inner_text().await?.unwrap_or_default()
                }
            } else {
                // Get full page content
                if include_html {
                    page.content().await?
                } else {
                    // Execute JS to get text content
                    let result = page.evaluate("document.body.innerText").await?;
                    result.into_value::<String>().unwrap_or_default()
                }
            };

            // Truncate if too long
            let max_len = 50000;
            if content.len() > max_len {
                Ok(format!(
                    "{}\n\n... [truncated, {} total characters]",
                    &content[..max_len],
                    content.len()
                ))
            } else {
                Ok(content)
            }
        })
        .await
    }
}

// ============================================================================
// Browser Click Tool
// ============================================================================

/// Click on an element
pub struct BrowserClick;

#[async_trait]
impl Tool for BrowserClick {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Click on an element in the browser. Use CSS selectors like '#id', '.class', 'button', 'a[href*=login]', etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to click"
                }
            },
            "required": ["selector"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'selector' argument"))?;

        with_page(|page| async move {
            let element = page.find_element(selector).await
                .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;
            
            element.click().await?;
            
            // Wait a bit for any navigation or dynamic updates
            tokio::time::sleep(Duration::from_millis(500)).await;

            Ok(format!("Clicked element: {}", selector))
        })
        .await
    }
}

// ============================================================================
// Browser Type Tool
// ============================================================================

/// Type text into an input element
pub struct BrowserType;

#[async_trait]
impl Tool for BrowserType {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Type text into an input field. First clicks the element to focus it, then types the text."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the input element"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type"
                },
                "clear_first": {
                    "type": "boolean",
                    "description": "Clear the input before typing (default: true)"
                }
            },
            "required": ["selector", "text"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'selector' argument"))?;
        let text = args["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'text' argument"))?;
        let clear_first = args["clear_first"].as_bool().unwrap_or(true);

        with_page(|page| async move {
            let element = page.find_element(selector).await
                .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;
            
            // Click to focus
            element.click().await?;
            
            // Clear if requested
            if clear_first {
                // Select all and delete
                element.type_str("").await?; // Focus
                page.evaluate("document.activeElement.value = ''").await.ok();
            }
            
            // Type the text
            element.type_str(text).await?;

            Ok(format!("Typed '{}' into: {}", text, selector))
        })
        .await
    }
}

// ============================================================================
// Browser Evaluate Tool
// ============================================================================

/// Execute JavaScript in the browser
pub struct BrowserEvaluate;

#[async_trait]
impl Tool for BrowserEvaluate {
    fn name(&self) -> &str {
        "browser_evaluate"
    }

    fn description(&self) -> &str {
        "Execute JavaScript code in the browser and return the result. Useful for complex interactions, extracting data, or debugging."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "JavaScript code to execute. The result of the last expression is returned."
                }
            },
            "required": ["script"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let script = args["script"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'script' argument"))?;

        with_page(|page| async move {
            let result = page.evaluate(script).await?;
            
            // Try to serialize the result
            let value = result.value();
            match value {
                Some(v) => Ok(serde_json::to_string_pretty(&v)?),
                None => Ok("(no return value)".to_string()),
            }
        })
        .await
    }
}

// ============================================================================
// Browser Wait Tool
// ============================================================================

/// Wait for an element or condition
pub struct BrowserWait;

#[async_trait]
impl Tool for BrowserWait {
    fn name(&self) -> &str {
        "browser_wait"
    }

    fn description(&self) -> &str {
        "Wait for an element to appear or a condition to be met. Use after clicking or navigating when content loads dynamically."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector to wait for"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Maximum time to wait in milliseconds (default: 10000)"
                }
            },
            "required": ["selector"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'selector' argument"))?;
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(10000);

        with_page(|page| async move {
            let start = std::time::Instant::now();
            let timeout = Duration::from_millis(timeout_ms);
            
            loop {
                if page.find_element(selector).await.is_ok() {
                    return Ok(format!(
                        "Element '{}' found after {} ms",
                        selector,
                        start.elapsed().as_millis()
                    ));
                }
                
                if start.elapsed() > timeout {
                    return Err(anyhow::anyhow!(
                        "Timeout waiting for element '{}' after {} ms",
                        selector,
                        timeout_ms
                    ));
                }
                
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
    }
}

// ============================================================================
// Browser Close Tool
// ============================================================================

/// Close the browser session
pub struct BrowserClose;

#[async_trait]
impl Tool for BrowserClose {
    fn name(&self) -> &str {
        "browser_close"
    }

    fn description(&self) -> &str {
        "Close the current browser page/tab. Use when done with browser automation to free resources."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let state = BROWSER_STATE.clone();
        let mut guard = state.lock().await;
        
        if let Some(session) = guard.take() {
            // Close the page
            session.page.close().await.ok();
            // Browser will be dropped
            Ok("Browser session closed".to_string())
        } else {
            Ok("No active browser session".to_string())
        }
    }
}

// ============================================================================
// Browser List Elements Tool
// ============================================================================

/// List interactive elements on the page
pub struct BrowserListElements;

#[async_trait]
impl Tool for BrowserListElements {
    fn name(&self) -> &str {
        "browser_list_elements"
    }

    fn description(&self) -> &str {
        "List interactive elements on the current page (links, buttons, inputs, etc.). Useful for understanding page structure before interacting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector to filter elements (default: 'a, button, input, select, textarea, [onclick]')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of elements to return (default: 50)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"].as_str()
            .unwrap_or("a, button, input, select, textarea, [onclick], [role='button']");
        let limit = args["limit"].as_u64().unwrap_or(50) as usize;

        with_page(|page| async move {
            // Use JavaScript to get element info
            let script = format!(r#"
                (() => {{
                    const elements = document.querySelectorAll('{}');
                    const results = [];
                    for (let i = 0; i < Math.min(elements.length, {}); i++) {{
                        const el = elements[i];
                        const rect = el.getBoundingClientRect();
                        results.push({{
                            tag: el.tagName.toLowerCase(),
                            id: el.id || null,
                            class: el.className || null,
                            text: (el.innerText || el.value || '').slice(0, 100).trim(),
                            href: el.href || null,
                            type: el.type || null,
                            name: el.name || null,
                            visible: rect.width > 0 && rect.height > 0
                        }});
                    }}
                    return results;
                }})()
            "#, selector, limit);

            let result = page.evaluate(script.as_str()).await?;
            let elements: Vec<Value> = result.into_value().unwrap_or_default();

            if elements.is_empty() {
                return Ok(format!("No elements found matching: {}", selector));
            }

            let mut output = format!("Found {} elements:\n\n", elements.len());
            for (i, el) in elements.iter().enumerate() {
                let tag = el["tag"].as_str().unwrap_or("?");
                let id = el["id"].as_str().filter(|s| !s.is_empty());
                let class = el["class"].as_str().filter(|s| !s.is_empty());
                let text = el["text"].as_str().filter(|s| !s.is_empty());
                let href = el["href"].as_str().filter(|s| !s.is_empty());
                let visible = el["visible"].as_bool().unwrap_or(true);
                
                // Build selector hint
                let selector_hint = if let Some(id) = id {
                    format!("#{}", id)
                } else if let Some(cls) = class {
                    let first_class = cls.split_whitespace().next().unwrap_or("");
                    format!("{}.{}", tag, first_class)
                } else {
                    tag.to_string()
                };

                output.push_str(&format!(
                    "{}. [{}] {}",
                    i + 1,
                    if visible { "✓" } else { "hidden" },
                    selector_hint
                ));
                
                if let Some(t) = text {
                    output.push_str(&format!(" - \"{}\"", t.chars().take(60).collect::<String>()));
                }
                if let Some(h) = href {
                    output.push_str(&format!(" → {}", h.chars().take(50).collect::<String>()));
                }
                output.push('\n');
            }

            Ok(output)
        })
        .await
    }
}
