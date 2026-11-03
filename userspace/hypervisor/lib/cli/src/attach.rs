// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::platform::{GuestConsole, PlatformServices, Stdio};
use anyhow::{Error, anyhow};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_virtualization::{GuestMarker, GuestProxy, GuestStatus};
use fuchsia_async as fasync;
use futures::future::Either;
use guest_cli_args as arguments;
use std::fmt;
use std::future::Future;
use std::io::{self, Write};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AttachResult {
    Attached,
    NotRunning,
    AttachFailure,
}

impl fmt::Display for AttachResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachResult::Attached => {
                write!(f, "Disconnected from guest after a successful attach")
            }
            AttachResult::NotRunning => write!(f, "Can't attach to a non-running guest"),
            AttachResult::AttachFailure => write!(f, "Failed to attach to guest"),
        }
    }
}

pub async fn handle_attach<P: PlatformServices>(
    services: &P,
    args: &arguments::attach_args::AttachArgs,
) -> Result<AttachResult, Error> {
    let manager = services.connect_to_manager().await?;
    let status = manager
        .get_info(args.vmid.get())
        .await?
        .guest_status
        .expect("guest status should always be set");
    if status != GuestStatus::Starting && status != GuestStatus::Running {
        return Ok(AttachResult::NotRunning);
    }

    let (guest_endpoint, guest_server_end) = create_proxy::<GuestMarker>();
    manager
        .connect(args.vmid.get(), guest_server_end)
        .await
        .map_err(|err| anyhow!("failed to get a connect response: {}", err))?
        .map_err(|err| anyhow!("connect failed with: {:?}", err))?;

    Ok(match attach(guest_endpoint, args.serial, args.vmid.get()).await {
        Ok(()) => AttachResult::Attached,
        Err(_) => AttachResult::AttachFailure,
    })
}

fn vmid_banner(vmid: u32) -> String {
    format!("==== VMID {vmid:2} ====\n")
}

fn secular_banner() -> &'static str {
    "\n==== SECULAR ====\n"
}

fn print_console_banner(banner: &str) {
    print!("{banner}");
    let _ = io::stdout().flush();
}

async fn with_vm_console_session<T>(
    vmid: u32,
    session: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    // FEATURE_RAW delivers Ctrl+C as a byte instead of a PTY interrupt, so zxsh
    // will not kill this process. Restored when the guard drops after the banner.
    let _raw_pty = enable_raw_pty_for_vm_switch();
    print_console_banner(&vmid_banner(vmid));
    let result = session.await;
    print_console_banner(secular_banner());
    result
}

#[cfg(target_os = "fuchsia")]
fn enable_raw_pty_for_vm_switch() -> RawPtyGuard {
    RawPtyGuard::enter()
}

#[cfg(not(target_os = "fuchsia"))]
fn enable_raw_pty_for_vm_switch() {}

#[cfg(target_os = "fuchsia")]
struct RawPtyGuard {
    proxy: Option<fidl_fuchsia_hardware_pty::DeviceSynchronousProxy>,
}

#[cfg(target_os = "fuchsia")]
impl RawPtyGuard {
    fn enter() -> Self {
        let Ok(channel) = fdio::clone_channel(&io::stdin()) else {
            return Self { proxy: None };
        };
        let proxy = fidl_fuchsia_hardware_pty::DeviceSynchronousProxy::new(channel);
        let _ = proxy.clr_set_feature(
            0,
            fidl_fuchsia_hardware_pty::FEATURE_RAW,
            zx::MonotonicInstant::INFINITE,
        );
        Self { proxy: Some(proxy) }
    }
}

#[cfg(target_os = "fuchsia")]
impl Drop for RawPtyGuard {
    fn drop(&mut self) {
        if let Some(proxy) = self.proxy.take() {
            let _ = proxy.clr_set_feature(
                fidl_fuchsia_hardware_pty::FEATURE_RAW,
                0,
                zx::MonotonicInstant::INFINITE,
            );
        }
    }
}

pub async fn attach(guest: GuestProxy, serial_only: bool, vmid: u32) -> Result<(), Error> {
    if serial_only {
        attach_serial(guest, vmid).await
    } else {
        attach_console_and_serial(guest, vmid).await
    }
}

// Attach to a running guest, using the guest's virtio-console and serial output for stdout, and
// the guest's virtio-console for stdin.
async fn attach_console_and_serial(guest: GuestProxy, vmid: u32) -> Result<(), Error> {
    // Tie serial output to stdout.
    let guest_serial_response = guest.get_serial().await?;
    let guest_serial = fasync::Socket::from_socket(guest_serial_response);
    let serial_output = async {
        futures::io::copy(guest_serial, &mut GuestConsole::get_unblocked_stdio(Stdio::Stdout))
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
    };

    // Host doesn't currently support duplicating Fuchsia handles, so just call get console twice
    // and let the VMM duplicate the socket for reading and writing.
    let console_input = guest.get_console().await?.map_err(|err| anyhow!(format!("{:?}", err)))?;
    let console_output = guest.get_console().await?.map_err(|err| anyhow!(format!("{:?}", err)))?;
    let guest_console = GuestConsole::new(console_input, console_output)?;

    with_vm_console_session(vmid, async move {
        let console = guest_console.run_with_stdio_until_detach();
        futures::pin_mut!(serial_output, console);
        match futures::future::select(serial_output, console).await {
            Either::Left((serial_res, _)) => serial_res,
            Either::Right((console_res, _)) => console_res,
        }
    })
    .await
}

// Attach to a running guest using serial for stdout and stdin.
async fn attach_serial(guest: GuestProxy, vmid: u32) -> Result<(), Error> {
    // Host doesn't currently support duplicating Fuchsia handles, so just call get serial twice
    // and let the VMM duplicate the socket for reading and writing.
    let serial_input = guest.get_serial().await?;
    let serial_output = guest.get_serial().await?;

    let guest_console = GuestConsole::new(serial_input, serial_output)?;
    with_vm_console_session(vmid, guest_console.run_with_stdio_until_detach()).await
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::Socket;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_virtualization::GuestError;
    use futures::StreamExt;
    use futures::future::join;

    #[fuchsia::test(allow_stalls = false)]
    async fn launch_invalid_console_returns_error() {
        let (guest_proxy, mut guest_stream) = create_proxy_and_stream::<GuestMarker>();
        let (serial_launch_sock, _serial_server_sock) = Socket::create_stream();

        let server = async move {
            let serial_responder = guest_stream
                .next()
                .await
                .expect("Failed to read from stream")
                .expect("Failed to parse request")
                .into_get_serial()
                .expect("Unexpected call to Guest Proxy");
            serial_responder.send(serial_launch_sock).expect("Failed to send response to proxy");

            let console_responder = guest_stream
                .next()
                .await
                .expect("Failed to read from stream")
                .expect("Failed to parse request")
                .into_get_console()
                .expect("Unexpected call to Guest Proxy");
            console_responder
                .send(Err(GuestError::DeviceNotPresent))
                .expect("Failed to send response to proxy");
        };

        let client = attach(guest_proxy, false, 0);
        let (_, client_res) = join(server, client).await;
        assert!(client_res.is_err());
    }

    #[fuchsia::test]
    fn console_switch_banners_match_requested_format() {
        assert_eq!(vmid_banner(0), "==== VMID  0 ====\n");
        assert_eq!(vmid_banner(9), "==== VMID  9 ====\n");
        assert_eq!(vmid_banner(10), "==== VMID 10 ====\n");
        assert_eq!(secular_banner(), "\n==== SECULAR ====\n");
    }
}
