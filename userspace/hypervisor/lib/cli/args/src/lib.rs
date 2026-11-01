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
pub const GUEST_PACKAGE_URL: &str = "fuchsia-boot:///guest_manager#meta/secular_guest_pkg.cm";

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
    Balloon(crate::balloon_args::BalloonArgs),
    List(crate::list_args::ListArgs),
    Socat(crate::socat_args::SocatArgs),
    VsockPerf(crate::vsockperf_args::VsockPerfArgs),
    Mem(crate::mem_args::MemArgs),
}

pub mod mem_args {
    use super::*;
    /// Interact with the guest virtio-mem. Usage: guest mem sub-command [ request-plugged or stats]
    #[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
    #[argh(subcommand, name = "mem")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct MemArgs {
        #[argh(subcommand)]
        pub mem_cmd: MemCommands,
    }
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    #[argh(subcommand)]
    pub enum MemCommands {
        RequestPluggedMem(RequestPluggedMem),
        StatsMem(StatsMem),
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Modify the requested amount of the dynamically plugged memory. Usage: guest mem request-plugged vmid=0 size
    #[argh(subcommand, name = "request-plugged")]
    pub struct RequestPluggedMem {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
        #[argh(positional)]
        /// target amount of memory to be dynamically plugged
        pub size: u64,
    }
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// See the stats of a guest's virtio-mem. Usage: guest mem stats vmid=0
    #[argh(subcommand, name = "stats")]
    pub struct StatsMem {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
    }
}

pub mod balloon_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Interact with the guest memory balloon. Usage: guest balloon sub-command vmid=0 ...
    #[argh(subcommand, name = "balloon")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct BalloonArgs {
        #[argh(subcommand)]
        pub balloon_cmd: BalloonCommands,
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    #[argh(subcommand)]
    pub enum BalloonCommands {
        Set(BalloonSet),
        Stats(BalloonStats),
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Modify the size of a memory balloon. Usage: guest balloon set vmid=0 num-pages
    #[argh(subcommand, name = "set")]
    pub struct BalloonSet {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
        #[argh(positional)]
        /// number of pages guest balloon will have after use.
        pub num_pages: u32,
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// See the stats of a guest's memory balloon. Usage: guest balloon stats vmid=0
    #[argh(subcommand, name = "stats")]
    pub struct BalloonStats {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
    }
}

pub mod list_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// List available guest environments.
    #[argh(subcommand, name = "list")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct ListArgs {
        #[argh(positional)]
        /// optional guest identifier to get detailed information about, for example vmid=0
        pub vmid: Option<Vmid>,
    }
}

pub mod socat_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Interact with the guest via socat. See the sub-command help for details.
    #[argh(subcommand, name = "socat")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct SocatArgs {
        #[argh(subcommand)]
        pub socat_cmd: SocatCommands,
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    #[argh(subcommand)]
    pub enum SocatCommands {
        Listen(SocatListen),
        Connect(SocatConnect),
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Create a socat connection on the specified port. Usage: guest socat connect vmid=0 port
    #[argh(subcommand, name = "connect")]
    pub struct SocatConnect {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
        #[argh(positional)]
        /// guest port number to attempt to connect to
        pub guest_port: u32,
    }

    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Listen through socat on the specified port. Usage: guest socat listen vmid=0 host-port
    #[argh(subcommand, name = "listen")]
    pub struct SocatListen {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
        #[argh(positional)]
        /// host port number to accept incoming guest connections on
        pub host_port: u32,
    }
}

pub mod vsockperf_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Perform a vsock micro benchmark on the target guest.
    #[argh(subcommand, name = "vsock-perf")]
    #[cfg_attr(not(target_os = "fuchsia"), ffx_command())]
    pub struct VsockPerfArgs {
        #[argh(positional)]
        /// guest identifier, for example vmid=0
        pub vmid: Vmid,
    }
}

pub mod launch_args {
    use super::*;
    #[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
    /// Launch a guest image. Usage: guest launch vmid=0 [--cmdline-add <arg>...] [--memory <memory-size>] [--cpus <num-cpus>] [--virtio-* <bool>]
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
    /// Stop a running guest. Usage: guest stop vmid=0 [-f]
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
    /// Attach console and serial to a running guest. Usage: guest attach vmid=0
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
