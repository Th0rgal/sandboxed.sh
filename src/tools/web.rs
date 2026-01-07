//! Web access tools: search and fetch URLs.
//!
//! Web search uses Tavily API if TAVILY_API_KEY is set, otherwise falls back to DuckDuckGo HTML.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Tool;

/// Search the web using Tavily API (preferred) or DuckDuckGo fallback.
pub struct WebSearch;

/// Tavily API request body.
#[derive(Debug, Serialize)]
struct TavilySearchRequest {
    api_key: String,
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_results: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_answer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_raw_content: Option<bool>,
}

/// Tavily API response.
#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyResult>,
}

/// A single result from Tavily.
#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
    #[serde(default)]
    score: f64,
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for real-time information. Returns search results with titles, snippets and URLs. Use for finding documentation, current events, examples, or any information you need."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 10)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' argument"))?;
        let num_results = args["num_results"].as_u64().unwrap_or(5).min(10) as u32;

        // Try Tavily first if API key is available
        if let Ok(api_key) = std::env::var("TAVILY_API_KEY") {
            if !api_key.is_empty() {
                return search_tavily(&api_key, query, num_results).await;
            }
        }

        // Fallback to DuckDuckGo (may be blocked by CAPTCHA)
        search_duckduckgo(query).await
    }
}

/// Search using Tavily API.
async fn search_tavily(api_key: &str, query: &str, max_results: u32) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let request = TavilySearchRequest {
        api_key: api_key.to_string(),
        query: query.to_string(),
        max_results: Some(max_results),
        include_answer: Some(true),
        include_raw_content: Some(false),
    };

    let response = client
        .post("https://api.tavily.com/search")
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("Tavily API error ({}): {}", status, error_text);
    }

    let tavily_response: TavilySearchResponse = response.json().await?;

    if tavily_response.results.is_empty() {
        return Ok(format!("No results found for: {}", query));
    }

    // Format results
    let mut output = String::new();

    // Include AI-generated answer if available
    if let Some(answer) = tavily_response.answer {
        if !answer.is_empty() {
            output.push_str("## Quick Answer\n\n");
            output.push_str(&answer);
            output.push_str("\n\n---\n\n## Sources\n\n");
        }
    }

    // Format individual results
    for (i, result) in tavily_response.results.iter().enumerate() {
        output.push_str(&format!(
            "### {}. {}\n**URL:** {}\n**Score:** {:.2}\n\n{}\n\n",
            i + 1,
            result.title,
            result.url,
            result.score,
            result.content
        ));
    }

    Ok(output)
}

/// Fallback search using DuckDuckGo HTML (may be blocked by CAPTCHA).
async fn search_duckduckgo(query: &str) -> anyhow::Result<String> {
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; OpenAgent/1.0)")
        .build()?;

    let response = client.get(&url).send().await?;
    let html = response.text().await?;

    // Check for CAPTCHA
    if html.contains("anomaly-modal") || html.contains("Unfortunately, bots") {
        return Err(anyhow::anyhow!(
            "DuckDuckGo blocked the request with CAPTCHA. Please configure TAVILY_API_KEY for reliable web search."
        ));
    }

    // Parse results (simple extraction)
    let results = extract_ddg_results(&html);

    if results.is_empty() {
        Ok(format!("No results found for: {}", query))
    } else {
        Ok(results.join("\n\n"))
    }
}

/// Extract search results from DuckDuckGo HTML.
fn extract_ddg_results(html: &str) -> Vec<String> {
    let mut results = Vec::new();

    // Simple regex-free extraction
    // Look for result divs
    for (i, chunk) in html.split("class=\"result__body\"").enumerate().skip(1) {
        if i > 5 {
            break;
        }

        // Extract title
        let title = chunk
            .split("class=\"result__a\"")
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split('<').next())
            .unwrap_or("No title");

        // Extract snippet
        let snippet = chunk
            .split("class=\"result__snippet\"")
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split('<').next())
            .unwrap_or("No snippet");

        // Extract URL
        let url = chunk
            .split("class=\"result__url\"")
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split('<').next())
            .map(|s| s.trim())
            .unwrap_or("");

        if !title.is_empty() && title != "No title" {
            results.push(format!(
                "**{}**\n{}\nURL: {}",
                html_decode(title),
                html_decode(snippet),
                url
            ));
        }
    }

    results
}

/// Basic HTML entity decoding.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Fetch content from a URL.
///
/// For large responses (>20KB), saves the full content to /root/tmp/ and returns
/// the file path along with a preview. This ensures no data is lost due to truncation.
pub struct FetchUrl;

#[async_trait]
impl Tool for FetchUrl {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL. For small responses (<20KB), returns the content directly. For large responses, saves the full content to /root/tmp/ and returns the file path with a preview. Useful for reading documentation, APIs, or downloading data."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value, _workspace: &Path) -> anyhow::Result<String> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' argument"))?;

        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; OpenAgent/1.0)")
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let response = client.get(url).send().await?;
        let status = response.status();

        if !status.is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", status));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let body = response.text().await?;

        // Determine file extension from content type
        let extension = if content_type.contains("application/json") {
            "json"
        } else if content_type.contains("text/html") {
            "html"
        } else if content_type.contains("text/csv") {
            "csv"
        } else if content_type.contains("text/xml") || content_type.contains("application/xml") {
            "xml"
        } else {
            "txt"
        };

        // If HTML, try to extract text content for display
        let display_content = if content_type.contains("text/html") {
            extract_text_from_html(&body)
        } else {
            body.clone()
        };

        // For large responses, save to file and return path
        const MAX_INLINE_SIZE: usize = 20000;
        if body.len() > MAX_INLINE_SIZE {
            // Use /tmp which works on all systems (macOS, Linux)
            let tmp_dir = Path::new("/tmp");

            // Generate unique filename
            let file_id = uuid::Uuid::new_v4();
            let filename = format!("fetch_{}.{}", file_id, extension);
            let file_path = tmp_dir.join(&filename);

            // Save full content to file
            std::fs::write(&file_path, &body)?;

            // Return path with preview (safe for UTF-8)
            let preview_len = std::cmp::min(2000, display_content.len());
            let safe_end = super::safe_truncate_index(&display_content, preview_len);
            let preview = &display_content[..safe_end];

            Ok(format!(
                "Response too large ({} bytes). Full content saved to: {}\n\nPreview (first {} chars):\n{}{}",
                body.len(),
                file_path.display(),
                safe_end,
                preview,
                if display_content.len() > safe_end { "\n..." } else { "" }
            ))
        } else {
            Ok(display_content)
        }
    }
}

/// Extract readable text from HTML (simple approach).
fn extract_text_from_html(html: &str) -> String {
    // Remove script and style tags
    let mut text = html.to_string();

    // Remove scripts
    while let Some(start) = text.find("<script") {
        if let Some(end) = text[start..].find("</script>") {
            text = format!("{}{}", &text[..start], &text[start + end + 9..]);
        } else {
            break;
        }
    }

    // Remove styles
    while let Some(start) = text.find("<style") {
        if let Some(end) = text[start..].find("</style>") {
            text = format!("{}{}", &text[..start], &text[start + end + 8..]);
        } else {
            break;
        }
    }

    // Remove all tags
    let mut result = String::new();
    let mut in_tag = false;

    for c in text.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
            result.push(' ');
        } else if !in_tag {
            result.push(c);
        }
    }

    // Clean up whitespace
    let result: String = result.split_whitespace().collect::<Vec<_>>().join(" ");

    html_decode(&result)
}
