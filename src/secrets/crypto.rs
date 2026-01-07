//! Cryptographic operations for secrets.
//!
//! Uses AES-256-GCM with PBKDF2 key derivation from a passphrase.
//! This is simpler than RSA and doesn't require key file management.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use thiserror::Error;

use super::types::EncryptedSecret;

/// Number of PBKDF2 iterations for key derivation.
/// 600,000 is recommended by OWASP for 2023+
const PBKDF2_ITERATIONS: u32 = 600_000;

/// Salt length in bytes
const SALT_LENGTH: usize = 32;

/// Nonce length in bytes (96 bits for AES-GCM)
const NONCE_LENGTH: usize = 12;

/// Key length in bytes (256 bits for AES-256)
const KEY_LENGTH: usize = 32;

/// Errors that can occur during cryptographic operations.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("Invalid base64: {0}")]
    InvalidBase64(String),

    #[error("Passphrase not available")]
    PassphraseNotAvailable,

    #[error("Key derivation failed: {0}")]
    KeyDerivationFailed(String),
}

/// Cryptographic engine for secrets encryption/decryption.
pub struct SecretsCrypto {
    /// The passphrase used for key derivation
    passphrase: Option<String>,
}

impl SecretsCrypto {
    /// Create a new crypto engine without a passphrase.
    pub fn new() -> Self {
        Self { passphrase: None }
    }

    /// Create a new crypto engine with a passphrase.
    pub fn with_passphrase(passphrase: String) -> Self {
        Self {
            passphrase: Some(passphrase),
        }
    }

    /// Set the passphrase for decryption.
    pub fn set_passphrase(&mut self, passphrase: String) {
        self.passphrase = Some(passphrase);
    }

    /// Clear the passphrase.
    pub fn clear_passphrase(&mut self) {
        self.passphrase = None;
    }

    /// Check if we have a passphrase set.
    pub fn has_passphrase(&self) -> bool {
        self.passphrase.is_some()
    }

    /// Derive an AES key from the passphrase and salt using PBKDF2.
    fn derive_key(&self, salt: &[u8]) -> Result<[u8; KEY_LENGTH], CryptoError> {
        let passphrase = self
            .passphrase
            .as_ref()
            .ok_or(CryptoError::PassphraseNotAvailable)?;

        let mut key = [0u8; KEY_LENGTH];

        // Use PBKDF2-HMAC-SHA256
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
            passphrase.as_bytes(),
            salt,
            PBKDF2_ITERATIONS,
            &mut key,
        );

        Ok(key)
    }

    /// Encrypt a plaintext value.
    pub fn encrypt(&self, plaintext: &str) -> Result<EncryptedSecret, CryptoError> {
        use aes_gcm::{
            aead::{Aead, KeyInit},
            Aes256Gcm, Nonce,
        };
        use rand::RngCore;

        // Generate random salt and nonce
        let mut salt = [0u8; SALT_LENGTH];
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

        // Derive key from passphrase
        let key = self.derive_key(&salt)?;

        // Create cipher
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

        Ok(EncryptedSecret {
            ciphertext: BASE64.encode(&ciphertext),
            nonce: BASE64.encode(&nonce_bytes),
            salt: BASE64.encode(&salt),
            metadata: None,
        })
    }

    /// Decrypt an encrypted secret.
    pub fn decrypt(&self, secret: &EncryptedSecret) -> Result<String, CryptoError> {
        use aes_gcm::{
            aead::{Aead, KeyInit},
            Aes256Gcm, Nonce,
        };

        // Decode base64
        let ciphertext = BASE64
            .decode(&secret.ciphertext)
            .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;

        let nonce_bytes = BASE64
            .decode(&secret.nonce)
            .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;

        let salt = BASE64
            .decode(&secret.salt)
            .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;

        // Derive key from passphrase
        let key = self.derive_key(&salt)?;

        // Create cipher
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Decrypt
        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| CryptoError::DecryptionFailed("Invalid passphrase or corrupted data".to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|e| CryptoError::DecryptionFailed(format!("Invalid UTF-8: {}", e)))
    }

    /// Verify that the passphrase is correct by attempting to decrypt a secret.
    pub fn verify_passphrase(&self, secret: &EncryptedSecret) -> bool {
        self.decrypt(secret).is_ok()
    }
}

impl Default for SecretsCrypto {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let crypto = SecretsCrypto::with_passphrase("test-passphrase-123".to_string());

        let plaintext = "my-secret-api-key-12345";
        let encrypted = crypto.encrypt(plaintext).unwrap();

        // Verify the encrypted data looks reasonable
        assert!(!encrypted.ciphertext.is_empty());
        assert!(!encrypted.nonce.is_empty());
        assert!(!encrypted.salt.is_empty());

        // Decrypt and verify
        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_wrong_passphrase_fails() {
        let crypto1 = SecretsCrypto::with_passphrase("correct-passphrase".to_string());
        let crypto2 = SecretsCrypto::with_passphrase("wrong-passphrase".to_string());

        let plaintext = "secret-data";
        let encrypted = crypto1.encrypt(plaintext).unwrap();

        // Decryption with wrong passphrase should fail
        let result = crypto2.decrypt(&encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_no_passphrase_fails() {
        let crypto = SecretsCrypto::new();

        let result = crypto.encrypt("test");
        assert!(matches!(result, Err(CryptoError::PassphraseNotAvailable)));
    }

    #[test]
    fn test_different_encryptions_produce_different_ciphertext() {
        let crypto = SecretsCrypto::with_passphrase("passphrase".to_string());

        let plaintext = "same-data";
        let encrypted1 = crypto.encrypt(plaintext).unwrap();
        let encrypted2 = crypto.encrypt(plaintext).unwrap();

        // Different salt/nonce should produce different ciphertext
        assert_ne!(encrypted1.ciphertext, encrypted2.ciphertext);
        assert_ne!(encrypted1.nonce, encrypted2.nonce);
        assert_ne!(encrypted1.salt, encrypted2.salt);

        // But both should decrypt to the same value
        assert_eq!(crypto.decrypt(&encrypted1).unwrap(), plaintext);
        assert_eq!(crypto.decrypt(&encrypted2).unwrap(), plaintext);
    }
}
