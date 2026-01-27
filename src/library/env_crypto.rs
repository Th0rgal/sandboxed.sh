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
/// Uses `PRIVATE_KEY_FILE` env var, or defaults to `{WORKING_DIR}/.openagent/private_key`.
fn private_key_file_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        return std::path::PathBuf::from(path);
    }
    let working_dir = std::env::var("WORKING_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/root"));
    working_dir.join(".openagent").join("private_key")
}

/// Ensure a private key is available, generating one lazily if needed.
///
/// 1. Checks `PRIVATE_KEY` env var (fast path, no I/O).
/// 2. Reads from the key file (`{WORKING_DIR}/.openagent/private_key`).
/// 3. Generates a new key, persists it to the file, and sets the env var.
pub async fn ensure_private_key() -> Result<[u8; KEY_LENGTH]> {
    // 1. Fast path: env var already set
    if let Some(key) = load_private_key_from_env()? {
        return Ok(key);
    }

    // 2. Try reading from persisted key file
    let key_file = private_key_file_path();
    if key_file.exists() {
        if let Ok(contents) = fs::read_to_string(&key_file).await {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                let key = parse_key(trimmed)
                    .context("Invalid key in private_key file")?;
                // Cache in process env for future calls
                std::env::set_var(PRIVATE_KEY_ENV, trimmed);
                tracing::info!("Loaded PRIVATE_KEY from {}", key_file.display());
                return Ok(key);
            }
        }
    }

    // 3. Generate new key and persist
    let key = generate_private_key();
    let key_hex = hex::encode(key);

    if let Some(parent) = key_file.parent() {
        fs::create_dir_all(parent).await
            .context("Failed to create directory for private_key file")?;
    }
    fs::write(&key_file, &key_hex).await
        .context("Failed to write private_key file")?;

    // Set in process env
    std::env::set_var(PRIVATE_KEY_ENV, &key_hex);

    tracing::info!("Generated new PRIVATE_KEY and saved to {}", key_file.display());
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

/// Check if a value is an unversioned encrypted tag (user input format).
pub fn is_unversioned_encrypted(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("<encrypted>")
        && trimmed.ends_with("</encrypted>")
        && !trimmed.contains(" v=\"")
}

/// Encrypt all unversioned <encrypted>value</encrypted> tags in content.
/// Transforms <encrypted>plaintext</encrypted> to <encrypted v="1">ciphertext</encrypted>.
pub fn encrypt_content_tags(key: &[u8; KEY_LENGTH], content: &str) -> Result<String> {
    let re =
        regex::Regex::new(UNVERSIONED_TAG_REGEX).map_err(|e| anyhow!("Invalid regex: {}", e))?;

    let mut result = content.to_string();
    let mut offset: i64 = 0;

    for cap in re.captures_iter(content) {
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

    Ok(result)
}

/// Decrypt all versioned <encrypted v="N">ciphertext</encrypted> tags in content.
/// Transforms <encrypted v="1">ciphertext</encrypted> to <encrypted>plaintext</encrypted>.
pub fn decrypt_content_tags(key: &[u8; KEY_LENGTH], content: &str) -> Result<String> {
    let re = regex::Regex::new(VERSIONED_TAG_REGEX).map_err(|e| anyhow!("Invalid regex: {}", e))?;

    let mut result = content.to_string();
    let mut offset: i64 = 0;

    for cap in re.captures_iter(content) {
        let full_match = cap.get(0).unwrap();
        let _version = cap.get(1).unwrap().as_str();
        let _ciphertext_b64 = cap.get(2).unwrap().as_str();

        // Reconstruct the full encrypted value for decryption
        let encrypted_value = full_match.as_str();
        let plaintext = decrypt_value(key, encrypted_value)?;

        // Format as unversioned tag for display
        let display_tag = format!("<encrypted>{}</encrypted>", plaintext);

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
    std::env::var(PRIVATE_KEY_ENV).ok().filter(|k| !k.trim().is_empty())
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
    fs::write(&key_file, key_hex.trim()).await
        .context("Failed to write private_key file")?;

    // Set in process env
    std::env::set_var(PRIVATE_KEY_ENV, key_hex.trim());

    tracing::info!("Restored PRIVATE_KEY from backup");
    Ok(())
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
}
