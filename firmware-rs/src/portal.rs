//! WLED-style onboarding: WPA2 AP `maschine-XXXX` + DNS catch-all + captive
//! portal asking for WiFi credentials and the OSC target. Saves to NVS and
//! reboots into STA mode.

use std::net::UdpSocket;

use esp_idf_svc::{
    handle::RawHandle,
    http::server::{Configuration as HttpConfig, EspHttpServer},
    http::Method,
    io::Write,
    nvs::EspDefaultNvsPartition,
    wifi::{AccessPointConfiguration, AuthMethod, BlockingWifi, Configuration, EspWifi, WifiDeviceId},
};

use crate::config::Config;

const PAGE: &str = r#"<!doctype html><title>Maschine Mk1</title>
<meta name=viewport content="width=device-width,initial-scale=1">
<style>body{font-family:system-ui;max-width:22em;margin:2em auto;padding:0 1em}
label{display:block;margin:.8em 0 .2em}input{width:100%;padding:.4em}
button{margin-top:1.2em;padding:.5em 1.5em}</style>
<h1>Maschine Mk1</h1>
<form method=post action=/save>
<label>WiFi network</label><input name=ssid required>
<label>WiFi password</label><input name=pass type=password>
<label>OSC target IP</label><input name=ip required placeholder="192.168.1.x">
<label>OSC target port</label><input name=port value="9000">
<button>Save &amp; reboot</button>
</form>"#;

/// Runs forever (the way out is the post-save reboot).
pub fn run(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    nvs: EspDefaultNvsPartition,
) -> anyhow::Result<()> {
    let mac = wifi.wifi_mut().driver_mut().get_mac(WifiDeviceId::Ap)?;
    let ssid = format!("maschine-{:02x}{:02x}", mac[4], mac[5]);
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: ssid.as_str().try_into().unwrap(),
        password: "maschine".try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;

    let ap_ip = wifi.wifi().ap_netif().get_ip_info()?.ip;
    offer_dns_over_dhcp(wifi, ap_ip)?;
    log::info!("portal up: AP {ssid} (password: maschine), http://{ap_ip}/");

    std::thread::Builder::new()
        .name("dns".into())
        .stack_size(4096)
        .spawn(move || dns_catch_all(ap_ip))?;

    let mut server = EspHttpServer::new(&HttpConfig {
        uri_match_wildcard: true,
        ..Default::default()
    })?;

    server.fn_handler("/", Method::Get, |req| {
        req.into_ok_response()?.write_all(PAGE.as_bytes())?;
        Ok::<(), anyhow::Error>(())
    })?;

    server.fn_handler("/save", Method::Post, move |mut req| {
        let mut body = [0u8; 512];
        let mut n = 0;
        loop {
            let r = req.read(&mut body[n..])?;
            if r == 0 || n + r == body.len() {
                n += r;
                break;
            }
            n += r;
        }
        let form = std::str::from_utf8(&body[..n])?;
        let ssid = form_value(form, "ssid").unwrap_or_default();
        let pass = form_value(form, "pass").unwrap_or_default();
        let ip = form_value(form, "ip").unwrap_or_default();
        let port = form_value(form, "port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(9000);
        if ssid.is_empty() {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"ssid is required")?;
            return Ok::<(), anyhow::Error>(());
        }
        Config::save(nvs.clone(), &ssid, &pass, &ip, port)?;
        log::info!("saved config for {ssid}, target {ip}:{port}; rebooting");
        req.into_ok_response()?
            .write_all(b"Saved. Rebooting onto your WiFi...")?;
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            esp_idf_svc::hal::reset::restart();
        });
        Ok(())
    })?;

    // Captive-portal probes (generate_204, hotspot-detect.html, ...) and
    // everything else: redirect to the form.
    let root = format!("http://{ap_ip}/");
    server.fn_handler("/*", Method::Get, move |req| {
        req.into_response(302, Some("Found"), &[("Location", &root)])?;
        Ok::<(), anyhow::Error>(())
    })?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// Make the softAP DHCP server hand out our own address as DNS, so clients
/// actually ask our catch-all. Raw esp_netif calls — verify on hardware.
fn offer_dns_over_dhcp(
    wifi: &BlockingWifi<EspWifi<'static>>,
    ip: std::net::Ipv4Addr,
) -> anyhow::Result<()> {
    use esp_idf_svc::sys::*;
    let netif = wifi.wifi().ap_netif().handle();
    unsafe {
        let mut dns = esp_netif_dns_info_t::default();
        dns.ip.u_addr.ip4.addr = u32::from_ne_bytes(ip.octets());
        dns.ip.type_ = 0; // ESP_IPADDR_TYPE_V4
        esp!(esp_netif_set_dns_info(
            netif,
            esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
            &mut dns,
        ))?;
        let mut opt: u8 = 1; // DHCPS_OFFER_DNS
        esp!(esp_netif_dhcps_option(
            netif,
            esp_netif_dhcp_option_mode_t_ESP_NETIF_OP_SET,
            esp_netif_dhcp_option_id_t_ESP_NETIF_DOMAIN_NAME_SERVER,
            &mut opt as *mut _ as *mut core::ffi::c_void,
            1,
        ))?;
    }
    Ok(())
}

/// ponytail: minimal DNS — answer every query with our AP address. Good enough
/// to trip captive-portal detection; not a real resolver.
fn dns_catch_all(ip: std::net::Ipv4Addr) {
    let sock = match UdpSocket::bind("0.0.0.0:53") {
        Ok(s) => s,
        Err(e) => {
            log::error!("DNS bind failed: {e}");
            return;
        }
    };
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, peer)) = sock.recv_from(&mut buf) else {
            continue;
        };
        if n < 12 || buf[2] & 0x80 != 0 {
            continue; // not a query
        }
        let mut resp = buf[..n].to_vec();
        resp[2] = 0x81; // response, recursion desired copied out of laziness
        resp[3] = 0x80; // recursion available, NOERROR
        resp[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT 1
        resp[8..12].fill(0); // NSCOUNT/ARCOUNT 0
        // answer: pointer to qname at 0x0c, A IN TTL 60, our IP
        resp.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        resp.extend_from_slice(&ip.octets());
        let _ = sock.send_to(&resp, peer);
    }
}

/// Pull one field out of application/x-www-form-urlencoded.
fn form_value(form: &str, key: &str) -> Option<String> {
    form.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
