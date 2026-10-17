//! Cancellable, bounded Windows pipe I/O for one PTY's WSL Hook channel.
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::sync::Arc;
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_REJECT_REMOTE_CLIENTS,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

#[derive(Clone)]
pub(super) struct Signal(Arc<OwnedHandle>);

impl Signal {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: manual-reset event, no borrowed security attributes or name.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this newly created event has exactly one owning handle.
        Ok(Self(Arc::new(unsafe { OwnedHandle::from_raw_handle(handle) })))
    }

    pub(super) fn set(&self) {
        // SAFETY: the shared owner keeps the event alive.
        unsafe {
            SetEvent(self.0.as_raw_handle());
        }
    }

    pub(super) fn is_set(&self) -> bool {
        // SAFETY: nonblocking query on the live event.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

pub(super) struct Endpoint {
    pipe: OwnedHandle,
    ready: Signal,
}

pub(super) fn peer_disconnected(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotConnected
        || error.raw_os_error().is_some_and(|code| {
            matches!(code as u32, ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED)
        })
}

impl Endpoint {
    pub(super) fn bind(name: &str) -> io::Result<Self> {
        if !name.starts_with(r"\\.\pipe\pebrel-wsl-hook-") || name.contains('\0') {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: valid NUL-terminated name; default token DACL; remote clients
        // refused. FIRST_PIPE_INSTANCE fails closed if another server owns it.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                65536,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sole owner of the new pipe handle.
        let pipe = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Self { pipe, ready: Signal::new()? })
    }

    fn operation(
        &self,
        stop: &Signal,
        deadline: Option<Instant>,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<u32> {
        if stop.is_set() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // One outstanding operation per endpoint. This event must be reset
        // before reuse; OVERLAPPED and its buffer stay alive until completion.
        unsafe {
            windows_sys::Win32::System::Threading::ResetEvent(self.ready.0.as_raw_handle());
        }
        // SAFETY: zero is the documented initial state for OVERLAPPED.
        let mut operation: OVERLAPPED = unsafe { std::mem::zeroed() };
        operation.hEvent = self.ready.0.as_raw_handle();
        let handle = self.pipe.as_raw_handle();
        if start(handle, &mut operation) == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_PIPE_CONNECTED {
                return Ok(0);
            }
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let timeout = deadline.map_or(INFINITE, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u128::from(u32::MAX - 1)) as u32
            });
            let handles = [stop.0.as_raw_handle(), self.ready.0.as_raw_handle()];
            // SAFETY: both handles and the operation outlive this wait.
            if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, timeout) }
                != WAIT_OBJECT_0 + 1
            {
                let mut ignored = 0;
                // SAFETY: cancel and reap before dropping OVERLAPPED/buffer.
                unsafe {
                    CancelIoEx(handle, &operation);
                    GetOverlappedResult(handle, &operation, &mut ignored, 1);
                }
                return Err(if stop.is_set() {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::TimedOut
                }
                .into());
            }
        }
        let mut transferred = 0;
        // SAFETY: the operation has completed, with live storage throughout.
        if unsafe { GetOverlappedResult(handle, &operation, &mut transferred, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred)
    }

    pub(super) fn connect(&self, stop: &Signal) -> io::Result<()> {
        self.operation(stop, None, |pipe, operation| unsafe { ConnectNamedPipe(pipe, operation) })?;
        let mut pid = 0;
        // A Linux identity is verified separately; this Windows client PID
        // must never be attached as the provider PID to the domain event.
        if unsafe { GetNamedPipeClientProcessId(self.pipe.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::ErrorKind::NotConnected.into());
        }
        Ok(())
    }

    fn read_exact(&self, mut bytes: &mut [u8], stop: &Signal, deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            let read = self.operation(stop, Some(deadline), |pipe, operation| unsafe {
                ReadFile(
                    pipe,
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    std::ptr::null_mut(),
                    operation,
                )
            })? as usize;
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            bytes = &mut bytes[read..];
        }
        Ok(())
    }

    pub(super) fn receive(
        &self,
        stop: &Signal,
        deadline: Instant,
        max: usize,
    ) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        self.read_exact(&mut length, stop, deadline)?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > max {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut wire = vec![0; length];
        self.read_exact(&mut wire, stop, deadline)?;
        Ok(wire)
    }

    pub(super) fn acknowledge(
        &self,
        accepted: bool,
        stop: &Signal,
        deadline: Instant,
    ) -> io::Result<()> {
        let bytes = [u8::from(accepted)];
        self.operation(stop, Some(deadline), |pipe, operation| unsafe {
            WriteFile(pipe, bytes.as_ptr(), 1, std::ptr::null_mut(), operation)
        })?;
        // DisconnectNamedPipe discards unread buffered bytes. Wait for the
        // client's bounded receipt instead of an unbounded FlushFileBuffers.
        let mut receipt = [0];
        self.read_exact(&mut receipt, stop, deadline)?;
        if receipt != [2] {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }

    pub(super) fn disconnect(&self) {
        // SAFETY: all outstanding operations have been reaped.
        unsafe {
            DisconnectNamedPipe(self.pipe.as_raw_handle());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    #[test]
    fn wsl_hook_pipe_preserves_partial_writes_and_acknowledgement() {
        let name = format!(
            r"\\.\pipe\pebrel-wsl-hook-test-{}-{}",
            std::process::id(),
            crate::ai_hook::remote::new_token().unwrap()
        );
        let endpoint = Endpoint::bind(&name).unwrap();
        let stop = Signal::new().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = std::fs::OpenOptions::new().read(true).write(true).open(name).unwrap();
            for byte in 24000u32.to_le_bytes() {
                stream.write_all(&[byte]).unwrap();
            }
            for _ in 0..240 {
                stream.write_all(&[b'x'; 100]).unwrap();
            }
            std::thread::sleep(Duration::from_millis(30));
            let mut ack = [0];
            stream.read_exact(&mut ack).unwrap();
            stream.write_all(&[2]).unwrap();
            ack
        });
        endpoint.connect(&stop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(endpoint.receive(&stop, deadline, 65536).unwrap(), vec![b'x'; 24000]);
        endpoint.acknowledge(true, &stop, deadline).unwrap();
        endpoint.disconnect();
        assert_eq!(client.join().unwrap(), [1]);
    }

    #[test]
    fn wsl_hook_pipe_revokes_idle_accept_without_waiting_for_a_client() {
        let name = format!(
            r"\\.\pipe\pebrel-wsl-hook-test-cancel-{}",
            crate::ai_hook::remote::new_token().unwrap()
        );
        let endpoint = Endpoint::bind(&name).unwrap();
        let stop = Signal::new().unwrap();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || endpoint.connect(&worker_stop));
        stop.set();
        assert_eq!(worker.join().unwrap().unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
}
