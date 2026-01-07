//! Browser automation tools using Chrome DevTools Protocol (CDP).
//!
//! These tools can either connect to an existing Chrome instance or launch one automatically.
//!
//! Environment variables:
//! - `BROWSER_CDP_URL`: CDP WebSocket URL for connecting to existing Chrome (default: `http://127.0.0.1:9222`)
//! - `BROWSER_ENABLED`: Set to `true` to enable browser tools (default: false)
//! - `BROWSER_PROXY`: Proxy URL with optional auth (format: `user:pass@host:port` or `host:port`)
//! - `BROWSER_HEADLESS`: Set to `true` for headless mode (default: true)
//! - `BROWSER_LAUNCH`: Set to `true` to launch Chrome instead of connecting to existing (default: false)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::Tool;

/// Default CDP endpoint
const DEFAULT_CDP_URL: &str = "http://127.0.0.1:9222";

/// Parsed proxy configuration
#[derive(Debug, Clone)]
struct ProxyConfig {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    scheme: String, // "http", "https", or "socks5"
}

impl ProxyConfig {
    /// Parse proxy URL from environment variable
    /// Formats supported:
    /// - `host:port` (no auth, defaults to http)
    /// - `user:pass@host:port` (with auth)
    /// - `socks5://host:port` (explicit scheme)
    /// - `socks5://user:pass@host:port`
    fn from_env() -> Option<Self> {
        let proxy_str = std::env::var("BROWSER_PROXY").ok()?;
        let proxy_str = proxy_str.trim();
        if proxy_str.is_empty() {
            return None;
        }

        // Parse scheme prefix
        let (scheme, rest) = if proxy_str.starts_with("socks5://") {
            ("socks5".to_string(), &proxy_str[9..])
        } else if proxy_str.starts_with("http://") {
            ("http".to_string(), &proxy_str[7..])
        } else if proxy_str.starts_with("https://") {
            ("https".to_string(), &proxy_str[8..])
        } else {
            ("http".to_string(), proxy_str)
        };

        // Check for auth credentials (user:pass@host:port)
        if let Some(at_pos) = rest.rfind('@') {
            let auth = &rest[..at_pos];
            let host_port = &rest[at_pos + 1..];

            // Parse auth (user:pass)
            let (username, password) = if let Some(colon_pos) = auth.find(':') {
                (
                    Some(auth[..colon_pos].to_string()),
                    Some(auth[colon_pos + 1..].to_string()),
                )
            } else {
                (Some(auth.to_string()), None)
            };

            // Parse host:port
            if let Some(colon_pos) = host_port.rfind(':') {
                let host = host_port[..colon_pos].to_string();
                let port: u16 = host_port[colon_pos + 1..].parse().ok()?;
                return Some(Self {
                    host,
                    port,
                    username,
                    password,
                    scheme,
                });
            }
        } else {
            // No auth, just host:port
            if let Some(colon_pos) = rest.rfind(':') {
                let host = rest[..colon_pos].to_string();
                let port: u16 = rest[colon_pos + 1..].parse().ok()?;
                return Some(Self {
                    host,
                    port,
                    username: None,
                    password: None,
                    scheme,
                });
            }
        }

        None
    }

    /// Create the proxy extension directory with configured credentials
    #[allow(dead_code)]
    fn create_extension(&self) -> anyhow::Result<PathBuf> {
        let ext_dir = std::env::temp_dir().join("open_agent_proxy_ext");
        std::fs::create_dir_all(&ext_dir)?;

        // Write manifest.json
        let manifest = r#"{
  "manifest_version": 3,
  "name": "Proxy Auth Extension",
  "version": "1.0",
  "description": "Handles proxy authentication",
  "permissions": [
    "proxy",
    "webRequest",
    "webRequestAuthProvider"
  ],
  "host_permissions": [
    "<all_urls>"
  ],
  "background": {
    "service_worker": "background.js"
  }
}"#;
        std::fs::write(ext_dir.join("manifest.json"), manifest)?;

        // Write background.js with actual credentials
        let background_js = format!(
            r#"// Proxy configuration
const PROXY_HOST = "{}";
const PROXY_PORT = {};
const PROXY_USER = "{}";
const PROXY_PASS = "{}";
const PROXY_SCHEME = "{}";

// Configure proxy settings
const proxyConfig = {{
  mode: "fixed_servers",
  rules: {{
    singleProxy: {{
      scheme: PROXY_SCHEME === "socks5" ? "socks5" : "http",
      host: PROXY_HOST,
      port: PROXY_PORT
    }},
    bypassList: ["localhost", "127.0.0.1"]
  }}
}};

chrome.proxy.settings.set(
  {{ value: proxyConfig, scope: "regular" }},
  () => console.log("Proxy configured:", PROXY_HOST + ":" + PROXY_PORT)
);

// Handle proxy authentication (HTTP/HTTPS proxies only)
chrome.webRequest.onAuthRequired.addListener(
  (details, callback) => {{
    console.log("Auth required for:", details.challenger);
    callback({{
      authCredentials: {{
        username: PROXY_USER,
        password: PROXY_PASS
      }}
    }});
  }},
  {{ urls: ["<all_urls>"] }},
  ["asyncBlocking"]
);

console.log("Proxy extension loaded - scheme:", PROXY_SCHEME, "host:", PROXY_HOST);
"#,
            self.host,
            self.port,
            self.username.as_deref().unwrap_or(""),
            self.password.as_deref().unwrap_or(""),
            self.scheme
        );
        std::fs::write(ext_dir.join("background.js"), background_js)?;

        Ok(ext_dir)
    }

    /// Get Chrome proxy argument
    fn chrome_arg(&self) -> String {
        format!(
            "--proxy-server={}://{}:{}",
            self.scheme, self.host, self.port
        )
    }
}

/// Shared browser state (lazy initialization)
static BROWSER_STATE: std::sync::LazyLock<Arc<Mutex<Option<BrowserSession>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Browser session holding the browser and current page
struct BrowserSession {
    #[allow(dead_code)]
    browser: Browser,
    page: Page,
    #[allow(dead_code)]
    proxy_ext_dir: Option<PathBuf>, // Keep reference to prevent cleanup
}

/// Get or create a browser session
async fn get_browser_session() -> anyhow::Result<Arc<Mutex<Option<BrowserSession>>>> {
    let state = BROWSER_STATE.clone();
    let mut guard = state.lock().await;

    if guard.is_none() {
        let should_launch = std::env::var("BROWSER_LAUNCH")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let proxy_config = ProxyConfig::from_env();

        let (browser, proxy_ext_dir) = if should_launch || proxy_config.is_some() {
            // Launch Chrome with custom configuration
            let (browser_instance, mut handler, ext_dir) = launch_browser(proxy_config).await?;

            // Spawn handler in background
            tokio::spawn(async move {
                while let Some(event) = handler.next().await {
                    if let Err(e) = event {
                        tracing::warn!("Browser event error: {}", e);
                    }
                }
            });

            (browser_instance, ext_dir)
        } else {
            // Connect to existing Chrome
            let cdp_url =
                std::env::var("BROWSER_CDP_URL").unwrap_or_else(|_| DEFAULT_CDP_URL.to_string());

            tracing::info!("Connecting to existing Chrome at {}", cdp_url);

            let (browser_instance, mut handler) = Browser::connect(&cdp_url).await.map_err(|e| {
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

            (browser_instance, None)
        };

        // Get or create a page
        let page = browser.new_page("about:blank").await?;

        *guard = Some(BrowserSession {
            browser,
            page,
            proxy_ext_dir,
        });
    }

    drop(guard);
    Ok(state)
}

/// Local port for gost proxy forwarder
const GOST_LOCAL_PORT: u16 = 18080;

/// Launch a new Chrome instance with optional proxy configuration.
///
/// When proxy auth is needed, we start a local gost proxy forwarder that handles
/// authentication with the upstream proxy. Chrome connects to the local proxy
/// without needing any auth.
async fn launch_browser(
    proxy_config: Option<ProxyConfig>,
) -> anyhow::Result<(Browser, chromiumoxide::Handler, Option<PathBuf>)> {
    let headless = std::env::var("BROWSER_HEADLESS")
        .map(|v| v.to_lowercase() != "false" && v != "0")
        .unwrap_or(true);

    let mut config_builder = BrowserConfig::builder();

    // Configure headless mode
    if headless {
        config_builder = config_builder.arg("--headless=new");
    } else {
        config_builder = config_builder.with_head();
    }

    // Add common Chrome arguments for stability
    config_builder = config_builder
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-sync")
        .arg("--disable-translate");

    // Configure proxy if provided
    if let Some(ref proxy) = proxy_config {
        tracing::info!(
            "Configuring browser proxy: {}://{}:{} (auth: {})",
            proxy.scheme,
            proxy.host,
            proxy.port,
            proxy.username.is_some()
        );

        if proxy.username.is_some() {
            // Start local gost proxy forwarder to handle authentication
            start_gost_forwarder(proxy).await?;

            // Point Chrome to local proxy (no auth needed)
            config_builder = config_builder.arg(format!(
                "--proxy-server=http://127.0.0.1:{}",
                GOST_LOCAL_PORT
            ));
            tracing::info!(
                "Chrome using local proxy forwarder at 127.0.0.1:{}",
                GOST_LOCAL_PORT
            );
        } else {
            // No auth needed, connect directly
            config_builder = config_builder.arg(&proxy.chrome_arg());
        }
    }

    let config = config_builder
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build browser config: {}", e))?;

    tracing::info!("Launching Chrome browser (headless: {})...", headless);

    let (browser, handler) = Browser::launch(config).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to launch Chrome. Make sure chromium/google-chrome is installed. Error: {}",
            e
        )
    })?;

    tracing::info!("Chrome browser launched successfully");

    Ok((browser, handler, None))
}

/// Start gost as a local proxy forwarder that handles upstream authentication.
/// gost is a Go-based tunnel tool that supports proxy chaining with auth.
async fn start_gost_forwarder(proxy: &ProxyConfig) -> anyhow::Result<()> {
    use tokio::process::Command;

    // Check if gost is already running on our port
    if let Ok(output) = Command::new("lsof")
        .args(["-i", &format!(":{}", GOST_LOCAL_PORT)])
        .output()
        .await
    {
        if !output.stdout.is_empty() {
            tracing::info!(
                "gost proxy forwarder already running on port {}",
                GOST_LOCAL_PORT
            );
            return Ok(());
        }
    }

    // Build upstream proxy URL with auth
    let upstream_url = if let (Some(user), Some(pass)) = (&proxy.username, &proxy.password) {
        format!(
            "{}://{}:{}@{}:{}",
            proxy.scheme, user, pass, proxy.host, proxy.port
        )
    } else {
        format!("{}://{}:{}", proxy.scheme, proxy.host, proxy.port)
    };

    // Start gost
    let gost = Command::new("gost")
        .args([
            &format!("-L=:{}", GOST_LOCAL_PORT),
            &format!("-F={}", upstream_url),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to start gost proxy forwarder: {}. Make sure gost is installed.",
                e
            )
        })?;

    // Wait for gost to be ready
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    tracing::info!(
        "Started gost proxy forwarder on 127.0.0.1:{} -> {}:{}",
        GOST_LOCAL_PORT,
        proxy.host,
        proxy.port
    );

    // Keep gost running (leak the handle intentionally)
    std::mem::forget(gost);

    Ok(())
}

/// Start a virtual X11 display using Xvfb
#[allow(dead_code)]
async fn start_virtual_display() -> anyhow::Result<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::process::Command;

    static DISPLAY_COUNTER: AtomicU32 = AtomicU32::new(50);

    let display_num = DISPLAY_COUNTER.fetch_add(1, Ordering::SeqCst);
    let display_id = format!(":{}", display_num);
    let resolution =
        std::env::var("DESKTOP_RESOLUTION").unwrap_or_else(|_| "1920x1080".to_string());

    // Clean up any existing files
    let lock_file = format!("/tmp/.X{}-lock", display_num);
    let socket_file = format!("/tmp/.X11-unix/X{}", display_num);
    let _ = std::fs::remove_file(&lock_file);
    let _ = std::fs::remove_file(&socket_file);

    // Start Xvfb
    let xvfb_args = format!("{} -screen 0 {}x24", display_id, resolution);
    let mut xvfb = Command::new("Xvfb")
        .args(xvfb_args.split_whitespace())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to start Xvfb: {}. Is Xvfb installed?", e))?;

    // Wait for Xvfb to be ready
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify Xvfb is running
    if let Ok(Some(status)) = xvfb.try_wait() {
        return Err(anyhow::anyhow!(
            "Xvfb exited immediately with status: {:?}",
            status
        ));
    }

    tracing::info!(
        "Xvfb started on display {} (pid: {:?})",
        display_id,
        xvfb.id()
    );

    // Keep the process handle alive by leaking it (it will be cleaned up on process exit)
    // This is intentional - we want Xvfb to keep running
    std::mem::forget(xvfb);

    Ok(display_id)
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
        "Take a screenshot of the current browser page.\n\n\
        BEFORE screenshotting to share with user:\n\
        1. Use browser_get_content to verify the page loaded correctly\n\
        2. Check for 404 errors, loading states, or empty pages\n\
        3. Only screenshot pages with actual content\n\n\
        Modes:\n\
        - return_image=true: YOU can see the screenshot (requires vision model)\n\
        - upload=true: Screenshot is uploaded and you get markdown to share\n\n\
        When sharing, you MUST include the 'markdown' value in your response!"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Description of what the screenshot shows (used in alt text). Default: 'screenshot'"
                },
                "full_page": {
                    "type": "boolean",
                    "description": "Capture the full scrollable page (default: false, captures viewport only)"
                },
                "return_image": {
                    "type": "boolean",
                    "description": "If true, the screenshot will be included in your context so YOU can see it (requires vision model). Use this to verify page content before responding. Default: false"
                },
                "upload": {
                    "type": "boolean",
                    "description": "If true, uploads screenshot and returns markdown for sharing with the user. Default: true"
                }
            }
        })
    }

    async fn execute(&self, args: Value, workspace: &Path) -> anyhow::Result<String> {
        let description = args["description"].as_str().unwrap_or("screenshot");
        let full_page = args["full_page"].as_bool().unwrap_or(false);
        let return_image = args["return_image"].as_bool().unwrap_or(false);
        let upload = args["upload"].as_bool().unwrap_or(true);
        let filename = format!(
            "screenshot_{}.png",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        );

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

        // Try to upload to Supabase if requested and configured
        let mut public_url: Option<String> = None;
        let mut markdown: Option<String> = None;

        if upload {
            if let Some((supabase_url, service_role_key)) = get_supabase_config() {
                match upload_to_supabase(&screenshot, &supabase_url, &service_role_key).await {
                    Ok(url) => {
                        tracing::info!(
                            local_path = %file_path.display(),
                            public_url = %url,
                            size = screenshot.len(),
                            "Screenshot uploaded to Supabase"
                        );
                        markdown = Some(format!("![{}]({})", description, url));
                        public_url = Some(url);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to upload screenshot to Supabase: {}", e);
                    }
                }
            }
        }

        // Build response
        let mut result = json!({
            "success": true,
            "local_path": file_path.display().to_string(),
            "size_bytes": screenshot.len()
        });

        if let Some(url) = &public_url {
            result["url"] = json!(url);
        }
        if let Some(md) = &markdown {
            result["markdown"] = json!(md);
            result["message"] = json!("Screenshot uploaded! Include the 'markdown' value in your response for the user to see it.");
        }

        // Add vision marker if return_image is true (so agent can SEE the screenshot)
        // Format: [VISION_IMAGE:url] - parsed by executor to include image in context
        let vision_marker = if return_image {
            if let Some(url) = &public_url {
                format!("\n\n[VISION_IMAGE:{}]", url)
            } else {
                // Can't do vision without a URL - need to upload first
                "\n\nNote: return_image requires upload=true to work (need URL for vision)"
                    .to_string()
            }
        } else {
            String::new()
        };

        Ok(format!("{}{}", result.to_string(), vision_marker))
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
                let element = page
                    .find_element(sel)
                    .await
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

            // Truncate if too long (safe for UTF-8)
            let max_len = 50000;
            if content.len() > max_len {
                let safe_end = super::safe_truncate_index(&content, max_len);
                Ok(format!(
                    "{}\n\n... [truncated, {} total characters]",
                    &content[..safe_end],
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
        "Click on an element in the browser. Use standard CSS selectors like '#id', '.class', 'button', 'a[href*=login]', etc. NOTE: jQuery pseudo-selectors like :contains() are NOT supported - use XPath or JavaScript for text matching instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Standard CSS selector (e.g., '#id', '.class', 'button', 'a[href*=download]'). Do NOT use jQuery :contains() - it's not valid CSS."
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
            let element = page
                .find_element(selector)
                .await
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
            let element = page
                .find_element(selector)
                .await
                .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;

            // Click to focus
            element.click().await?;

            // Clear if requested
            if clear_first {
                // Select all and delete
                element.type_str("").await?; // Focus
                page.evaluate("document.activeElement.value = ''")
                    .await
                    .ok();
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
        "Execute JavaScript code in the browser and return the result. Useful for complex interactions, extracting data, or debugging. NOTE: DevTools-only APIs like getEventListeners() are NOT available - use standard DOM APIs only."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "JavaScript code to execute. Use standard DOM APIs only - DevTools-only functions like getEventListeners() are not available. The result of the last expression is returned."
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
        "Wait for an element to appear or a condition to be met. Use after clicking or navigating when content loads dynamically. NOTE: Use standard CSS selectors only - jQuery pseudo-selectors like :contains() are NOT supported."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Standard CSS selector to wait for (e.g., '#id', '.class', 'div.loaded'). Do NOT use jQuery :contains()."
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
        "List interactive elements on the current page (links, buttons, inputs, etc.). Useful for understanding page structure before interacting. NOTE: Use standard CSS selectors only - jQuery pseudo-selectors like :contains() are NOT supported."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Standard CSS selector to filter elements (default: 'a, button, input, select, textarea, [onclick]'). Do NOT use jQuery :contains() - it's not valid CSS."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of elements to return (default: 50)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let selector = args["selector"]
            .as_str()
            .unwrap_or("a, button, input, select, textarea, [onclick], [role='button']");
        let limit = args["limit"].as_u64().unwrap_or(50) as usize;

        with_page(|page| async move {
            // Use JavaScript to get element info
            let script = format!(
                r#"
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
            "#,
                selector, limit
            );

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
                    output.push_str(&format!(
                        " - \"{}\"",
                        t.chars().take(60).collect::<String>()
                    ));
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
