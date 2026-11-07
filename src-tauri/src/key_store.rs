//! Secure key storage in the system credential vault.
//!
//! On macOS: Keychain via the `security` CLI tool (signed by Apple, so reading
//! does not re-prompt on dev rebuilds).
//! On Windows: Windows Credential Manager.

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    pub fn read(service: &str) -> Option<String> {
        let output = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .ok()?;
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (output.status.success() && !key.is_empty()).then_some(key)
    }

    pub fn write(service: &str, key: &str) -> Result<(), String> {
        // -U replaces a key that is already saved.
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-U",
                "-s",
                service,
                "-a",
                "chatelle",
                "-w",
                key,
            ])
            .output()
            .map_err(|e| format!("Could not run security: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Could not save the key: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    pub fn delete(service: &str) -> Result<(), String> {
        let output = Command::new("security")
            .args(["delete-generic-password", "-s", service])
            .output()
            .map_err(|e| format!("Could not run security: {e}"))?;
        // 44: no key was saved, so there is nothing to remove.
        if !output.status.success() && output.status.code() != Some(44) {
            return Err(format!(
                "Could not remove the key: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub fn read(target: &str) -> Option<String> {
        let target = wide(target);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
            .ok()?;
            let size = (*credential).CredentialBlobSize as usize;
            let blob = (*credential).CredentialBlob;
            let key = if blob.is_null() || size == 0 {
                String::new()
            } else {
                let bytes = std::slice::from_raw_parts(blob, size);
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16_lossy(&units).trim().to_string()
            };
            CredFree(credential as *const _);
            (!key.is_empty()).then_some(key)
        }
    }

    pub fn write(target: &str, key: &str) -> Result<(), String> {
        let mut target = wide(target);
        let mut user = wide("chatelle");
        let mut blob: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }.map_err(|e| format!("Could not save the key: {e}"))
    }

    pub fn delete(target: &str) -> Result<(), String> {
        let target = wide(target);
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
            Err(e) => Err(format!("Could not remove the key: {e}")),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn read(_service: &str) -> Option<String> {
        None
    }

    pub fn write(_service: &str, _key: &str) -> Result<(), String> {
        Err("Saving keys is not supported on this platform".to_string())
    }

    pub fn delete(_service: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Saved names began with "handy-" before the rename to Chatelle.
const LEGACY_PREFIX: &str = "handy-";

/// The name a key was saved under before the rename, if it had one.
fn legacy_name(service: &str) -> Option<String> {
    service
        .strip_prefix("chatelle-")
        .map(|rest| format!("{LEGACY_PREFIX}{rest}"))
}

pub fn read(service: &str) -> Option<String> {
    if let Some(key) = platform::read(service) {
        return Some(key);
    }
    // A key saved before the rename: move it to the new name, once.
    let legacy = legacy_name(service)?;
    let key = platform::read(&legacy)?;
    if platform::write(service, &key).is_ok() {
        let _ = platform::delete(&legacy);
    }
    Some(key)
}

pub fn write(service: &str, key: &str) -> Result<(), String> {
    platform::write(service, key)
}

pub fn delete(service: &str) -> Result<(), String> {
    // Also remove a copy left under the old name, so it can't come back.
    if let Some(legacy) = legacy_name(service) {
        let _ = platform::delete(&legacy);
    }
    platform::delete(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_keys_know_their_old_name() {
        assert_eq!(
            legacy_name("chatelle-gemini-key").as_deref(),
            Some("handy-gemini-key")
        );
        assert_eq!(legacy_name("other"), None);
    }
}
