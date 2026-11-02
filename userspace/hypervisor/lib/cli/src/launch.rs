// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::attach::attach;
use crate::platform::PlatformServices;
use fidl_fuchsia_virtualization::{
    GuestConfig, GuestError, GuestManagerError, GuestMarker, GuestProxy, GuestStatus,
};
use guest_cli_args as arguments;
use std::fmt;

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LaunchResult {
    LaunchCompleted,
    AttachFailed(String),
    RoutingError,
    FidlError(String),
    LaunchFailure { manager: u32, guest: Option<u32> },
}

impl fmt::Display for LaunchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchResult::LaunchCompleted => write!(f, "Successfully launched the guest"),
            LaunchResult::AttachFailed(error) => {
                write!(f, "Failed to attach to a running guest: {}", error)
            }
            LaunchResult::FidlError(error) => write!(f, "Failed FIDL call: {}", error),
            LaunchResult::LaunchFailure { manager, guest } => {
                write!(
                    f,
                    "Failed to launch guest: {:?}",
                    GuestManagerError::from_primitive(*manager).expect("expected a valid error")
                )?;
                if let Some(guest_err) = guest {
                    write!(
                        f,
                        " ({:?})",
                        GuestError::from_primitive(*guest_err).expect("expected a valid guest error")
                    )?;
                }
                Ok(())
            }
            LaunchResult::RoutingError => {
                writeln!(f, "")?;
                writeln!(f, "Unable to connect to the guest manager.")?;
                writeln!(
                    f,
                    "  Ensure guest_manager is in the image (//userspace/hypervisor:framework)"
                )?;
                writeln!(f, "")
            }
        }
    }
}

pub async fn handle_launch<P: PlatformServices>(
    services: &P,
    args: &arguments::launch_args::LaunchArgs,
) -> LaunchResult {
    let config = parse_vmm_args(args);
    let guest = launch(services, args.vmid, config).await;
    if let Err(err) = guest {
        return err;
    }

    if !args.detach {
        let attach_err = attach(guest.unwrap(), false).await.err();
        if let Ok(manager) = services.connect_to_manager().await {
            if let Ok(info) = manager.get_info(args.vmid.get()).await {
                if let Some(stop) = info.stop_error {
                    return LaunchResult::LaunchFailure {
                        manager: GuestManagerError::StartFailure.into_primitive(),
                        guest: Some(stop.into_primitive()),
                    };
                }
                if info.guest_status == Some(GuestStatus::Stopped) {
                    println!("Guest stopped (clean shutdown). Use `guest list vmid={}` for details.",
                             args.vmid);
                }
            }
        }
        if let Some(err) = attach_err {
            return LaunchResult::AttachFailed(format!("{}", err));
        }
    }

    LaunchResult::LaunchCompleted
}

fn parse_vmm_args(arguments: &arguments::launch_args::LaunchArgs) -> GuestConfig {
    // FIDL requires we make a GuestConfig::default() before trying to update fields
    let mut guest_config = GuestConfig::default();

    if !arguments.cmdline_add.is_empty() {
        guest_config.cmdline_add = Some(arguments.cmdline_add.clone())
    };

    guest_config.guest_memory = arguments.memory;
    guest_config.cpus = arguments.cpus;
    guest_config.virtio_balloon = arguments.virtio_balloon;
    guest_config.virtio_console = arguments.virtio_console;
    guest_config.virtio_rng = arguments.virtio_rng;
    guest_config.virtio_vsock = arguments.virtio_vsock;
    guest_config.virtio_mem = arguments.virtio_mem;
    guest_config.virtio_mem_region_size = arguments.virtio_mem_region_size;
    guest_config.virtio_mem_region_alignment = arguments.virtio_mem_region_alignment;
    guest_config.virtio_mem_block_size = arguments.virtio_mem_block_size;

    guest_config
}

// Connect to a guest manager and launch the corresponding guest.
async fn launch<P: PlatformServices>(
    services: &P,
    vmid: arguments::Vmid,
    config: GuestConfig,
) -> Result<GuestProxy, LaunchResult> {
    let (guest, guest_server_end) = fidl::endpoints::create_proxy::<GuestMarker>();

    println!("Starting {}", vmid);
    let manager = services
        .connect_to_manager()
        .await
        .map_err(|err| LaunchResult::FidlError(format!("Connect to manager - {}", err)))?;

    match manager.launch(vmid.get(), config, guest_server_end).await {
        Err(fidl::Error::ClientChannelClosed { .. }) => Err(LaunchResult::RoutingError),
        Err(err) => Err(LaunchResult::FidlError(format!("Send launch message - {}", err))),
        Ok(launch_result) => match launch_result {
            Ok(()) => Ok(guest),
            Err(error) => {
                let guest_err = if error == GuestManagerError::StartFailure {
                    match manager.get_info(vmid.get()).await {
                        Ok(info) => info.stop_error.map(|e| e.into_primitive()),
                        Err(_) => None,
                    }
                } else {
                    None
                };
                Err(LaunchResult::LaunchFailure {
                    manager: error.into_primitive(),
                    guest: guest_err,
                })
            }
        },
    }
}
