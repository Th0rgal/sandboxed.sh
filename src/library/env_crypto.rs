//! Encryption utilities for workspace template environment variables.
//!
//! Uses AES-256-GCM with a static key stored in PRIVATE_KEY environment variable.
//! Encrypted values are wrapped in `<encrypted v="1">BASE64</encrypted>` format
//! for autodetection. Plaintext values (no wrapper) are treated as legacy.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use std::collections::HashMap;
use tokio::fs;

/// Key length in bytes (256 bits for AES-256)
const KEY_LENGTH: usize = 32;

/// Nonce length in bytes (96 bits for AES-GCM)
const NONCE_LENGTH: usize = 12;

/// Environment variable name for the encryption key
pub const PRIVATE_KEY_ENV: &str = "PRIVATE_KEY";

/// Current encryption format version
const ENCRYPTION_VERSION: &str = "1";

/// Wrapper prefix for encrypted values
const ENCRYPTED_PREFIX: &str = "<encrypted v=\"";
const ENCRYPTED_SUFFIX: &str = "</encrypted>";

/// Check if a value is encrypted (has the wrapper format).
pub fn is_encrypted(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with(ENCRYPTED_PREFIX) && trimmed.ends_with(ENCRYPTED_SUFFIX)
}

/// Parse an encrypted value, returning (version, base64_payload).
fn parse_encrypted(value: &str) -> Option<(&str, &str)> {
    let trimmed = value.trim();
    if !trimmed.starts_with(ENCRYPTED_PREFIX) || !trimmed.ends_with(ENCRYPTED_SUFFIX) {
        return None;
    }

    // Find the closing `">` of the version attribute
    let after_prefix = &trimmed[ENCRYPTED_PREFIX.len()..];
    let version_end = after_prefix.find("\">")?;
    let version = &after_prefix[..version_end];

    // Extract the base64 payload between `">` and `</encrypted>`
    let payload_start = ENCRYPTED_PREFIX.len() + version_end + 2; // +2 for `">`
    let payload_end = trimmed.len() - ENCRYPTED_SUFFIX.len();
    let payload = &trimmed[payload_start..payload_end];

    Some((version, payload))
}

/// Encrypt a plaintext value using AES-256-GCM.
/// Returns the value wrapped in `<encrypted v="1">BASE64(nonce||ciphertext)</encrypted>`.
pub fn encrypt_value(key: &[u8; KEY_LENGTH], plaintext: &str) -> Result<String> {
    // Don't double-encrypt
    if is_encrypted(plaintext) {
        return Ok(plaintext.to_string());
    }

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_LENGTH];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    // Create cipher and encrypt
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("Failed to create cipher: {}", e))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;

    // Combine nonce + ciphertext and encode
    let mut combined = Vec::with_capacity(NONCE_LENGTH + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    let encoded = BASE64.encode(&combined);

    Ok(format!(
        "<encrypted v=\"{}\">{}</encrypted>",
        ENCRYPTION_VERSION, encoded
    ))
}

/// Decrypt an encrypted value.
/// If the value is plaintext (no wrapper), returns it unchanged.
pub fn decrypt_value(key: &[u8; KEY_LENGTH], value: &str) -> Result<String> {
    // Passthrough plaintext values
    let (version, payload) = match parse_encrypted(value) {
        Some(parsed) => parsed,
        None => return Ok(value.to_string()),
    };

    // Validate version
    if version != ENCRYPTION_VERSION {
        return Err(anyhow!(
            "Unsupported encryption version: {}. Expected: {}",
            version,
            ENCRYPTION_VERSION
        ));
    }

    // Decode base64
    let combined = BASE64
        .decode(payload)
        .context("Failed to decode encrypted value")?;

    if combined.len() < NONCE_LENGTH {
        return Err(anyhow!("Encrypted value too short"));
    }

    // Split nonce and ciphertext
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LENGTH);

    // Create cipher and decrypt
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("Failed to create cipher: {}", e))?;
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("Decryption failed: invalid key or corrupted data"))?;

    String::from_utf8(plaintext).context("Decrypted value is not valid UTF-8")
}

/// Encrypt all values in an env_vars HashMap.
/// Values that are already encrypted are left unchanged.
pub fn encrypt_env_vars(
    key: &[u8; KEY_LENGTH],
    env_vars: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut encrypted = HashMap::with_capacity(env_vars.len());
    for (k, v) in env_vars {
        encrypted.insert(k.clone(), encrypt_value(key, v)?);
    }
    Ok(encrypted)
}

/// Decrypt all values in an env_vars HashMap.
/// Plaintext values are passed through unchanged.
pub fn decrypt_env_vars(
    key: &[u8; KEY_LENGTH],
    env_vars: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut decrypted = HashMap::with_capacity(env_vars.len());
    for (k, v) in env_vars {
        decrypted.insert(k.clone(), decrypt_value(key, v)?);
    }
    Ok(decrypted)
}

/// Result of graceful decryption
pub struct GracefulDecryptResult {
    pub env_vars: HashMap<String, String>,
    pub failed_keys: Vec<String>,
}

/// Decrypt all values in an env_vars HashMap, handling failures gracefully.
/// Values that fail to decrypt are returned with a special marker prefix.
/// Returns both the (possibly partially decrypted) env vars and a list of keys that failed.
pub fn decrypt_env_vars_graceful(
    key: &[u8; KEY_LENGTH],
    env_vars: &HashMap<String, String>,
) -> GracefulDecryptResult {
    let mut decrypted = HashMap::with_capacity(env_vars.len());
    let mut failed_keys = Vec::new();

    for (k, v) in env_vars {
        match decrypt_value(key, v) {
            Ok(plaintext) => {
                decrypted.insert(k.clone(), plaintext);
            }
            Err(e) => {
                tracing::warn!(
                    key = %k,
                    error = %e,
                    "Failed to decrypt env var, marking as failed"
                );
                // Keep the original encrypted value with a failure marker
                decrypted.insert(k.clone(), format!("[DECRYPTION_FAILED]{}", v));
                failed_keys.push(k.clone());
            }
        }
    }

    GracefulDecryptResult {
        env_vars: decrypted,
        failed_keys,
    }
}

/// Load the encryption key from environment.
/// Returns None if PRIVATE_KEY is not set.
pub fn load_private_key_from_env() -> Result<Option<[u8; KEY_LENGTH]>> {
    let key_str = match std::env::var(PRIVATE_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k,
        _ => return Ok(None),
    };

    parse_key(&key_str)
        .map(Some)
        .context("Invalid PRIVATE_KEY format")
}

/// Get the path to the private key file.
/// Uses `PRIVATE_KEY_FILE` env var, or defaults to `{WORKING_DIR}/.sandboxed-sh/private_key`.
fn private_key_file_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        return std::path::PathBuf::from(path);
    }
    let working_dir = std::env::var("WORKING_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/root"));
    working_dir.join(".sandboxed-sh").join("private_key")
}

/// Ensure a private key is available, generating one lazily if needed.
///
/// 1. Checks `PRIVATE_KEY` env var (fast path, no I/O).
/// 2. Reads from the key file (`{WORKING_DIR}/.sandboxed-sh/private_key`).
/// 3. Generates a new key, persists it to the file, and sets the env var.
pub async fn ensure_private_key() -> Result<[u8; KEY_LENGTH]> {
    // 1. Fast path: env var already set
    if let Some(key) = load_private_key_from_env()? {
        tracing::trace!("Using PRIVATE_KEY from environment variable");
        return Ok(key);
    }

    // 2. Try reading from persisted key file
    let key_file = private_key_file_path();
    tracing::debug!(
        key_file = %key_file.display(),
        exists = key_file.exists(),
        "Checking for private key file"
    );

    if key_file.exists() {
        match fs::read_to_string(&key_file).await {
            Ok(contents) => {
                let trimmed = contents.trim();
                if !trimmed.is_empty() {
                    let key = parse_key(trimmed).context("Invalid key in private_key file")?;
                    // Cache in process env for future calls
                    std::env::set_var(PRIVATE_KEY_ENV, trimmed);
                    tracing::info!(
                        key_file = %key_file.display(),
                        "Loaded PRIVATE_KEY from file"
                    );
                    return Ok(key);
                }
                tracing::warn!(
                    key_file = %key_file.display(),
                    "Private key file exists but is empty"
                );
            }
            Err(e) => {
                tracing::warn!(
                    key_file = %key_file.display(),
                    error = %e,
                    "Failed to read private key file"
                );
            }
        }
    }

    // 3. Generate new key and persist
    tracing::info!(
        key_file = %key_file.display(),
        "Generating new PRIVATE_KEY"
    );

    let key = generate_private_key();
    let key_hex = hex::encode(key);

    if let Some(parent) = key_file.parent() {
        fs::create_dir_all(parent)
            .await
            .context("Failed to create directory for private_key file")?;
    }
    fs::write(&key_file, &key_hex)
        .await
        .context("Failed to write private_key file")?;

    // Set in process env
    std::env::set_var(PRIVATE_KEY_ENV, &key_hex);

    tracing::info!(
        key_file = %key_file.display(),
        "Generated new PRIVATE_KEY and saved to file"
    );
    Ok(key)
}

/// Parse a key from hex or base64 format.
fn parse_key(key_str: &str) -> Result<[u8; KEY_LENGTH]> {
    let trimmed = key_str.trim();

    // Try hex first (64 characters = 32 bytes)
    if trimmed.len() == KEY_LENGTH * 2 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex::decode(trimmed).context("Invalid hex key")?;
        let mut key = [0u8; KEY_LENGTH];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // Try base64
    let bytes = BASE64
        .decode(trimmed)
        .context("Key is neither valid hex nor base64")?;

    if bytes.len() != KEY_LENGTH {
        return Err(anyhow!(
            "Key must be {} bytes, got {} bytes",
            KEY_LENGTH,
            bytes.len()
        ));
    }

    let mut key = [0u8; KEY_LENGTH];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Generate a new random encryption key.
pub fn generate_private_key() -> [u8; KEY_LENGTH] {
    let mut key = [0u8; KEY_LENGTH];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

// ─────────────────────────────────────────────────────────────────────────────
// Content encryption (for skill markdown files)
// ─────────────────────────────────────────────────────────────────────────────

/// Regex to match unversioned <encrypted>value</encrypted> tags (user input format).
const UNVERSIONED_TAG_REGEX: &str = r"<encrypted>([^<]*)</encrypted>";

/// Regex to match versioned <encrypted v="N">value</encrypted> tags (storage format).
const VERSIONED_TAG_REGEX: &str = r#"<encrypted v="(\d+)">([^<]*)</encrypted>"#;

/// Regex to match any encrypted tag (both versioned and unversioned).
const ANY_ENCRYPTED_TAG_REGEX: &str = r#"<encrypted(?:\s+v="\d+")?>([^<]*)</encrypted>"#;

/// Regex to match failed-to-decrypt tags.
const FAILED_ENCRYPTED_TAG_REGEX: &str =
    r#"<encrypted-failed(?:\s+v="\d+")?>([^<]*)</encrypted-failed>"#;

/// Check if a value is an unversioned encrypted tag (user input format).
pub fn is_unversioned_encrypted(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("<encrypted>")
        && trimmed.ends_with("</encrypted>")
        && !trimmed.contains(" v=\"")
}

/// Check if content contains any encrypted tags (versioned, unversioned, or failed).
pub fn has_encrypted_tags(content: &str) -> bool {
    content.contains("<encrypted>")
        || content.contains("<encrypted v=\"")
        || content.contains("<encrypted-failed")
}

/// Check if content contains any failed-to-decrypt tags.
pub fn has_failed_encrypted_tags(content: &str) -> bool {
    content.contains("<encrypted-failed")
}

/// Strip all <encrypted>...</encrypted> tags from content, leaving only the inner values.
///
/// This is used when deploying skills to workspaces where the actual plaintext
/// values are needed (after decryption has already been performed).
///
/// Handles versioned, unversioned, and failed tags:
/// - `<encrypted>plaintext</encrypted>` → `plaintext`
/// - `<encrypted v="1">ciphertext</encrypted>` → `ciphertext` (should not happen after decryption)
/// - `<encrypted-failed v="1">ciphertext</encrypted-failed>` → `[DECRYPTION_FAILED]` (placeholder)
pub fn strip_encrypted_tags(content: &str) -> String {
    // First handle failed tags with a placeholder
    let re_failed = regex::Regex::new(FAILED_ENCRYPTED_TAG_REGEX).expect("Invalid regex");
    let content = re_failed.replace_all(content, "[DECRYPTION_FAILED]");

    // Then handle normal encrypted tags
    let re = regex::Regex::new(ANY_ENCRYPTED_TAG_REGEX).expect("Invalid regex");
    re.replace_all(&content, "$1").to_string()
}

/// Encrypt all unversioned <encrypted>value</encrypted> tags in content.
/// Transforms <encrypted>plaintext</encrypted> to <encrypted v="1">ciphertext</encrypted>.
pub fn encrypt_content_tags(key: &[u8; KEY_LENGTH], content: &str) -> Result<String> {
    let re =
        regex::Regex::new(UNVERSIONED_TAG_REGEX).map_err(|e| anyhow!("Invalid regex: {}", e))?;

    let captures: Vec<_> = re.captures_iter(content).collect();

    if captures.is_empty() {
        tracing::trace!("No unversioned encrypted tags found in content");
        return Ok(content.to_string());
    }

    tracing::debug!(
        tags_found = captures.len(),
        "Encrypting unversioned <encrypted> tags in content"
    );

    let mut result = content.to_string();
    let mut offset: i64 = 0;

    for cap in captures {
        let full_match = cap.get(0).unwrap();
        let plaintext = cap.get(1).unwrap().as_str();

        // Skip if already versioned (shouldn't happen with this regex, but be safe)
        if full_match.as_str().contains(" v=\"") {
            continue;
        }

        // Encrypt the plaintext value
        let encrypted = encrypt_value(key, plaintext)?;

        // Calculate adjusted position with offset
        let start = (full_match.start() as i64 + offset) as usize;
        let end = (full_match.end() as i64 + offset) as usize;

        // Update offset for next replacement
        offset += encrypted.len() as i64 - full_match.len() as i64;

        // Replace in result
        result = format!("{}{}{}", &result[..start], encrypted, &result[end..]);
    }

    tracing::debug!("Successfully encrypted content tags");
    Ok(result)
}

/// Marker for encrypted values that failed to decrypt (wrong key).
const DECRYPT_FAILED_PREFIX: &str = "<encrypted-failed v=\"";

/// Check if a value is a failed-to-decrypt encrypted tag.
pub fn is_decrypt_failed(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with(DECRYPT_FAILED_PREFIX) && trimmed.ends_with("</encrypted-failed>")
}

/// Decrypt all versioned <encrypted v="N">ciphertext</encrypted> tags in content.
/// Transforms <encrypted v="1">ciphertext</encrypted> to <encrypted>plaintext</encrypted>.
/// If decryption fails (wrong key), transforms to <encrypted-failed v="1">ciphertext</encrypted-failed>
/// so the frontend can display it specially for the user to re-enter.
pub fn decrypt_content_tags(key: &[u8; KEY_LENGTH], content: &str) -> Result<String> {
    let re = regex::Regex::new(VERSIONED_TAG_REGEX).map_err(|e| anyhow!("Invalid regex: {}", e))?;

    let mut result = content.to_string();
    let mut offset: i64 = 0;

    for cap in re.captures_iter(content) {
        let full_match = cap.get(0).unwrap();
        let version = cap.get(1).unwrap().as_str();
        let ciphertext_b64 = cap.get(2).unwrap().as_str();

        // Reconstruct the full encrypted value for decryption
        let encrypted_value = full_match.as_str();

        // Try to decrypt, but handle failure gracefully
        let display_tag = match decrypt_value(key, encrypted_value) {
            Ok(plaintext) => {
                // Success: format as unversioned tag for display
                format!("<encrypted>{}</encrypted>", plaintext)
            }
            Err(e) => {
                // Failure: mark as failed so frontend can show it differently
                tracing::warn!(
                    error = %e,
                    "Failed to decrypt value, marking as encrypted-failed for user to re-enter"
                );
                format!(
                    "<encrypted-failed v=\"{}\">{}</encrypted-failed>",
                    version, ciphertext_b64
                )
            }
        };

        // Calculate adjusted position with offset
        let start = (full_match.start() as i64 + offset) as usize;
        let end = (full_match.end() as i64 + offset) as usize;

        // Update offset for next replacement
        offset += display_tag.len() as i64 - full_match.len() as i64;

        // Replace in result
        result = format!("{}{}{}", &result[..start], display_tag, &result[end..]);
    }

    Ok(result)
}

/// Get the hex-encoded private key from environment (for backup export).
/// Returns None if no key is configured.
pub fn get_private_key_hex() -> Option<String> {
    std::env::var(PRIVATE_KEY_ENV)
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Set the private key from a hex string (for backup restore).
/// Persists to the key file and sets the env var.
pub async fn set_private_key_hex(key_hex: &str) -> Result<()> {
    // Validate
    let _key = parse_key(key_hex).context("Invalid key format")?;

    // Persist to file
    let key_file = private_key_file_path();
    if let Some(parent) = key_file.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&key_file, key_hex.trim())
        .await
        .context("Failed to write private_key file")?;

    // Set in process env
    std::env::set_var(PRIVATE_KEY_ENV, key_hex.trim());

    tracing::info!("Restored PRIVATE_KEY from backup");
    Ok(())
}

/// Parse a key from a hex string.
/// Returns the 32-byte key if valid, or an error otherwise.
pub fn parse_key_hex(key_hex: &str) -> Result<[u8; KEY_LENGTH]> {
    parse_key(key_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; KEY_LENGTH] {
        let mut key = [0u8; KEY_LENGTH];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = i as u8;
        }
        key
    }

    #[test]
    fn test_is_encrypted() {
        assert!(is_encrypted("<encrypted v=\"1\">abc123</encrypted>"));
        assert!(is_encrypted("  <encrypted v=\"1\">abc123</encrypted>  "));
        assert!(!is_encrypted("plaintext"));
        assert!(!is_encrypted("<encrypted>missing version</encrypted>"));
        assert!(!is_encrypted("<encrypted v=\"1\">no closing tag"));
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = test_key();
        let plaintext = "my-secret-api-key-12345";

        let encrypted = encrypt_value(&key, plaintext).unwrap();
        assert!(is_encrypted(&encrypted));
        assert!(encrypted.starts_with("<encrypted v=\"1\">"));
        assert!(encrypted.ends_with("</encrypted>"));

        let decrypted = decrypt_value(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_plaintext_passthrough() {
        let key = test_key();
        let plaintext = "not-encrypted-value";

        let result = decrypt_value(&key, plaintext).unwrap();
        assert_eq!(result, plaintext);
    }

    #[test]
    fn test_no_double_encrypt() {
        let key = test_key();
        let plaintext = "secret";

        let encrypted = encrypt_value(&key, plaintext).unwrap();
        let double_encrypted = encrypt_value(&key, &encrypted).unwrap();

        // Should be the same (no double encryption)
        assert_eq!(encrypted, double_encrypted);
    }

    #[test]
    fn test_different_encryptions_differ() {
        let key = test_key();
        let plaintext = "same-data";

        let encrypted1 = encrypt_value(&key, plaintext).unwrap();
        let encrypted2 = encrypt_value(&key, plaintext).unwrap();

        // Different random nonces should produce different ciphertext
        assert_ne!(encrypted1, encrypted2);

        // But both should decrypt to the same value
        assert_eq!(decrypt_value(&key, &encrypted1).unwrap(), plaintext);
        assert_eq!(decrypt_value(&key, &encrypted2).unwrap(), plaintext);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = test_key();
        let mut key2 = test_key();
        key2[0] = 255; // Different key

        let encrypted = encrypt_value(&key1, "secret").unwrap();
        let result = decrypt_value(&key2, &encrypted);

        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_decrypt_env_vars() {
        let key = test_key();
        let mut env_vars = HashMap::new();
        env_vars.insert("API_KEY".to_string(), "secret-api-key".to_string());
        env_vars.insert("DB_PASSWORD".to_string(), "db-pass-123".to_string());

        let encrypted = encrypt_env_vars(&key, &env_vars).unwrap();

        // All values should be encrypted
        for v in encrypted.values() {
            assert!(is_encrypted(v));
        }

        let decrypted = decrypt_env_vars(&key, &encrypted).unwrap();

        assert_eq!(decrypted.get("API_KEY").unwrap(), "secret-api-key");
        assert_eq!(decrypted.get("DB_PASSWORD").unwrap(), "db-pass-123");
    }

    #[test]
    fn test_mixed_plaintext_encrypted() {
        let key = test_key();
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "ENCRYPTED".to_string(),
            encrypt_value(&key, "secret").unwrap(),
        );
        env_vars.insert("PLAINTEXT".to_string(), "not-encrypted".to_string());

        let decrypted = decrypt_env_vars(&key, &env_vars).unwrap();

        assert_eq!(decrypted.get("ENCRYPTED").unwrap(), "secret");
        assert_eq!(decrypted.get("PLAINTEXT").unwrap(), "not-encrypted");
    }

    #[test]
    fn test_parse_key_hex() {
        let hex_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let key = parse_key(hex_key).unwrap();

        for (i, byte) in key.iter().enumerate() {
            assert_eq!(*byte, i as u8);
        }
    }

    #[test]
    fn test_parse_key_base64() {
        let key_bytes = test_key();
        let base64_key = BASE64.encode(key_bytes);
        let parsed = parse_key(&base64_key).unwrap();

        assert_eq!(parsed, key_bytes);
    }

    #[test]
    fn test_parse_key_invalid() {
        // Too short
        assert!(parse_key("abc").is_err());
        // Invalid hex
        assert!(
            parse_key("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err()
        );
    }

    #[test]
    fn test_empty_string() {
        let key = test_key();

        let encrypted = encrypt_value(&key, "").unwrap();
        let decrypted = decrypt_value(&key, &encrypted).unwrap();

        assert_eq!(decrypted, "");
    }

    #[test]
    fn test_unicode_content() {
        let key = test_key();
        let plaintext = "Hello, 世界! 🎉";

        let encrypted = encrypt_value(&key, plaintext).unwrap();
        let decrypted = decrypt_value(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_is_unversioned_encrypted() {
        assert!(is_unversioned_encrypted("<encrypted>secret</encrypted>"));
        assert!(is_unversioned_encrypted(
            "  <encrypted>secret</encrypted>  "
        ));
        assert!(!is_unversioned_encrypted(
            "<encrypted v=\"1\">secret</encrypted>"
        ));
        assert!(!is_unversioned_encrypted("plaintext"));
    }

    #[test]
    fn test_encrypt_content_tags() {
        let key = test_key();
        let content = "Hello, here is my key: <encrypted>sk-12345</encrypted> and more text.";

        let encrypted = encrypt_content_tags(&key, content).unwrap();

        // Should have versioned tag now
        assert!(encrypted.contains("<encrypted v=\"1\">"));
        assert!(encrypted.contains("</encrypted>"));
        assert!(!encrypted.contains("<encrypted>sk-12345</encrypted>"));
        assert!(encrypted.starts_with("Hello, here is my key: "));
        assert!(encrypted.ends_with(" and more text."));
    }

    #[test]
    fn test_decrypt_content_tags() {
        let key = test_key();
        let content = "Hello, here is my key: <encrypted>sk-12345</encrypted> and more text.";

        // First encrypt
        let encrypted = encrypt_content_tags(&key, content).unwrap();

        // Then decrypt
        let decrypted = decrypt_content_tags(&key, &encrypted).unwrap();

        // Should be back to unversioned format
        assert_eq!(decrypted, content);
    }

    #[test]
    fn test_encrypt_decrypt_multiple_tags() {
        let key = test_key();
        let content = r#"
API keys:
- OpenAI: <encrypted>sk-openai-key</encrypted>
- Anthropic: <encrypted>sk-ant-key</encrypted>

Use them wisely.
"#;

        let encrypted = encrypt_content_tags(&key, content).unwrap();

        // Both should be encrypted
        assert!(!encrypted.contains("<encrypted>sk-openai-key</encrypted>"));
        assert!(!encrypted.contains("<encrypted>sk-ant-key</encrypted>"));

        // Count versioned tags
        let count = encrypted.matches("<encrypted v=\"1\">").count();
        assert_eq!(count, 2);

        // Decrypt should restore original
        let decrypted = decrypt_content_tags(&key, &encrypted).unwrap();
        assert_eq!(decrypted, content);
    }

    #[test]
    fn test_already_encrypted_passthrough() {
        let key = test_key();
        let content = "Already encrypted: <encrypted v=\"1\">abc123</encrypted>";

        // Encrypting again should not double-encrypt
        let result = encrypt_content_tags(&key, content).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_has_encrypted_tags() {
        // Unversioned tags
        assert!(has_encrypted_tags(
            "text <encrypted>secret</encrypted> more"
        ));
        assert!(has_encrypted_tags("<encrypted>secret</encrypted>"));

        // Versioned tags
        assert!(has_encrypted_tags(
            "text <encrypted v=\"1\">ciphertext</encrypted> more"
        ));
        assert!(has_encrypted_tags(
            "<encrypted v=\"1\">ciphertext</encrypted>"
        ));

        // No tags
        assert!(!has_encrypted_tags("plain text without any tags"));
        assert!(!has_encrypted_tags(""));
        assert!(!has_encrypted_tags("encrypted but not in tags"));
    }

    #[test]
    fn test_strip_encrypted_tags_unversioned() {
        let content = "API key: <encrypted>sk-12345</encrypted> is here.";
        let stripped = strip_encrypted_tags(content);
        assert_eq!(stripped, "API key: sk-12345 is here.");
    }

    #[test]
    fn test_strip_encrypted_tags_versioned() {
        let content = "API key: <encrypted v=\"1\">BASE64CIPHER</encrypted> is here.";
        let stripped = strip_encrypted_tags(content);
        assert_eq!(stripped, "API key: BASE64CIPHER is here.");
    }

    #[test]
    fn test_strip_encrypted_tags_multiple() {
        let content = r#"
Keys:
- OpenAI: <encrypted>sk-openai</encrypted>
- Anthropic: <encrypted v="1">sk-ant-encrypted</encrypted>
- Plain: not-encrypted
"#;
        let stripped = strip_encrypted_tags(content);
        assert_eq!(
            stripped,
            r#"
Keys:
- OpenAI: sk-openai
- Anthropic: sk-ant-encrypted
- Plain: not-encrypted
"#
        );
    }

    #[test]
    fn test_strip_encrypted_tags_no_tags() {
        let content = "Plain content without any encrypted tags.";
        let stripped = strip_encrypted_tags(content);
        assert_eq!(stripped, content);
    }

    #[test]
    fn test_strip_encrypted_tags_empty() {
        assert_eq!(strip_encrypted_tags(""), "");
    }

    #[test]
    fn test_strip_encrypted_tags_preserves_structure() {
        let content = r#"---
name: my-skill
---

# My Skill

Use this key: <encrypted>secret-key-value</encrypted>

```bash
export API_KEY=<encrypted>another-secret</encrypted>
```
"#;
        let stripped = strip_encrypted_tags(content);
        assert_eq!(
            stripped,
            r#"---
name: my-skill
---

# My Skill

Use this key: secret-key-value

```bash
export API_KEY=another-secret
```
"#
        );
    }

    #[test]
    fn test_full_encryption_flow_with_strip() {
        // This tests the complete flow:
        // 1. User input with <encrypted>plaintext</encrypted>
        // 2. Encrypt for storage -> <encrypted v="1">ciphertext</encrypted>
        // 3. Decrypt for display -> <encrypted>plaintext</encrypted>
        // 4. Strip for deployment -> plaintext

        let key = test_key();
        let user_input = "Key: <encrypted>my-secret-api-key</encrypted>";

        // Step 1->2: Encrypt for storage
        let stored = encrypt_content_tags(&key, user_input).unwrap();
        assert!(stored.contains("<encrypted v=\"1\">"));
        assert!(!stored.contains("<encrypted>my-secret-api-key</encrypted>"));

        // Step 2->3: Decrypt for display
        let displayed = decrypt_content_tags(&key, &stored).unwrap();
        assert_eq!(displayed, user_input);

        // Step 3->4: Strip for deployment
        let deployed = strip_encrypted_tags(&displayed);
        assert_eq!(deployed, "Key: my-secret-api-key");
    }
}
