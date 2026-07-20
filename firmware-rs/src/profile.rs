//! Profile storage — 8 sparse JSON profiles (docs/design.md "Profiles").
//!
//! ponytail: slots are NVS blobs (`p0`–`p7`), not files on a dedicated flash
//! partition — same flash, no partition-table change or re-onboarding. The
//! default 24 KB NVS holds 8 profiles at the size cap below; move to a custom
//! partition table if that ceiling is ever hit for real.

use std::collections::HashMap;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use serde::{Deserialize, Serialize};

const NS: &str = "mk1prof";
/// Per-profile JSON size cap (trust boundary: uploads larger than this are
/// rejected before they can wedge NVS).
pub const MAX_JSON: usize = 3072;

/// Sparse profile: any control without a map entry uses the generated
/// default address (`/maschine/<key>`); an entry with no `osc` silences it.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    /// Overrides the device-config OSC target, `"ip:port"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Feedback registration: this address is sent to the target every 5 s
    /// with our LED port (9001) as an int arg. rustjay: `"/rustjay/sync"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,
    /// Keyed by control: banked `"<a-h>/pad/<1-16>"`, `"<a-h>/knob/<1-8>"`,
    /// `"<a-h>/softkey/<1-8>"`; global `"volume"|"tempo"|"swing"` and
    /// `"button/<name>"`.
    #[serde(default)]
    pub map: HashMap<String, Entry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc: Option<String>,
    /// Knobs only. `"wrap"` sends a wrapping position (never clamped) instead
    /// of the rail-clamped virtual one — for hosts that derive relative
    /// deltas (vp404 trim knobs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Knobs only: sensitivity, default 1.0 (one revolution = full range).
    /// E.g. 0.15 ≈ frame-accurate trim on a 1200-frame clip (the minimum
    /// published knob step is 5/999 of a revolution). f64 not f32: serde's
    /// f32 primitive path trips an LLVM Xtensa ISel crash (PCREL_WRAPPER on
    /// an f32 constant pool) in the esp toolchain — cast at the use site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Incoming OSC address (on :9001) that drives this control's LED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_source: Option<String>,
    /// Held for the screens feature; not rendered yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Held for DIN MIDI OUT; not emitted yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<serde_json::Value>,
}

fn key(slot: u8) -> String {
    format!("p{slot}")
}

pub fn load_json(part: EspDefaultNvsPartition, slot: u8) -> Option<String> {
    let nvs = EspNvs::new(part, NS, true).ok()?;
    let mut buf = vec![0u8; MAX_JSON];
    let raw = nvs.get_raw(&key(slot), &mut buf).ok()??;
    String::from_utf8(raw.to_vec()).ok()
}

pub fn load(part: EspDefaultNvsPartition, slot: u8) -> Option<Profile> {
    serde_json::from_str(&load_json(part, slot)?).ok()
}

/// Validate + persist one slot; returns the parsed profile.
pub fn save_json(part: EspDefaultNvsPartition, slot: u8, json: &str) -> anyhow::Result<Profile> {
    anyhow::ensure!(json.len() <= MAX_JSON, "profile too large (max {MAX_JSON} bytes)");
    let p: Profile = serde_json::from_str(json)?;
    let mut nvs = EspNvs::new(part, NS, true)?;
    nvs.set_raw(&key(slot), json.as_bytes())?;
    Ok(p)
}

pub fn active(part: EspDefaultNvsPartition) -> u8 {
    EspNvs::new(part, crate::config::NAMESPACE, true)
        .ok()
        .and_then(|nvs| nvs.get_u8("prof").ok().flatten())
        .unwrap_or(0)
        .min(7)
}

pub fn set_active(part: EspDefaultNvsPartition, slot: u8) -> anyhow::Result<()> {
    let nvs = EspNvs::new(part, crate::config::NAMESPACE, true)?;
    nvs.set_u8("prof", slot.min(7))?;
    Ok(())
}
