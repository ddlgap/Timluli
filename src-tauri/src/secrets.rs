//! Encrypted at-rest storage for translation provider API keys.
//!
//! Keys live in `secrets.json` next to `settings.json` (in %APPDATA%, which is
//! user-writable unlike the read-only install dir). Each value is a DPAPI blob
//! (tied to the Windows user account) wrapped in base64, so settings.json stays
//! shareable and the raw keys never touch disk in plaintext.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::AppHandle;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredSecrets {
    #[serde(default)]
    pub groq: Option<String>,
    #[serde(default)]
    pub cerebras: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub groq_set: bool,
    pub cerebras_set: bool,
}

fn secrets_path(app: &AppHandle) -> PathBuf {
    crate::settings::settings_dir(app).join("secrets.json")
}

pub fn load_secrets(app: &AppHandle) -> StoredSecrets {
    match std::fs::read_to_string(secrets_path(app)) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => StoredSecrets::default(),
    }
}

fn save_secrets(app: &AppHandle, secrets: &StoredSecrets) -> Result<(), String> {
    let dir = crate::settings::settings_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(secrets).map_err(|e| e.to_string())?;
    std::fs::write(secrets_path(app), json).map_err(|e| e.to_string())
}

fn encode_key(plaintext: &str) -> Result<String, String> {
    let blob = encrypt_bytes(plaintext)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

fn decode_key(b64: &str) -> Option<String> {
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    decrypt_bytes(&blob).ok()
}

/// Persists provided keys. An absent or blank field leaves the stored key
/// untouched (so the user can update one provider without re-entering the other).
pub fn save_keys(
    app: &AppHandle,
    groq: Option<String>,
    cerebras: Option<String>,
) -> Result<(), String> {
    let mut secrets = load_secrets(app);
    if let Some(g) = groq {
        let g = g.trim();
        if !g.is_empty() {
            secrets.groq = Some(encode_key(g)?);
        }
    }
    if let Some(c) = cerebras {
        let c = c.trim();
        if !c.is_empty() {
            secrets.cerebras = Some(encode_key(c)?);
        }
    }
    save_secrets(app, &secrets)
}

/// Returns the decrypted key for a provider, or `None` if unset/undecryptable.
pub fn get_key(app: &AppHandle, provider: &str) -> Option<String> {
    let secrets = load_secrets(app);
    let enc = match provider {
        "groq" => secrets.groq,
        "cerebras" => secrets.cerebras,
        _ => None,
    }?;
    decode_key(&enc)
}

pub fn status(app: &AppHandle) -> KeyStatus {
    let secrets = load_secrets(app);
    let set = |v: &Option<String>| v.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    KeyStatus {
        groq_set: set(&secrets.groq),
        cerebras_set: set(&secrets.cerebras),
    }
}

// ─── DPAPI (Windows) ─────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn encrypt_bytes(plaintext: &str) -> Result<Vec<u8>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut data = plaintext.as_bytes().to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptProtectData(&in_blob, PCWSTR::null(), None, None, None, 0, &mut out_blob)
            .map_err(|e| format!("DPAPI encrypt failed: {e}"))?;
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
        Ok(result)
    }
}

#[cfg(target_os = "windows")]
fn decrypt_bytes(bytes: &[u8]) -> Result<String, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut data = bytes.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out_blob)
            .map_err(|e| format!("DPAPI decrypt failed: {e}"))?;
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let result = String::from_utf8_lossy(slice).into_owned();
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
        Ok(result)
    }
}

// Non-Windows fallback: base64 only (no encryption). Timluli ships Windows-only;
// this keeps the crate compiling on other hosts for tooling/CI.
#[cfg(not(target_os = "windows"))]
fn encrypt_bytes(plaintext: &str) -> Result<Vec<u8>, String> {
    Ok(plaintext.as_bytes().to_vec())
}

#[cfg(not(target_os = "windows"))]
fn decrypt_bytes(bytes: &[u8]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(bytes).into_owned())
}
