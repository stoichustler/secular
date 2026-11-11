// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Parser for `/boot/config/secular.cfg` (JSON5 NS physical memory map).

use serde::de::{self, Deserializer, Visitor};
use serde::Deserialize;
use std::fmt;

/// BOOTFS path packed by `//userspace/secular:secular-cfg`.
pub const CFG_PATH: &str = "/boot/config/secular.cfg";

/// Record kind (which JSON array the region came from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Window,
    Device,
    Reserve,
    Ram,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Window => "window",
            Kind::Device => "device",
            Kind::Reserve => "reserve",
            Kind::Ram => "ram",
        }
    }
}

/// Who may see a passthrough / RAM region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guest {
    All,
    Host,
    Vmid(u32),
}

/// DMA translation for a passthrough device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dma {
    None,
    Identity,
    Nested,
}

/// Stage-2 memory type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    Device,
    Cached,
}

/// One memory region after JSON5 decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub kind: Kind,
    pub name: String,
    pub paddr: u64,
    pub size: u64,
    pub role: Option<String>,
    pub irq: Option<u32>,
    pub sid: Option<u32>,
    pub guest: Guest,
    pub dma: Dma,
    pub cache: Cache,
    pub owner: Option<String>,
    pub ipa: Option<u64>,
}

impl Region {
    pub fn is_tipc_window(&self) -> bool {
        self.kind == Kind::Window && matches!(self.role.as_deref(), None | Some("tipc"))
    }
}

/// Parsed `secular.cfg`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    pub windows: Vec<Region>,
    pub devices: Vec<Region>,
    pub reserve: Vec<Region>,
    pub ram: Vec<Region>,
}

impl Config {
    pub fn tipc_windows(&self) -> impl Iterator<Item = &Region> {
        self.windows.iter().filter(|r| r.is_tipc_window())
    }
}

/// Decimal, `0x` hex, optional `K`/`M`/`G` suffix (1024).
pub fn parse_u64(token: &str) -> Option<u64> {
    let token = token.trim();
    let (num, mul) = if let Some(n) = token.strip_suffix('K').or_else(|| token.strip_suffix('k')) {
        (n, 1024u64)
    } else if let Some(n) = token.strip_suffix('M').or_else(|| token.strip_suffix('m')) {
        (n, 1024 * 1024)
    } else if let Some(n) = token.strip_suffix('G').or_else(|| token.strip_suffix('g')) {
        (n, 1024 * 1024 * 1024)
    } else {
        (token, 1)
    };
    let value = if let Some(hex) = num.strip_prefix("0x").or_else(|| num.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()?
    } else {
        num.parse().ok()?
    };
    value.checked_mul(mul)
}

/// Parse a JSON5 document. Empty input is an empty config.
pub fn parse(text: &str) -> Result<Config, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Config::default());
    }
    let file: File = serde_json5::from_str(text).map_err(|e| e.to_string())?;
    if file.version != 0 && file.version != 1 {
        return Err(format!("secular.cfg: unsupported version {}", file.version));
    }
    Ok(Config {
        version: if file.version == 0 { 1 } else { file.version },
        windows: map_regions(Kind::Window, file.windows),
        devices: map_regions(Kind::Device, file.devices),
        reserve: map_regions(Kind::Reserve, file.reserve),
        ram: map_regions(Kind::Ram, file.ram),
    })
}

/// TIPC windows in file order.
pub fn tipc_windows(cfg: &Config) -> Vec<&Region> {
    cfg.tipc_windows().collect()
}

#[derive(Default, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    windows: Vec<Raw>,
    #[serde(default)]
    devices: Vec<Raw>,
    #[serde(default)]
    reserve: Vec<Raw>,
    #[serde(default)]
    ram: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    name: String,
    #[serde(default, deserialize_with = "de_u64")]
    paddr: u64,
    #[serde(deserialize_with = "de_u64")]
    size: u64,
    #[serde(default)]
    role: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u32")]
    irq: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_u32")]
    sid: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_guest")]
    guest: Option<Guest>,
    #[serde(default)]
    dma: Option<String>,
    #[serde(default)]
    cache: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    ipa: Option<u64>,
}

fn map_regions(kind: Kind, raw: Vec<Raw>) -> Vec<Region> {
    raw.into_iter()
        .filter(|r| r.size != 0)
        .map(|r| Region {
            guest: r.guest.unwrap_or(if kind == Kind::Window { Guest::Host } else { Guest::All }),
            dma: match r.dma.as_deref() {
                Some("identity") => Dma::Identity,
                Some("nested") => Dma::Nested,
                _ => Dma::None,
            },
            cache: match r.cache.as_deref() {
                Some("cached") => Cache::Cached,
                Some("device") => Cache::Device,
                None if kind == Kind::Device => Cache::Device,
                _ => Cache::Cached,
            },
            kind,
            name: r.name,
            paddr: r.paddr,
            size: r.size,
            role: r.role,
            irq: r.irq,
            sid: r.sid,
            owner: r.owner,
            ipa: r.ipa,
        })
        .collect()
}

fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    d.deserialize_any(U64Visitor)
}

fn de_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    d.deserialize_any(OptU64Visitor)
}

fn de_opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(de_opt_u64(d)?.map(|v| v as u32))
}

fn de_opt_guest<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Guest>, D::Error> {
    d.deserialize_any(GuestVisitor)
}

struct U64Visitor;
impl Visitor<'_> for U64Visitor {
    type Value = u64;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number or size string")
    }
    fn visit_u64<E>(self, v: u64) -> Result<u64, E> {
        Ok(v)
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
        u64::try_from(v).map_err(E::custom)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
        parse_u64(v).ok_or_else(|| E::custom(format!("bad number `{v}`")))
    }
}

struct OptU64Visitor;
impl Visitor<'_> for OptU64Visitor {
    type Value = Option<u64>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number, size string, or null")
    }
    fn visit_none<E>(self) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_unit<E>(self) -> Result<Option<u64>, E> {
        Ok(None)
    }
    fn visit_u64<E>(self, v: u64) -> Result<Option<u64>, E> {
        Ok(Some(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<u64>, E> {
        Ok(Some(u64::try_from(v).map_err(E::custom)?))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<u64>, E> {
        parse_u64(v).map(Some).ok_or_else(|| E::custom(format!("bad number `{v}`")))
    }
}

struct GuestVisitor;
impl Visitor<'_> for GuestVisitor {
    type Value = Option<Guest>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("vmid, \"*\", or \"host\"")
    }
    fn visit_none<E>(self) -> Result<Option<Guest>, E> {
        Ok(None)
    }
    fn visit_unit<E>(self) -> Result<Option<Guest>, E> {
        Ok(None)
    }
    fn visit_u64<E>(self, v: u64) -> Result<Option<Guest>, E> {
        Ok(Some(Guest::Vmid(v as u32)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<Guest>, E> {
        Ok(Some(Guest::Vmid(u32::try_from(v).map_err(E::custom)?)))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<Guest>, E> {
        Ok(Some(match v {
            "*" => Guest::All,
            "host" => Guest::Host,
            other => Guest::Vmid(
                parse_u64(other)
                    .ok_or_else(|| E::custom(format!("bad guest `{other}`")))?
                    as u32,
            ),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_u64("4096"), Some(4096));
        assert_eq!(parse_u64("0x1000"), Some(0x1000));
        assert_eq!(parse_u64("4K"), Some(4096));
        assert_eq!(parse_u64("512M"), Some(512 * 1024 * 1024));
    }

    #[test]
    fn json5_windows() {
        let cfg = parse(
            r#"{
              // comment
              version: 1,
              windows: [
                { name: "tipc0", paddr: 0, size: "4K", role: "tipc" },
                { name: "tipc1", paddr: "0x7fe00000", size: "0x10000" },
              ],
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.windows[0].size, 0x1000);
        assert!(cfg.windows[0].is_tipc_window());
        assert_eq!(cfg.windows[1].paddr, 0x7fe00000);
        assert_eq!(tipc_windows(&cfg).len(), 2);
    }

    #[test]
    fn json5_passthrough() {
        let cfg = parse(
            r#"{
              devices: [
                { name: "uart1", paddr: "0x09040000", size: "4K", irq: 39, guest: 1, dma: "none", cache: "device" },
              ],
              reserve: [
                { name: "trusty", paddr: "0x7fe00000", size: "64K", owner: "trusty" },
              ],
              ram: [
                { name: "linux", paddr: 0, size: "512M", guest: 1 },
              ],
            }"#,
        )
        .unwrap();
        let d = &cfg.devices[0];
        assert_eq!(d.kind, Kind::Device);
        assert_eq!(d.irq, Some(39));
        assert_eq!(d.guest, Guest::Vmid(1));
        assert_eq!(d.dma, Dma::None);
        assert_eq!(d.cache, Cache::Device);
        assert_eq!(cfg.reserve[0].kind, Kind::Reserve);
        assert_eq!(cfg.ram[0].size, 512 * 1024 * 1024);
        assert!(tipc_windows(&cfg).is_empty());
    }

    #[test]
    fn empty_and_bad() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert!(parse("{ not json").is_err());
        assert!(parse("{ version: 99 }").is_err());
    }
}
