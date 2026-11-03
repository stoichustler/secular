// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgValue, FromArgs};
use std::fmt;

#[cfg(not(target_os = "fuchsia"))]
use ffx_core::ffx_command;

/// Userspace identifier of a guest. Parsed from `vmid=<id>` or a bare integer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Vmid(pub u32);

impl Vmid {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl FromArgValue for Vmid {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        let rest = value.strip_prefix("vmid=").unwrap_or(value);
        rest.parse::<u32>().map(Vmid).map_err(|_| {
            format!(
                "Invalid vmid \"{}\". Expected `vmid=<id>` (for example `vmid=0`) \
                 or a non-negative integer.",
                value
            )
        })
    }
}

impl fmt::Display for Vmid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vmid={}", self.0)
    }
}

/// Unified guest_manager is a bootstrap child on Secular.
pub const GUEST_MANAGER_MONIKER: &str = "/bootstrap/guest-manager";
pub const GUEST_MANAGER_INTERFACE: &str = "fuchsia.virtualization.GuestManager";
pub const GUEST_PACKAGE_URL: &str = "fuchsia-boot:///guest_manager#meta/guest_pkg.cm";

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Top-level command.
pub struct GuestOptions {
    #[argh(subcommand)]
    pub nested: SubCommands,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum SubCommands {
    Attach(crate::attach_args::AttachArgs),
    Launch(crate::launch_args::LaunchArgs),
    Stop(crate::stop_args::StopArgs),
    List(crate::list_args::ListArgs),
}

pub mod list_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// List guests, or show details for one guest. Usage: vmm list [vmid=0]
    #[argh(subcommand, name = "list")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct ListArgs {
        #[argh(positional)]
        /// optional guest identifier to get detailed information about, for example vmid=0
        pub vmid: Option<Vmid>,
    }
}

pub mod launch_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Launch a guest image. Usage: vmm launch vmid=0 [--cmdline-add <arg>...] [--memory <memory-size>] [--cpus <num-cpus>] [--virtio-* <bool>]
    #[argh(subcommand, name = "launch")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct LaunchArgs {
        #[argh(positional)]
        /// guest identifier to launch, for example vmid=0
        pub vmid: Vmid,
        /// adds provided strings to the existing kernel command line
        #[argh(option)]
        pub cmdline_add: Vec<String>,
        /// allocate 'bytes' of memory for the guest
        #[argh(option)]
        pub memory: Option<u64>,
        /// number of virtual cpus available for the guest
        #[argh(option)]
        pub cpus: Option<u8>,
        /// enable virtio-balloon
        #[argh(option)]
        pub virtio_balloon: Option<bool>,
        /// enable virtio-console
        #[argh(option)]
        pub virtio_console: Option<bool>,
        /// enable virtio-rng
        #[argh(option)]
        pub virtio_rng: Option<bool>,
        /// enable virtio-vsock
        #[argh(option)]
        pub virtio_vsock: Option<bool>,
        /// enable virtio-mem to allow dynamically (un)plug memory to the guest
        #[argh(option)]
        pub virtio_mem: Option<bool>,
        /// virtio-mem pluggable region size
        #[argh(option)]
        pub virtio_mem_region_size: Option<u64>,
        /// virtio-mem pluggable region alignment
        #[argh(option)]
        pub virtio_mem_region_alignment: Option<u64>,
        /// virtio-mem pluggable blocksize
        #[argh(option)]
        pub virtio_mem_block_size: Option<u64>,
        /// detach from a guest allowing it to run in the background
        #[argh(switch, short = 'd')]
        pub detach: bool,
    }
}

pub mod stop_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Stop a running guest. Usage: vmm stop vmid=0 [-f]
    #[argh(subcommand, name = "stop")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct StopArgs {
        /// guest identifier to stop, for example vmid=0
        #[argh(positional)]
        pub vmid: Vmid,
        /// force stop the guest
        #[argh(switch, short = 'f')]
        pub force: bool,
    }
}

pub mod attach_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Attach console and serial to a running guest. Usage: vmm attach vmid=0
    #[argh(subcommand, name = "attach")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct AttachArgs {
        /// guest identifier to attach to, for example vmid=0
        #[argh(positional)]
        pub vmid: Vmid,
        /// attach via serial instead of virtio-console
        #[argh(switch)]
        pub serial: bool,
    }
}
