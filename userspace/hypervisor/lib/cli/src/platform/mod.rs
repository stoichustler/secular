// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use blocking::Unblock;
use fidl_fuchsia_virtualization::{GuestManagerProxy, GuestMarker, GuestProxy};
use fuchsia_async as fasync;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use guest_cli_args::Vmid;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, RawFd};

/// ASCII ETX. Ignored while attached so Ctrl+C does not switch VM consoles.
const CTRL_C: u8 = 0x03;
/// ASCII EOT. Detaches the guest console and returns to secular.
const CTRL_D: u8 = 0x04;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
pub use fuchsia::*;

#[cfg(not(target_os = "fuchsia"))]
mod host;
#[cfg(not(target_os = "fuchsia"))]
pub use host::*;

// NOTE: This logic is partially duplicated in //userspace/hypervisor/bin/guest/src/services.rs,
// but that will be removed once this migration to a common codebase is finished.
// TODO(https://fxbug.dev/42067874): Remove other implementation.
pub enum Stdio {
    Stdin,
    Stdout,
    Stderr,
}

impl AsRawFd for Stdio {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Stdio::Stdin => std::io::stdin().as_raw_fd(),
            Stdio::Stdout => std::io::stdout().as_raw_fd(),
            Stdio::Stderr => std::io::stderr().as_raw_fd(),
        }
    }
}

pub struct UnbufferedStdio(Option<std::fs::File>);

impl UnbufferedStdio {
    // Unbuffer stdio by creating a File from the raw FD, but without taking ownership of the FD.
    // This allows for using interactive commands, and the custom Drop implementation prevents
    // the file from closing the stdio FD upon destruction.
    fn new(stdio: Stdio) -> Self {
        // A note about safety: Using from_raw_fd requires that the fd remains valid. As this is
        // only using the process stdio handles, validity can be assumed for the life of th
        // process.
        unsafe { Self { 0: Some(std::fs::File::from_raw_fd(stdio.as_raw_fd())) } }
    }
}

impl AsRawFd for UnbufferedStdio {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_ref().unwrap().as_raw_fd()
    }
}

impl AsFd for UnbufferedStdio {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_ref().unwrap().as_fd()
    }
}

impl Drop for UnbufferedStdio {
    fn drop(&mut self) {
        // Prevent the file from closing the fd (which it doesn't own).
        _ = self.0.take().unwrap().into_raw_fd();
    }
}

impl Write for UnbufferedStdio {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.0.as_mut().unwrap().write(buf)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.0.as_mut().unwrap().flush()
    }
}

impl Read for UnbufferedStdio {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.0.as_mut().unwrap().read(buf)
    }
}

pub struct GuestConsole {
    input: Option<fasync::Socket>,
    output: Option<fasync::Socket>,
}

impl GuestConsole {
    pub fn new(input: fidl::Socket, output: fidl::Socket) -> Result<Self> {
        Ok(GuestConsole {
            input: Some(fasync::Socket::from_socket(input)),
            output: Some(fasync::Socket::from_socket(output)),
        })
    }

    pub fn get_unblocked_stdio(stdio: Stdio) -> Unblock<UnbufferedStdio> {
        Unblock::new(UnbufferedStdio::new(stdio))
    }

    pub async fn run<R: futures::io::AsyncRead + Unpin, W: futures::io::AsyncWrite + Unpin>(
        mut self,
        host_tx: R,
        mut host_rx: W,
    ) -> Result<()> {
        let mut input = self.input.take().expect("run can only be called once");
        let output = self.output.take().expect("run can only be called once");

        let guest_input = futures::io::copy(host_tx, &mut input);
        let guest_output = futures::io::copy(output, &mut host_rx);

        futures::future::try_select(guest_input, guest_output)
            .await
            .map(|_| ())
            .map_err(|e| e.factor_first().0.into())
    }

    /// Like [`run`], but Ctrl+D detaches and Ctrl+C is ignored rather than forwarded.
    pub async fn run_until_detach<
        R: futures::io::AsyncRead + Unpin,
        W: futures::io::AsyncWrite + Unpin,
    >(
        mut self,
        host_tx: R,
        mut host_rx: W,
    ) -> Result<()> {
        let mut input = self.input.take().expect("run can only be called once");
        let output = self.output.take().expect("run can only be called once");

        let guest_input = copy_host_input_until_vm_switch(host_tx, &mut input);
        let guest_output = futures::io::copy(output, &mut host_rx);
        futures::pin_mut!(guest_input, guest_output);

        futures::future::try_select(guest_input, guest_output)
            .await
            .map(|_| ())
            .map_err(|e| e.factor_first().0.into())
    }

    pub async fn run_with_stdio(self) -> Result<()> {
        self.run(
            GuestConsole::get_unblocked_stdio(Stdio::Stdin),
            GuestConsole::get_unblocked_stdio(Stdio::Stdout),
        )
        .await
    }

    pub async fn run_with_stdio_until_detach(self) -> Result<()> {
        self.run_until_detach(
            GuestConsole::get_unblocked_stdio(Stdio::Stdin),
            GuestConsole::get_unblocked_stdio(Stdio::Stdout),
        )
        .await
    }
}

/// Returns bytes that should be forwarded to the guest, and whether Ctrl+D ended the chunk.
fn filter_vm_switch_input(input: &[u8]) -> (Vec<u8>, bool) {
    let mut forwarded = Vec::with_capacity(input.len());
    for &byte in input {
        match byte {
            CTRL_C => {}
            CTRL_D => return (forwarded, true),
            _ => forwarded.push(byte),
        }
    }
    (forwarded, false)
}

async fn copy_host_input_until_vm_switch<R, W>(mut reader: R, mut writer: W) -> std::io::Result<u64>
where
    R: futures::io::AsyncRead + Unpin,
    W: futures::io::AsyncWrite + Unpin,
{
    let mut buf = [0u8; 1024];
    let mut written = 0u64;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(written);
        }
        let (forwarded, detach) = filter_vm_switch_input(&buf[..n]);
        if !forwarded.is_empty() {
            writer.write_all(&forwarded).await?;
            written = written.saturating_add(forwarded.len() as u64);
        }
        if detach {
            return Ok(written);
        }
    }
}

#[async_trait(?Send)]
pub trait PlatformServices {
    async fn connect_to_manager(&self) -> Result<GuestManagerProxy>;

    async fn connect_to_guest(&self, vmid: Vmid) -> Result<GuestProxy> {
        let guest_manager = self.connect_to_manager().await?;
        let (guest, guest_server_end) = fidl::endpoints::create_proxy::<GuestMarker>();
        guest_manager
            .connect(vmid.get(), guest_server_end)
            .await?
            .map_err(|err| anyhow!("{:?}", err))?;
        Ok(guest)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    async fn guest_console_copies_async_stream() {
        // Wire up a mock guest console, mocking out the virtio-console on a guest.
        let (guest_console_socket, guest_console_tx) = fidl::Socket::create_stream();
        let guest_console_rx =
            guest_console_tx.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap();
        let guest_console = GuestConsole::new(guest_console_rx, guest_console_tx)
            .expect("failed to make guest console");

        // This represents a host's stdio. While stdout and stdin are unidirectional, sockets
        // are bidirectional so we can duplicate one and split it into both in and out.
        let (host_stdio, host_stdin_sock) = fidl::Socket::create_stream();
        let host_stdout_sock = host_stdin_sock.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap();
        let host_stdout = fasync::Socket::from_socket(host_stdout_sock);
        let host_stdin = fasync::Socket::from_socket(host_stdin_sock);

        // Have the "guest" write this command to its console stdout, which should be pushed to our
        // stdout.
        let test_string = "Test Command";
        guest_console_socket.write(format!("{test_string}").as_bytes()).unwrap();

        // Drop the endpoint, which looks like the guest terminating (and sends an EOF).
        drop(guest_console_socket);

        guest_console.run(host_stdin, host_stdout).await.expect("failed to complete!");

        let mut buffer = [0; 1024];
        let n = host_stdio.read(&mut buffer[..]).expect("failed to read from socket");

        assert_eq!(n, test_string.len());
        assert_eq!(String::from_utf8(buffer[..n].to_vec()).unwrap(), test_string);
    }

    #[::fuchsia::test]
    fn vm_switch_input_drops_ctrl_c_and_stops_at_ctrl_d() {
        assert_eq!(filter_vm_switch_input(b"ab\x03cd\x04ef"), (b"abcd".to_vec(), true));
        assert_eq!(filter_vm_switch_input(b"hello"), (b"hello".to_vec(), false));
        assert_eq!(filter_vm_switch_input(b"\x03\x03"), (Vec::new(), false));
        assert_eq!(filter_vm_switch_input(b"\x04"), (Vec::new(), true));
    }

    #[::fuchsia::test]
    async fn guest_console_ctrl_d_detaches_and_ignores_ctrl_c() {
        let (guest_console_socket, guest_console_tx) = fidl::Socket::create_stream();
        let guest_console_rx =
            guest_console_tx.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap();
        let guest_console = GuestConsole::new(guest_console_rx, guest_console_tx)
            .expect("failed to make guest console");

        let (host_stdio, host_stdin_sock) = fidl::Socket::create_stream();
        let host_stdout_sock = host_stdin_sock.duplicate_handle(fidl::Rights::SAME_RIGHTS).unwrap();
        let host_stdout = fasync::Socket::from_socket(host_stdout_sock);
        let host_stdin = fasync::Socket::from_socket(host_stdin_sock);

        host_stdio.write(b"hi\x03!\x04ignored").unwrap();

        guest_console
            .run_until_detach(host_stdin, host_stdout)
            .await
            .expect("failed to complete!");

        let mut buffer = [0; 1024];
        let n = guest_console_socket.read(&mut buffer[..]).expect("failed to read guest input");
        assert_eq!(&buffer[..n], b"hi!");
    }
}
