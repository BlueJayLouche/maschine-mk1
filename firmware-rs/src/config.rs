//! WiFi + OSC target config in NVS. Profiles (docs/design.md) come later and
//! live on a flash partition, not here.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};

pub const NAMESPACE: &str = "mk1";

pub struct Config {
    pub ssid: String,
    pub pass: String,
    pub target_ip: String,
    pub target_port: u16,
}

impl Config {
    /// None until the portal has saved WiFi credentials.
    pub fn load(part: EspDefaultNvsPartition) -> anyhow::Result<Option<Config>> {
        let nvs = EspNvs::new(part, NAMESPACE, true)?;
        let mut buf = [0u8; 96];
        let Some(ssid) = nvs.get_str("ssid", &mut buf)?.map(str::to_string) else {
            return Ok(None);
        };
        if ssid.is_empty() {
            return Ok(None);
        }
        let pass = nvs.get_str("pass", &mut buf)?.unwrap_or_default().to_string();
        let target_ip = nvs.get_str("ip", &mut buf)?.unwrap_or_default().to_string();
        let target_port = nvs.get_u16("port")?.unwrap_or(9000);
        Ok(Some(Config { ssid, pass, target_ip, target_port }))
    }

    pub fn save(
        part: EspDefaultNvsPartition,
        ssid: &str,
        pass: &str,
        ip: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let mut nvs = EspNvs::new(part, NAMESPACE, true)?;
        nvs.set_str("ssid", ssid)?;
        nvs.set_str("pass", pass)?;
        nvs.set_str("ip", ip)?;
        nvs.set_u16("port", port)?;
        Ok(())
    }
}
