// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::platform::PlatformServices;
use anyhow::{Context, Result};
use async_trait::async_trait;
use fidl_fuchsia_virtualization::{GuestManagerMarker, GuestManagerProxy};
use fuchsia_component::client::connect_to_protocol_at_path;
use guest_cli_args::GUEST_MANAGER_INTERFACE;

pub struct FuchsiaPlatformServices;

impl FuchsiaPlatformServices {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait(?Send)]
impl PlatformServices for FuchsiaPlatformServices {
    async fn connect_to_manager(&self) -> Result<GuestManagerProxy> {
        let manager = connect_to_protocol_at_path::<GuestManagerMarker>(
            format!("/svc/{}", GUEST_MANAGER_INTERFACE).as_str(),
        )
        .context("Failed to connect to manager service")?;
        Ok(manager)
    }
}
