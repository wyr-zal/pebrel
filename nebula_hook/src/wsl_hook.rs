//! Narrow WSL relay. Credentials and provider content travel only on stdin/pipe.
use std::io::{self, Read as _, Write as _};

pub(super) fn valid_wsl_hook(wire: &[u8]) -> bool {
    if wire.len() > 64 * 1024 + 128 {
        return false;
    }
    let Some(end) = wire.iter().position(|byte| *byte == b'\n') else { return false };
    let Some(token) = wire[..end].strip_prefix(b"pebrel-wsl-hook/1 token=") else {
        return false;
    };
    if token.len() != 32 || !token.iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    let Some(header_end) = wire[end + 1..].iter().position(|byte| *byte == b'\n') else {
        return false;
    };
    let Ok(header) = std::str::from_utf8(&wire[end + 1..end + 1 + header_end]) else {
        return false;
    };
    let mut fields = header.split_whitespace();
    fields.next() == Some("nebula-hook/1")
        && matches!(fields.next(), Some("source=claude" | "source=codex"))
}

pub(super) fn forward(wire: &[u8]) -> io::Result<()> {
    let pipe = std::env::var("PEBREL_WSL_HOOK_PIPE").map_err(|_| io::ErrorKind::NotFound)?;
    if !pipe.starts_with(r"\\.\pipe\pebrel-wsl-hook-") {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut stream = None;
    for _ in 0..20 {
        match std::fs::OpenOptions::new().read(true).write(true).open(&pipe) {
            Ok(opened) => {
                stream = Some(opened);
                break;
            },
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    let mut stream = stream.ok_or(io::ErrorKind::NotConnected)?;
    stream.write_all(&(wire.len() as u32).to_le_bytes())?;
    stream.write_all(wire)?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    stream.write_all(&[2])?;
    if ack != [1] {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
