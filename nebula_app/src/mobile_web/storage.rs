//! Atomic configuration storage; only the small wrapping key goes in the OS keyring.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use std::{
    io::{Read, Write},
    path::Path,
};

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

pub(super) fn read(path: &Path, key: &[u8], scope: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() < 28 || bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(String::from("invalid encrypted remote configuration"));
    }
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| String::from("invalid configuration key"))?;
    let nonce: [u8; 12] = bytes[..12].try_into().expect("nonce length checked");
    cipher
        .decrypt(&Nonce::from(nonce), Payload { msg: &bytes[12..], aad: scope })
        .map_err(|_| String::from("remote configuration authentication failed"))
}

pub(super) fn write(path: &Path, key: &[u8], scope: &[u8], bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 + 28 > MAX_CONFIG_BYTES {
        return Err(String::from("remote configuration too large"));
    }
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| String::from("invalid configuration key"))?;
    let mut nonce = [0_u8; 12];
    getrandom::fill(&mut nonce)
        .map_err(|_| String::from("configuration randomness unavailable"))?;
    let encrypted = cipher
        .encrypt(&Nonce::from(nonce), Payload { msg: bytes, aad: scope })
        .map_err(|_| String::from("remote configuration encryption failed"))?;
    let parent = path.parent().ok_or_else(|| String::from("configuration directory missing"))?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(&nonce)
        .and_then(|()| temporary.write_all(&encrypted))
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_config_is_atomic_authenticated_and_exceeds_keyring_blob_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key).unwrap();
        let bytes = vec![b'x'; 16 * 1024];
        write(&path, &key, b"isolated-config", &bytes).unwrap();
        assert_eq!(read(&path, &key, b"isolated-config").unwrap(), bytes);
        assert!(read(&path, &key, b"other-config").is_err());
        assert!(write(&path, &[0], b"isolated-config", b"replacement").is_err());
        assert_eq!(read(&path, &key, b"isolated-config").unwrap(), bytes);
        let mut damaged = std::fs::read(&path).unwrap();
        damaged[15] ^= 1;
        std::fs::write(&path, damaged).unwrap();
        assert!(read(&path, &key, b"isolated-config").is_err());
    }
}
