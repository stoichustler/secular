// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::platform::PlatformServices;
use anyhow::Result;
use async_trait::async_trait;
use fidl::endpoints::{DiscoverableProtocolMarker, create_proxy};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_io::OpenFlags;
use fidl_fuchsia_sys2 as fsys;
use fidl_fuchsia_virtualization::{GuestManagerMarker, GuestManagerProxy};
use guest_cli_args::{GUEST_MANAGER_INTERFACE, GUEST_MANAGER_MONIKER};

pub struct HostPlatformServices {
    remote_control: RemoteControlProxy,
}

impl HostPlatformServices {
    pub fn new(remote_control: RemoteControlProxy) -> Self {
        Self { remote_control }
    }
}

#[async_trait(?Send)]
impl PlatformServices for HostPlatformServices {
    async fn connect_to_manager(&self) -> Result<GuestManagerProxy> {
        let (guest_manager, server_end) = create_proxy::<GuestManagerMarker>();
        // This may fail, but we report the error when we later try to use the GuestManagerProxy.
        let _ = self
            .remote_control
            .connect_capability(
                GUEST_MANAGER_MONIKER,
                fsys::OpenDirType::ExposedDir,
                GUEST_MANAGER_INTERFACE,
                server_end.into_channel(),
            )
            .await?;
        Ok(guest_manager)
    }
}
