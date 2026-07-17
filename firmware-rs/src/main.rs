//! Maschine Mk1 standalone firmware — esp-rs port of the C spike in
//! ../firmware. Boot flow: saved config → join WiFi and run (USB host + OSC
//! land next); no config or join failure → captive portal (see portal.rs and
//! docs/design.md).

mod config;
mod portal;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::peripherals::Peripherals,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};

use config::Config;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs.clone()))?,
        sysloop,
    )?;

    match Config::load(nvs.clone())? {
        Some(cfg) => match join_sta(&mut wifi, &cfg) {
            Ok(()) => {
                log::info!(
                    "on {}, OSC target {}:{} — waiting for Maschine {:04x}:{:04x}",
                    cfg.ssid,
                    cfg.target_ip,
                    cfg.target_port,
                    mk1_protocol::VENDOR_ID,
                    mk1_protocol::PRODUCT_ID,
                );
                // USB host session + OSC bridge land here next.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
            Err(e) => {
                log::warn!("WiFi join failed ({e}); starting setup portal");
                portal::run(&mut wifi, nvs)
            }
        },
        None => portal::run(&mut wifi, nvs),
    }
}

fn join_sta(wifi: &mut BlockingWifi<EspWifi<'static>>, cfg: &Config) -> anyhow::Result<()> {
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: cfg
            .ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("ssid too long"))?,
        password: cfg
            .pass
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("password too long"))?,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?; // errors out on bad creds after esp-idf's internal retries
    wifi.wait_netif_up()?;
    Ok(())
}
