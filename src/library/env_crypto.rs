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
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

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

/// Load the encryption key from environment, generating one if missing.
/// If a key is generated, it will be appended to the .env file at the given path.
pub async fn load_or_create_private_key(env_file_path: &Path) -> Result<[u8; KEY_LENGTH]> {
    // Try to load existing key
    if let Some(key) = load_private_key_from_env()? {
        return Ok(key);
    }

    // Generate new key
    let key = generate_private_key();
    let key_hex = hex::encode(key);

    // Append to .env file
    let env_line = format!("\n# Auto-generated encryption key for template env vars\n{}={}\n", PRIVATE_KEY_ENV, key_hex);

    // Create or append to .env file
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env_file_path)
        .await
        .context("Failed to open .env file for writing")?;

    file.write_all(env_line.as_bytes())
        .await
        .context("Failed to write PRIVATE_KEY to .env file")?;

    // Also set in current process environment
    std::env::set_var(PRIVATE_KEY_ENV, &key_hex);

    tracing::info!("Generated new PRIVATE_KEY and saved to .env");

    Ok(key)
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
        assert!(parse_key("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
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
}
