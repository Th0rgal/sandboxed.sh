//! Web access tools: fetch URLs.
//!
//! Only the `fetch_url` tool remains; search is handled upstream by OpenCode/OMO agents.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::Tool;

/// Fetch content from a URL.
///
/// For large responses (>20KB), saves the full content to /tmp/ and returns
/// the file path along with a preview to avoid truncation.
pub struct FetchUrl;

#[async_trait]
impl Tool for FetchUrl {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL. For small responses (<20KB), returns the content directly. For large responses, saves the full content to /tmp/ and returns the file path with a preview. Useful for reading documentation, APIs, or downloading data."
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
            let tmp_dir = Path::new("/tmp");

            // Generate unique filename
            let filename = format!("fetch_{}.{}", Uuid::new_v4(), extension);
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

/// Basic HTML entity decoding.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
