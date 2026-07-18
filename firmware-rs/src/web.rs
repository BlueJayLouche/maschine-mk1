//! Profiles web page, served once joined to the LAN (portal.rs handles
//! pre-join onboarding). Paste/download profile JSON per slot, pick the
//! active one. ponytail: raw JSON textarea, no mapping editor — per
//! docs/design.md v1. No auth: same LAN-trust story as the OSC surface.

use esp_idf_svc::{
    http::server::{Configuration as HttpConfig, EspHttpServer},
    http::Method,
    io::Write,
    nvs::EspDefaultNvsPartition,
};

use crate::osc;
use crate::profile;

const PAGE_HEAD: &str = r#"<!doctype html><title>Maschine Mk1 profiles</title>
<meta name=viewport content="width=device-width,initial-scale=1">
<style>body{font-family:system-ui;max-width:44em;margin:2em auto;padding:0 1em}
button{padding:.4em 1em;margin:.15em}button.on{background:#2a6}
textarea{width:100%;font-family:monospace;font-size:.85em;margin:.8em 0}
#msg{color:#2a6}#msg.err{color:#c33}</style>
<h1>Maschine Mk1 profiles</h1>
<div id=slots></div>
<textarea id=json rows=22 spellcheck=false></textarea><br>
<button onclick=save()>Save slot</button>
<button onclick=activate()>Activate slot</button>
<span id=msg></span>
<p>Sparse JSON: <code>{"name","target"?,"sync"?,"map":{"a/pad/1":{"osc","led_source"?,"label"?}}}</code>.
Keys: <code>&lt;a-h&gt;/pad/1-16</code>, <code>&lt;a-h&gt;/knob/1-8</code>,
<code>&lt;a-h&gt;/softkey/1-8</code>, <code>volume|tempo|swing</code>,
<code>button/&lt;name&gt;</code>. Unmapped controls use
<code>/maschine/&lt;key&gt;</code>; an entry without <code>osc</code> is silent.
Switch on the unit: hold Shift + group button.</p>
<script>"#;

// Injected between head and tail: `let names=[...],active=N;`
const PAGE_TAIL: &str = r#"let cur=active;
const S=document.getElementById('slots'),T=document.getElementById('json'),M=document.getElementById('msg');
function paint(){S.innerHTML='';names.forEach((n,i)=>{const b=document.createElement('button');
b.textContent=(i+1)+(i==active?' ★':'')+(n?' '+n:'');b.className=i==cur?'on':'';
b.onclick=()=>pick(i);S.append(b)})}
async function pick(i){cur=i;T.value=await(await fetch('/profile?n='+i)).text();paint()}
function msg(t,e){M.textContent=t;M.className=e?'err':''}
async function save(){const r=await fetch('/profile?n='+cur,{method:'POST',body:T.value});
msg(await r.text(),!r.ok);if(r.ok){try{names[cur]=JSON.parse(T.value).name||''}catch{};paint()}}
async function activate(){const r=await fetch('/activate?n='+cur,{method:'POST'});
msg(await r.text(),!r.ok);if(r.ok){active=cur;paint()}}
pick(cur);</script>"#;

fn slot_param(uri: &str) -> Option<u8> {
    let q = uri.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("n="))?
        .parse::<u8>()
        .ok()
        .filter(|&n| n < 8)
}

pub fn serve(nvs: EspDefaultNvsPartition) -> anyhow::Result<EspHttpServer<'static>> {
    // uri_match_wildcard: esp-idf matches handlers against the full URI
    // including the query string, so "/profile?n=3" needs "/profile*".
    let mut server = EspHttpServer::new(&HttpConfig {
        stack_size: 10240,
        uri_match_wildcard: true,
        ..Default::default()
    })?;

    let n = nvs.clone();
    server.fn_handler("/", Method::Get, move |req| {
        let names: Vec<String> = (0..8)
            .map(|s| profile::load(n.clone(), s).map(|p| p.name).unwrap_or_default())
            .collect();
        let inject = format!(
            "let names={},active={};",
            serde_json::to_string(&names)?,
            profile::active(n.clone())
        );
        let mut resp = req.into_ok_response()?;
        resp.write_all(PAGE_HEAD.as_bytes())?;
        resp.write_all(inject.as_bytes())?;
        resp.write_all(PAGE_TAIL.as_bytes())?;
        Ok::<(), anyhow::Error>(())
    })?;

    let n = nvs.clone();
    server.fn_handler("/profile*", Method::Get, move |req| {
        let Some(slot) = slot_param(req.uri()) else {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"n=0..7 required")?;
            return Ok::<(), anyhow::Error>(());
        };
        let json = profile::load_json(n.clone(), slot)
            .unwrap_or_else(|| "{\n  \"name\": \"\",\n  \"map\": {}\n}".into());
        req.into_response(200, Some("OK"), &[("Content-Type", "application/json")])?
            .write_all(json.as_bytes())?;
        Ok(())
    })?;

    let n = nvs.clone();
    server.fn_handler("/profile*", Method::Post, move |mut req| {
        let Some(slot) = slot_param(req.uri()) else {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"n=0..7 required")?;
            return Ok::<(), anyhow::Error>(());
        };
        let mut body = vec![0u8; profile::MAX_JSON + 1];
        let mut len = 0;
        loop {
            let r = req.read(&mut body[len..])?;
            len += r;
            if r == 0 || len == body.len() {
                break;
            }
        }
        let parsed = std::str::from_utf8(&body[..len])
            .map_err(anyhow::Error::from)
            .and_then(|s| profile::save_json(n.clone(), slot, s));
        match parsed {
            Ok(p) => {
                log::info!("profile {slot} \"{}\" saved via web", p.name);
                if slot == profile::active(n.clone()) {
                    osc::publish(osc::Event::Reload);
                }
                req.into_ok_response()?.write_all(b"saved")?;
            }
            Err(e) => {
                req.into_response(400, Some("Bad Request"), &[])?
                    .write_all(format!("rejected: {e}").as_bytes())?;
            }
        }
        Ok(())
    })?;

    let n = nvs.clone();
    server.fn_handler("/activate*", Method::Post, move |req| {
        let Some(slot) = slot_param(req.uri()) else {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"n=0..7 required")?;
            return Ok::<(), anyhow::Error>(());
        };
        profile::set_active(n.clone(), slot)?;
        osc::publish(osc::Event::Reload);
        log::info!("profile {slot} activated via web");
        req.into_ok_response()?.write_all(b"activated")?;
        Ok(())
    })?;

    Ok(server)
}
