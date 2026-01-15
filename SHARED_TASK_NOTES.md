# Encrypted Env Vars - Implementation Notes

## Goal
Encryption-at-rest for workspace template env vars using `PRIVATE_KEY` in `.env`.

## Current Status
- [x] Design complete
- [x] `src/library/env_crypto.rs` implemented with full test coverage (13 tests)
- [x] Integration into `get_workspace_template()` (decrypt on load)
- [x] Integration into `save_workspace_template()` (encrypt on save)
- [x] `.env.example` updated with PRIVATE_KEY documentation
- [x] All 43 tests passing

## What Was Implemented

### Encryption Format
```
<encrypted v="1">BASE64(nonce||ciphertext)</encrypted>
```
- Version in wrapper allows future format changes
- 12-byte random nonce prepended to ciphertext
- AES-256-GCM AEAD encryption

### Key Functions (`src/library/env_crypto.rs`)
- `is_encrypted(value)` - Check for wrapper format
- `encrypt_value(key, plaintext)` - Returns wrapped encrypted string
- `decrypt_value(key, value)` - Passthrough if plaintext, decrypt if wrapped
- `encrypt_env_vars()` / `decrypt_env_vars()` - Batch operations for HashMap
- `load_private_key_from_env()` - Load from PRIVATE_KEY (hex or base64)
- `load_or_create_private_key(path)` - Auto-generate if missing (async)
- `generate_private_key()` - Generate 32 random bytes

### Integration Points
- `LibraryStore::get_workspace_template()` - Decrypts after JSON parse
- `LibraryStore::save_workspace_template()` - Encrypts before JSON serialize

### Backward Compatibility
- Plaintext values pass through unchanged on decrypt
- Warning logged if encrypted values found but no key configured
- Warning logged if saving plaintext when no key configured

## Remaining Work

### 1. Auto-generate key on startup (Priority)
Currently `load_or_create_private_key()` exists but isn't called at startup.
Need to integrate into application initialization to auto-generate the key.

Look at `src/main.rs` or startup code to call:
```rust
let env_path = std::env::current_dir()?.join(".env");
env_crypto::load_or_create_private_key(&env_path).await?;
```

### 2. Key rotation command
Implement a CLI command or API endpoint to:
1. Load old key from env
2. Generate new key
3. Re-encrypt all template env vars with new key
4. Update .env with new key

### 3. Integration tests
Add tests that actually save/load templates through `LibraryStore` with encryption.
Current tests only cover the crypto primitives.

### 4. Dashboard UI verification
Verify the dashboard displays plaintext env vars correctly (no UX regression).
API endpoints should return decrypted values transparently.

## Files Changed
- `src/library/env_crypto.rs` (NEW) - Crypto utilities
- `src/library/mod.rs` - Module declaration + template load/save integration
- `Cargo.toml` - Added `hex = "0.4"` dependency
- `.env.example` - Documented PRIVATE_KEY

## Testing
```bash
cargo test --lib env_crypto  # 13 crypto tests
cargo test --lib             # All 43 tests
```

## Notes
- Key format: 64 hex chars OR base64-encoded 32 bytes
- No double-encryption (already-encrypted values pass through)
- Different encryptions produce different ciphertext (random nonce)
- Existing `src/secrets/crypto.rs` uses passphrase-based PBKDF2 (different use case)
