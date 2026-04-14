//! FIDO Agent Proxy — intercepts SSH agent protocol sign requests for
//! FIDO/SK keys and relays them to the Sandboxed.sh backend for mobile
//! app approval.
//!
//! Usage:
//!   SANDBOXED_SH_API_URL=http://localhost:3000 \
//!   SANDBOXED_SH_API_TOKEN=... \
//!   fido_agent_proxy [--socket /run/sandboxed-sh/fido-agent.sock]
//!
//! The proxy speaks the SSH agent protocol (RFC draft-miller-ssh-agent).
//! For FIDO/SK key sign requests, it pauses the protocol exchange, sends
//! an approval request to the backend, and waits for a response.  For all
//! other operations, it forwards to the upstream agent (SSH_AUTH_SOCK_UPSTREAM).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

// SSH agent protocol constants
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_FAILURE: u8 = 5;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;

// FIDO/SK key type prefixes
const SK_KEY_TYPES: &[&str] = &[
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "sk-ssh-ed25519-cert-v01@openssh.com",
    "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
];

fn main() -> io::Result<()> {
    let socket_path = std::env::args()
        .skip_while(|a| a != "--socket")
        .nth(1)
        .unwrap_or_else(|| "/run/sandboxed-sh/fido-agent.sock".to_string());

    let api_url = std::env::var("SANDBOXED_SH_API_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_token = std::env::var("SANDBOXED_SH_API_TOKEN").ok();

    // Remove stale socket
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = PathBuf::from(&socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    // Make socket world-accessible (containers run as root)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o777))?;
    }

    eprintln!("[fido-agent-proxy] Listening on {}", socket_path);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let api_url = api_url.clone();
                let api_token = api_token.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(stream, &api_url, api_token.as_deref()) {
                        eprintln!("[fido-agent-proxy] Client error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("[fido-agent-proxy] Accept error: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_client(mut client: UnixStream, api_url: &str, api_token: Option<&str>) -> io::Result<()> {
    // Connect to upstream agent if available
    let upstream_sock = std::env::var("SSH_AUTH_SOCK_UPSTREAM").ok();
    let mut upstream: Option<UnixStream> = upstream_sock
        .as_deref()
        .and_then(|path| UnixStream::connect(path).ok());

    loop {
        // Read message: 4-byte big-endian length + payload
        let mut len_buf = [0u8; 4];
        if client.read_exact(&mut len_buf).is_err() {
            return Ok(()); // Client disconnected
        }
        let msg_len = u32::from_be_bytes(len_buf) as usize;
        if msg_len == 0 || msg_len > 256 * 1024 {
            return Ok(());
        }

        let mut msg = vec![0u8; msg_len];
        client.read_exact(&mut msg)?;

        let msg_type = msg[0];

        match msg_type {
            SSH_AGENTC_SIGN_REQUEST => {
                // Parse sign request to check if it's a FIDO key
                if let Some(key_type) = parse_sign_request_key_type(&msg[1..]) {
                    if SK_KEY_TYPES.iter().any(|sk| *sk == key_type) {
                        // FIDO key — relay to backend for approval
                        let fingerprint = compute_key_fingerprint(&msg[1..]);
                        eprintln!(
                            "[fido-agent-proxy] FIDO sign request: type={}, fingerprint={}",
                            key_type, fingerprint
                        );

                        match request_approval(api_url, api_token, &key_type, &fingerprint) {
                            Ok(true) => {
                                // Approved — forward to upstream agent
                                if let Some(ref mut up) = upstream {
                                    up.write_all(&len_buf)?;
                                    up.write_all(&msg)?;
                                    let response = read_agent_message(up)?;
                                    client.write_all(&response)?;
                                } else {
                                    // No upstream agent — can't actually sign
                                    send_failure(&mut client)?;
                                }
                            }
                            Ok(false) => {
                                eprintln!("[fido-agent-proxy] Signing denied");
                                send_failure(&mut client)?;
                            }
                            Err(e) => {
                                eprintln!("[fido-agent-proxy] Approval request failed: {}", e);
                                send_failure(&mut client)?;
                            }
                        }
                        continue;
                    }
                }
                // Non-FIDO key — forward to upstream
                forward_or_fail(&mut client, upstream.as_mut(), &len_buf, &msg)?;
            }
            SSH_AGENTC_REQUEST_IDENTITIES => {
                // Identity listing — forward to upstream to get real key list
                if let Some(ref mut up) = upstream {
                    up.write_all(&len_buf)?;
                    up.write_all(&msg)?;
                    let response = read_agent_message(up)?;
                    client.write_all(&response)?;
                } else {
                    // No upstream — return empty identity list
                    let reply = [
                        0,
                        0,
                        0,
                        5, // length = 5
                        SSH_AGENT_IDENTITIES_ANSWER,
                        0,
                        0,
                        0,
                        0, // nkeys = 0
                    ];
                    client.write_all(&reply)?;
                }
            }
            _ => {
                // Unknown message — forward to upstream or fail
                forward_or_fail(&mut client, upstream.as_mut(), &len_buf, &msg)?;
            }
        }
    }
}

/// Forward a message to the upstream agent, or send failure if no upstream.
fn forward_or_fail(
    client: &mut UnixStream,
    upstream: Option<&mut UnixStream>,
    len_buf: &[u8; 4],
    msg: &[u8],
) -> io::Result<()> {
    if let Some(up) = upstream {
        up.write_all(len_buf)?;
        up.write_all(msg)?;
        let response = read_agent_message(up)?;
        client.write_all(&response)?;
    } else {
        send_failure(client)?;
    }
    Ok(())
}

/// Read a full SSH agent message (length-prefixed).
fn read_agent_message(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    let mut msg = vec![0u8; msg_len];
    stream.read_exact(&mut msg)?;

    let mut result = Vec::with_capacity(4 + msg_len);
    result.extend_from_slice(&len_buf);
    result.extend_from_slice(&msg);
    Ok(result)
}

/// Send SSH_AGENT_FAILURE response.
fn send_failure(client: &mut UnixStream) -> io::Result<()> {
    let reply = [0, 0, 0, 1, SSH_AGENT_FAILURE];
    client.write_all(&reply)
}

/// Parse the key type from an SSH_AGENTC_SIGN_REQUEST message body.
/// Wire format: string key_blob, string data, uint32 flags
/// key_blob contains: string key_type, ...
fn parse_sign_request_key_type(data: &[u8]) -> Option<String> {
    // First field is the key blob (string: 4-byte length + data)
    if data.len() < 4 {
        return None;
    }
    let blob_len = u32::from_be_bytes(data[0..4].try_into().ok()?) as usize;
    if data.len() < 4 + blob_len {
        return None;
    }
    let blob = &data[4..4 + blob_len];

    // Inside the blob, first field is the key type string
    if blob.len() < 4 {
        return None;
    }
    let type_len = u32::from_be_bytes(blob[0..4].try_into().ok()?) as usize;
    if blob.len() < 4 + type_len {
        return None;
    }
    String::from_utf8(blob[4..4 + type_len].to_vec()).ok()
}

/// Compute the SHA256 fingerprint of the key blob (matches `ssh-keygen -l` output format).
fn compute_key_fingerprint(data: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    if data.len() < 4 {
        return "unknown".to_string();
    }
    let blob_len = u32::from_be_bytes(data[0..4].try_into().unwrap_or([0; 4])) as usize;
    if data.len() < 4 + blob_len {
        return "unknown".to_string();
    }
    let blob = &data[4..4 + blob_len];

    let hash = Sha256::digest(blob);
    format!("SHA256:{}", STANDARD_NO_PAD.encode(hash))
}

/// Call the backend to request FIDO signing approval.
/// Blocks until the iOS app responds or timeout (60s).
fn request_approval(
    api_url: &str,
    api_token: Option<&str>,
    key_type: &str,
    key_fingerprint: &str,
) -> Result<bool, String> {
    let url = format!("{}/api/fido/request", api_url);

    let mut body = HashMap::new();
    body.insert("key_type", key_type.to_string());
    body.insert("key_fingerprint", key_fingerprint.to_string());
    body.insert("origin", "ssh".to_string());

    // Use a simple blocking HTTP request (we're in a thread, not async)
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(65)) // slightly longer than server timeout
        .build();

    let mut req = client.post(&url).set("Content-Type", "application/json");

    if let Some(token) = api_token {
        req = req.set("Authorization", &format!("Bearer {}", token));
    }

    let resp = req
        .send_json(ureq::json!({
            "key_type": key_type,
            "key_fingerprint": key_fingerprint,
            "origin": "ssh"
        }))
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let body: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("JSON parse failed: {}", e))?;

    Ok(body["approved"].as_bool().unwrap_or(false))
}
