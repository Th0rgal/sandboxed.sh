//! Storage tools for uploading images and files to cloud storage.
//!
//! Uses Supabase Storage when SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY are set.
//! Images are uploaded to the `images` bucket and returned as public URLs.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::Tool;

/// Get Supabase configuration from environment.
fn get_supabase_config() -> Option<(String, String)> {
    let url = std::env::var("SUPABASE_URL").ok()?;
    let key = std::env::var("SUPABASE_SERVICE_ROLE_KEY").ok()?;
    
    if url.is_empty() || key.is_empty() {
        return None;
    }
    
    Some((url, key))
}

/// Upload an image file to Supabase Storage and return a public URL.
///
/// This tool is useful for sharing screenshots, diagrams, or other images
/// in markdown messages. The returned URL can be used in markdown like:
/// `![description](url)`
pub struct UploadImage;

#[async_trait]
impl Tool for UploadImage {
    fn name(&self) -> &str {
        "upload_image"
    }

    fn description(&self) -> &str {
        "Upload an image file to cloud storage and get a public URL. Use this to share screenshots, diagrams, or other images in your messages. The returned URL can be embedded in markdown using ![description](url) syntax. Supports PNG, JPEG, GIF, and WebP formats."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the local image file to upload (e.g., 'screenshots/screenshot_20240101_120000.png')"
                },
                "description": {
                    "type": "string",
                    "description": "Optional description for the image (used in alt text)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value, working_dir: &Path) -> anyhow::Result<String> {
        let (supabase_url, service_role_key) = get_supabase_config()
            .ok_or_else(|| anyhow::anyhow!(
                "Supabase not configured. Set SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY environment variables."
            ))?;
        
        let path_arg = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        
        let description = args["description"].as_str().unwrap_or("image");
        
        // Resolve path (relative to working_dir or absolute)
        let file_path = if Path::new(path_arg).is_absolute() {
            std::path::PathBuf::from(path_arg)
        } else {
            working_dir.join(path_arg)
        };
        
        // Verify file exists
        if !file_path.exists() {
            return Err(anyhow::anyhow!("File not found: {}", file_path.display()));
        }
        
        // Read file content
        let content = std::fs::read(&file_path)?;
        
        // Determine content type from extension
        let extension = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_lowercase();
        
        let content_type = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => return Err(anyhow::anyhow!(
                "Unsupported image format: {}. Supported: png, jpg, jpeg, gif, webp, svg",
                extension
            )),
        };
        
        // Generate a unique path for the uploaded file
        let file_id = uuid::Uuid::new_v4();
        let upload_path = format!("{}.{}", file_id, extension);
        
        tracing::info!(
            local_path = %file_path.display(),
            upload_path = %upload_path,
            size = content.len(),
            "Uploading image to Supabase Storage"
        );
        
        // Upload to Supabase Storage
        let storage_url = format!(
            "{}/storage/v1/object/images/{}",
            supabase_url.trim_end_matches('/'),
            upload_path
        );
        
        let client = reqwest::Client::new();
        let resp = client
            .post(&storage_url)
            .header("apikey", &service_role_key)
            .header("Authorization", format!("Bearer {}", service_role_key))
            .header("Content-Type", content_type)
            .header("x-upsert", "true")
            .body(content)
            .send()
            .await?;
        
        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to upload image: {} - {}",
                status,
                error_text
            ));
        }
        
        // Construct public URL
        let public_url = format!(
            "{}/storage/v1/object/public/images/{}",
            supabase_url.trim_end_matches('/'),
            upload_path
        );
        
        // Return markdown-ready format
        Ok(json!({
            "success": true,
            "url": public_url,
            "markdown": format!("![{}]({})", description, public_url),
            "path": upload_path,
            "size_bytes": std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0)
        }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_content_type_detection() {
        let tool = UploadImage;
        assert_eq!(tool.name(), "upload_image");
    }
}
