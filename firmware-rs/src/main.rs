//! Maschine Mk1 standalone firmware — esp-rs port of the C spike in
//! ../firmware. Boot flow: saved config → join WiFi and run (USB host + OSC
//! land next); no config or join failure → captive portal (see portal.rs and
//! docs/design.md).

mod config;
mod osc;
mod portal;
mod usb;

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

    // Mk1 session runs regardless of WiFi/portal state.
    let led_tx = usb::start()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs.clone()))?,
        sysloop,
    )?;

    // Escape hatch: hold Shift+MIDI (Shift+Control in the kernel/cabl naming
    // — silkscreened "MIDI" on the unit) through the first couple of button
    // reports at boot to force portal mode even with a saved, working
    // config. Poll rather than a fixed sleep so a Mk1 that's slower to
    // enumerate still gets a fair, bounded window.
    let mut force_portal = false;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if usb::buttons_held(mk1_protocol::input::Button::Shift, mk1_protocol::input::Button::Control) {
            force_portal = true;
            break;
        }
    }
    if force_portal {
        log::info!("Shift+MIDI held at boot — forcing setup portal");
    }

    match (force_portal, Config::load(nvs.clone())?) {
        (true, _) => portal::run(&mut wifi, nvs),
        (false, None) => portal::run(&mut wifi, nvs),
        (false, Some(cfg)) => match join_sta(&mut wifi, &cfg) {
            Ok(()) => {
                log::info!("on {}, OSC target {}:{}", cfg.ssid, cfg.target_ip, cfg.target_port);
                osc::start(&cfg.target_ip, cfg.target_port, led_tx)?;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
            Err(e) => {
                log::warn!("WiFi join failed ({e}); starting setup portal");
                portal::run(&mut wifi, nvs)
            }
        },
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
