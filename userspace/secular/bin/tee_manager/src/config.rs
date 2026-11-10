// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use serde_derive::Deserialize;
use std::fs::File;
use std::io::BufReader;
use uuid::Uuid;

const CONFIG_FILE: &'static str = "/config/data/tee_manager.config";

#[derive(Deserialize)]
pub struct Config {
    pub application_uuids: Vec<Uuid>,
}

impl Config {
    /// Returns `Ok(None)` when no application config is installed.
    pub fn from_file() -> Result<Option<Self>, Error> {
        match File::open(CONFIG_FILE) {
            Ok(file) => Ok(Some(
                serde_json::from_reader(BufReader::new(file)).context("Unable to parse config")?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("Unable to open config"),
        }
    }
}
