use std::sync::{Arc, Mutex};
use std::net::ToSocketAddrs;

// embedded_graphics imports not needed in main.rs
use embedded_graphics::prelude::RgbColor;
use esp_idf_svc::eventloop::EspSystemEventLoop;

use crate::ui::DisplayTargetDrive;
use crate::boards::ui::{DisplayBuffer, new_chat_ui};

mod app;
mod audio;
mod bt;
mod codec;
mod network;
mod protocol;
mod ui;
mod ws;

mod boards;

mod peripheral;

#[derive(Debug, Clone)]
struct Setting {
    ssid: String,
    pass: String,
    server_url: String,
    background_gif: (Vec<u8>, bool), // (data, ended)
    avatar_gif: (Vec<u8>, bool),     // (data, ended)
    state: u8,                       // if 1, enter setup mode
    // AFE parameters
    afe_linear_gain: f32,
    agc_target_level_dbfs: i32,
    agc_compression_gain_db: i32,
}

impl Setting {
    fn load_from_nvs(nvs: &esp_idf_svc::nvs::EspDefaultNvs) -> anyhow::Result<Self> {
        let mut str_buf = [0; 128];

        let ssid = nvs
            .get_str("ssid", &mut str_buf)
            .map_err(|e| log::error!("Failed to get ssid: {:?}", e))
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string();

        let pass = nvs
            .get_str("pass", &mut str_buf)
            .map_err(|e| log::error!("Failed to get pass: {:?}", e))
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string();

        static DEFAULT_SERVER_URL: Option<&str> = std::option_env!("DEFAULT_SERVER_URL");
        log::info!("DEFAULT_SERVER_URL: {:?}", DEFAULT_SERVER_URL);

        let server_url = nvs
            .get_str("server_url", &mut str_buf)
            .map_err(|e| log::error!("Failed to get server_url: {:?}", e))
            .ok()
            .flatten()
            .or(DEFAULT_SERVER_URL)
            .unwrap_or_default()
            .to_string();

        let background_gif = if nvs.contains("background_gif")? {
            let background_gif_size = nvs
                .blob_len("background_gif")
                .map_err(|e| log::error!("Failed to get background_gif size: {:?}", e))
                .ok()
                .flatten()
                .unwrap_or(1024 * 1024);

            let mut gif_buf = vec![0; background_gif_size];
            let gif_buf_ = nvs
                .get_blob("background_gif", &mut gif_buf)?
                .unwrap_or(ui::DEFAULT_BACKGROUND);

            if gif_buf_.len() != background_gif_size {
                log::warn!(
                    "Background GIF size mismatch: expected {}, got {}",
                    background_gif_size,
                    gif_buf_.len()
                );
                gif_buf_.to_vec()
            } else {
                gif_buf
            }
        } else {
            ui::DEFAULT_BACKGROUND.to_vec()
        };

        let avatar_gif = if nvs.contains("avatar_gif")? {
            let avatar_gif_size = nvs
                .blob_len("avatar_gif")
                .map_err(|e| log::error!("Failed to get avatar_gif size: {:?}", e))
                .ok()
                .flatten()
                .unwrap_or(128 * 1024);

            let mut gif_buf = vec![0; avatar_gif_size];
            let gif_buf_ = nvs.get_blob("avatar_gif", &mut gif_buf)?.unwrap_or(&[]);

            if gif_buf_.len() != avatar_gif_size {
                log::warn!(
                    "Avatar GIF size mismatch: expected {}, got {}",
                    avatar_gif_size,
                    gif_buf_.len()
                );
                gif_buf_.to_vec()
            } else {
                gif_buf
            }
        } else {
            Vec::new()
        };

        let state = nvs.get_u8("state")?.unwrap_or(0);

        let mut afe_linear_gain_buf = [0u8; 4];
        let afe_linear_gain = nvs
            .get_blob("afe_linear_gain", &mut afe_linear_gain_buf)
            .map_err(|e| {
                log::error!("Failed to get afe_linear_gain: {:?}", e);
            })
            .ok()
            .flatten()
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(unsafe { audio::AFE_LINEAR_GAIN });

        let agc_target_level_dbfs = nvs
            .get_i32("agc_tl_dbfs")
            .map_err(|e| {
                log::error!("Failed to get agc_target_level_dbfs: {:?}", e);
            })
            .ok()
            .flatten()
            .unwrap_or(unsafe { audio::AGC_TARGET_LEVEL_DBFS });

        let agc_compression_gain_db = nvs
            .get_i32("agc_cg_db")
            .map_err(|e| {
                log::error!("Failed to get agc_compression_gain_db: {:?}", e);
            })
            .ok()
            .flatten()
            .unwrap_or(unsafe { audio::AGC_COMPRESSION_GAIN_DB });

        Ok(Setting {
            ssid,
            pass,
            server_url,
            background_gif: (background_gif, false),
            avatar_gif: (avatar_gif, false),
            state,
            afe_linear_gain,
            agc_target_level_dbfs,
            agc_compression_gain_db,
        })
    }

    fn need_init(&self) -> bool {
        self.state == 1
            || self.ssid.is_empty()
            || self.pass.is_empty()
            || self.server_url.is_empty()
    }
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals = esp_idf_svc::hal::prelude::Peripherals::take().unwrap();

    // Early diagnostic: force backlight GPIO48 HIGH to power panel during startup.

        log::info!("Early backlight diagnostic: forcing GPIO48 HIGH");
        unsafe {
            use esp_idf_svc::sys::*;
            let bl_num = 48 as i32;
            let _ = esp!(gpio_set_direction(bl_num, gpio_mode_t_GPIO_MODE_OUTPUT));
            let _ = esp!(gpio_set_level(bl_num, 1));
        }
    let sysloop = EspSystemEventLoop::take()?;
    let _fs = esp_idf_svc::io::vfs::MountedEventfs::mount(20)?;
    let partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let nvs = esp_idf_svc::nvs::EspDefaultNvs::new(partition, "setting", true)?;

    let setting = Setting::load_from_nvs(&nvs)?;
    // Share Setting + NVS in a mutex so BLE handlers can modify NVS anytime
    let setting = Arc::new(Mutex::new((setting, nvs)));
    {
        let s = setting.lock().unwrap();
        s.1.set_u8("state", 0).unwrap();
        log::info!("SSID: {:?}", s.0.ssid);
        log::info!("PASS: {:?}", s.0.pass);
        log::info!("Server URL: {:?}", s.0.server_url);
    }

    log_heap();

    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(64);
    let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();

    // (HTTP server on :8080 removed — we start the main HTTP server after Wi‑Fi)
    // Small current-thread runtime used by main for blocking async helpers (ws, button waits)
    let b = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        // Increase default thread stack size for blocking thread pool to avoid
        // small-stack crashes when tokio spawns blocking DNS/IO threads on ESP32.
        .thread_stack_size(128 * 1024)
        .build()
        .expect("failed to build main current-thread runtime");

    // Initialize board HAL (SPI + LCD) before attempting any framebuffer flushes.
    // This ensures ESP_LCD_PANEL_HANDLE is set and `flush_display` won't return 258.
    #[cfg(feature = "esp32s3cam")]
    {
        if let Err(e) = crate::boards::esp32s3cam::init_spi(
            peripherals.spi3,
            peripherals.pins.gpio20,
            peripherals.pins.gpio19,
        ) {
            log::error!("Failed to init SPI for display: {:?}", e);
        }
        if let Err(e) = crate::boards::esp32s3cam::init_lcd(
            peripherals.pins.gpio45,
            peripherals.pins.gpio47,
            peripherals.pins.gpio21,
        ) {
            log::error!("Failed to init LCD: {:?}", e);
        }
    }

    #[cfg(all(feature = "boards", not(feature = "esp32s3cam")))]
    {
        if let Err(e) = crate::boards::base::init_spi(
            peripherals.spi3,
            peripherals.pins.gpio20,
            peripherals.pins.gpio19,
        ) {
            log::error!("Failed to init SPI for display (base): {:?}", e);
        }
        if let Err(e) = crate::boards::base::init_lcd(
            peripherals.pins.gpio45,
            peripherals.pins.gpio47,
            peripherals.pins.gpio21,
        ) {
            log::error!("Failed to init LCD (base): {:?}", e);
        }
    }

    // Create display framebuffer and UI
    let mut framebuffer = Box::new(DisplayBuffer::new(crate::ui::ColorFormat::BLACK));
    let avatar_gif: Vec<u8> = { let s = setting.lock().unwrap(); s.0.avatar_gif.0.clone() };
    let mut chat_ui = new_chat_ui::<4>(framebuffer.as_mut(), &avatar_gif)?;

    // Restore display-related NVS settings (MADCTL, GAP, ROTATION) before first flush.
    {
        let (saved_madctl, saved_gap_x, saved_gap_y, saved_rot) = {
            let s = setting.lock().unwrap();
            let mad = s.1.get_u8("madctl").ok().flatten();
            let gx = s.1.get_i32("gap_x").ok().flatten();
            let gy = s.1.get_i32("gap_y").ok().flatten();
            let rot = s.1.get_u8("disp_rot").ok().flatten();
            (mad, gx, gy, rot)
        };

        if let Some(m) = saved_madctl {
            #[cfg(feature = "esp32s3cam")]
            if let Err(e) = crate::boards::esp32s3cam::set_madctl(m) { log::error!("Failed to restore MADCTL: {:?}", e); }
            #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
            if let Err(e) = crate::boards::base::set_madctl(m) { log::error!("Failed to restore MADCTL: {:?}", e); }
            log::info!("Restored MADCTL from NVS: 0x{:02X}", m);
        }

        if saved_gap_x.is_some() || saved_gap_y.is_some() {
            let gx = saved_gap_x.unwrap_or(0);
            let gy = saved_gap_y.unwrap_or(0);
            #[cfg(feature = "esp32s3cam")]
            if let Err(e) = crate::boards::esp32s3cam::set_gap(gx, gy) { log::error!("Failed to restore gap: {:?}", e); }
            #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
            if let Err(e) = crate::boards::base::set_gap(gx, gy) { log::error!("Failed to restore gap: {:?}", e); }
            log::info!("Restored GAP from NVS: x={}, y={}", gx, gy);
        } else if let Some(rot) = saved_rot {
            // If no explicit gap stored but rotation is 1 we prefer an initial adjustment
            if rot == 1 {
                // For rotation==1 apply a default gap mapping that yields a visible Y offset on rotated panel
                #[cfg(feature = "esp32s3cam")]
                if let Err(e) = crate::boards::esp32s3cam::set_gap(80, 0) { log::error!("Failed to apply default gap for rot=1: {:?}", e); }
                #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                if let Err(e) = crate::boards::base::set_gap(80, 0) { log::error!("Failed to apply default gap for rot=1: {:?}", e); }
                log::info!("Applied default GAP for rot=1: x=80, y=0 (maps to Y=80 when swapped)");
            }
        }

        if let Some(rot) = saved_rot {
            #[cfg(feature = "esp32s3cam")]
            if let Err(e) = crate::boards::esp32s3cam::set_rotation_state(rot) { log::error!("Failed to restore rotation: {:?}", e); }
            #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
            if let Err(e) = crate::boards::base::set_rotation_state(rot) { log::error!("Failed to restore rotation: {:?}", e); }
            log::info!("Restored rotation state from NVS: {}", rot);
        }
    }

    // Single-button pin used on some boards (esp32s3cam). Create only when feature enabled.
    #[cfg(feature = "esp32s3cam")]
    let mut button = {
        let pin = peripherals.pins.gpio0;
        let mut pin_dr = esp_idf_svc::hal::gpio::PinDriver::input(pin)?;
        pin_dr.set_pull(esp_idf_svc::hal::gpio::Pull::Up)?;
        pin_dr.set_interrupt_type(esp_idf_svc::hal::gpio::InterruptType::NegEdge)?;
        pin_dr
    };

    // Track whether BLE server has been started
    let mut bt_started = false;

    framebuffer.flush()?;

    let (ssid_clone, pass_clone) = { let s = setting.lock().unwrap(); (s.0.ssid.clone(), s.0.pass.clone()) };
    let _wifi = network::wifi(&ssid_clone, &pass_clone, peripherals.modem, sysloop.clone());
    if _wifi.is_err() {
        chat_ui.set_state("Failed to connect to wifi".to_string());
        chat_ui.set_text("Press K0 to open settings".to_string());
        chat_ui.render_to_target(framebuffer.as_mut())?;
        framebuffer.flush()?;

        #[cfg(feature = "esp32s3cam")]
        {
            b.block_on(button.wait_for_falling_edge()).unwrap();
            setting.lock().unwrap().1.set_u8("state", 1).unwrap();
            unsafe { esp_idf_svc::sys::esp_restart() }
        }
        #[cfg(not(feature = "esp32s3cam"))]
        {
            // No local button available: set setup state and reboot immediately
            setting.lock().unwrap().1.set_u8("state", 1).unwrap();
            unsafe { esp_idf_svc::sys::esp_restart() }
        }
    }

    let wifi = _wifi.unwrap();

    // compute device id from WiFi MAC and ensure BLE is started so user can
    // always configure via BLE if HTTP or WiFi are unavailable
    let mac = wifi.sta_netif().get_mac().unwrap();
    let dev_id = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    if !bt_started {
        match bt::bt(&dev_id, setting.clone(), evt_tx.clone()) {
            Ok(()) => {
                bt_started = true;
                log::info!("BLE server started (device id: {})", dev_id);
            }
            Err(e) => log::error!("Failed to start BLE server: {:?}", e),
        }
    }

    // Start a lightweight HTTP settings server (port 80) so users can configure
    // the device via web UI without using BLE. Runs in its own thread with a
    // tiny Tokio runtime.
    {
        let setting_clone = setting.clone();
        let evt_tx_clone = evt_tx.clone();
        let spawn_result = std::thread::Builder::new()
            .name("http_server".to_string())
            .stack_size(32 * 1024)
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    // Use slightly smaller stack for http server runtime threads
                    .thread_stack_size(64 * 1024)
                    .build()
                    .expect("failed to build http server runtime");

                rt.block_on(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let listener = match tokio::net::TcpListener::bind("0.0.0.0:80").await {
                        Ok(l) => l,
                        Err(e) => {
                            log::error!("HTTP server bind failed: {:?}", e);
                            return;
                        }
                    };
                    log::info!("HTTP settings server listening on 0.0.0.0:80");

                    loop {
                        let (mut socket, addr) = match listener.accept().await {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!("HTTP accept error: {:?}", e);
                                continue;
                            }
                        };

                        let sc = setting_clone.clone();
                        let evt_tx_conn = evt_tx_clone.clone();
                        tokio::spawn(async move {
                            let mut header_buf = [0u8; 8192];
                            let mut read_bytes = 0usize;
                            // read until header end or buffer full
                            loop {
                                match socket.read(&mut header_buf[read_bytes..]).await {
                                    Ok(0) => return, // closed
                                    Ok(n) => {
                                        read_bytes += n;
                                        if read_bytes >= 4 && header_buf[..read_bytes].windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                        if read_bytes == header_buf.len() { break; }
                                    }
                                    Err(_) => return,
                                }
                            }

                            let header_str = String::from_utf8_lossy(&header_buf[..read_bytes]).to_string();
                            let header_end = header_str.find("\r\n\r\n").map(|i| i + 4).unwrap_or(read_bytes);

                            // parse request line
                            let mut lines = header_str.split("\r\n");
                            let req_line = lines.next().unwrap_or("");
                            let mut parts = req_line.split_whitespace();
                            let method = parts.next().unwrap_or("");
                            let path = parts.next().unwrap_or("/");

                            // parse headers for content-length
                            let mut content_length: usize = 0;
                            for line in lines {
                                if line.is_empty() { break; }
                                if let Some(rest) = line.strip_prefix("Content-Length:") {
                                    content_length = rest.trim().parse().unwrap_or(0);
                                }
                            }

                            // read body if any
                            let mut body = Vec::new();
                            if read_bytes > header_end {
                                body.extend_from_slice(&header_buf[header_end..read_bytes]);
                            }
                            while body.len() < content_length {
                                let mut tmp = vec![0u8; 1024];
                                match socket.read(&mut tmp).await {
                                    Ok(0) => break,
                                    Ok(n) => body.extend_from_slice(&tmp[..n]),
                                    Err(_) => break,
                                }
                            }

                            // Simple routing
                            if method == "GET" && (path == "/" || path == "/setup") {
                                // Minimal setup page (JS uses fetch to the JSON endpoints)
                                let html = r#"<!doctype html>
<html>
<head><meta charset='utf-8'><title>EchoKit Setup</title></head>
<body>
<h2>EchoKit Settings (HTTP)</h2>
<div id='status'></div>
<form id='settingsForm'>
SSID: <input id='ssid' /><br/>
PASS: <input id='pass' type='password' /><br/>
Server URL: <input id='server' /><br/>
MADCTL: <input id='madctl' placeholder='0x36' /><button type='button' id='applyMad'>Apply</button><br/>
GAP X: <input id='gapx' type='number' value='0' /> Y: <input id='gapy' type='number' value='0' /><button type='button' id='applyGap'>Apply GAP</button><br/>
Rotate: <input id='rot' type='number' min='0' max='3' value='0' /><button type='button' id='applyRot'>Apply</button><br/>
<button id='saveSettings' type='button'>Save SSID/PASS/Server</button>
</form>
<script>
async function load() {
  const r = await fetch('/api/settings');
  const json = await r.json();
  document.getElementById('ssid').value = json.ssid || '';
  document.getElementById('pass').value = json.pass || '';
  document.getElementById('server').value = json.server_url || '';
  document.getElementById('madctl').value = json.madctl_hex || '';
  document.getElementById('gapx').value = json.gap_x || 0;
  document.getElementById('gapy').value = json.gap_y || 0;
  document.getElementById('rot').value = json.disp_rot || 0;
}
document.getElementById('applyMad').addEventListener('click', async () => {
  const v = document.getElementById('madctl').value.trim();
  await fetch('/api/set/madctl', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({madctl:v})});
  alert('MADCTL send');
});
document.getElementById('applyGap').addEventListener('click', async () => {
  const x = parseInt(document.getElementById('gapx').value) || 0;
  const y = parseInt(document.getElementById('gapy').value) || 0;
  await fetch('/api/set/gap', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({x:x,y:y})});
  alert('GAP send');
});
document.getElementById('applyRot').addEventListener('click', async () => {
  const r = parseInt(document.getElementById('rot').value) || 0;
  await fetch('/api/set/rotate', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({rot:r})});
  alert('Rotate send');
});
document.getElementById('saveSettings').addEventListener('click', async () => {
  const ssid = document.getElementById('ssid').value || '';
  const pass = document.getElementById('pass').value || '';
  const server = document.getElementById('server').value || '';
  await fetch('/api/set/settings', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ssid:ssid, pass:pass, server_url:server})});
  alert('Settings saved (may require reboot to apply)');
});
load();
</script>
</body>
</html>"#;

                                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}", html.len(), html);
                                let _ = socket.write_all(resp.as_bytes()).await;
                                return;
                            }

                            if method == "GET" && path == "/api/settings" {
                                // Acquire the lock, clone needed fields, then drop the lock
                                let (ssid, pass, server_url, madctl, gap_x, gap_y) = {
                                    let s = sc.lock().unwrap();
                                    (
                                        s.0.ssid.clone(),
                                        s.0.pass.clone(),
                                        s.0.server_url.clone(),
                                        s.1.get_u8("madctl").ok().flatten(),
                                        s.1.get_i32("gap_x").ok().flatten().unwrap_or(0),
                                        s.1.get_i32("gap_y").ok().flatten().unwrap_or(0),
                                    )
                                };

                                let rot_state = {
                                    #[cfg(feature = "esp32s3cam")]
                                    { crate::boards::esp32s3cam::get_rotation_state() }
                                    #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                                    { crate::boards::base::get_rotation_state() }
                                    #[cfg(not(any(feature = "esp32s3cam", all(feature = "boards", not(feature = "_no_default")))))]
                                    { 0u8 }
                                };

                                let resp_json = serde_json::json!({
                                    "ssid": ssid,
                                    "pass": pass,
                                    "server_url": server_url,
                                    "madctl": madctl,
                                    "madctl_hex": madctl.map(|m| format!("0x{:02X}", m)),
                                    "gap_x": gap_x,
                                    "gap_y": gap_y,
                                    "disp_rot": rot_state
                                });
                                let body = serde_json::to_string(&resp_json).unwrap_or_else(|_| "{}".to_string());
                                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                                let _ = socket.write_all(resp.as_bytes()).await;
                                return;
                            }

                            if method == "POST" && path.starts_with("/api/set/") {
                                let body_text = String::from_utf8_lossy(&body).to_string();
                                // parse JSON payload if possible
                                let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body);

                                match path {
                                    "/api/set/madctl" => {
                                        if let Ok(json) = parsed {
                                            if let Some(v) = json.get("madctl") {
                                                if let Some(s) = v.as_str() {
                                                    let s_trim = s.trim().strip_prefix("0x").unwrap_or(s.trim());
                                                    if let Ok(val) = u8::from_str_radix(s_trim, 16) {
                                                        // apply to board and save to NVS
                                                        #[cfg(feature = "esp32s3cam")]
                                                        match crate::boards::esp32s3cam::set_madctl(val) {
                                                            Ok(()) => {},
                                                            Err(e) => log::error!("Failed to set MADCTL (esp32s3cam): {:?}", e),
                                                        }
                                                        #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                                                        match crate::boards::base::set_madctl(val) {
                                                            Ok(()) => {},
                                                            Err(e) => log::error!("Failed to set MADCTL (base): {:?}", e),
                                                        }

                                                        let mut s = sc.lock().unwrap();
                                                        if let Err(e) = s.1.set_u8("madctl", val) {
                                                            log::error!("Failed to save MADCTL to NVS: {:?}", e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    "/api/set/gap" => {
                                        if let Ok(json) = parsed {
                                            let x = json.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                            let y = json.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                            #[cfg(feature = "esp32s3cam")]
                                            match crate::boards::esp32s3cam::set_gap(x, y) {
                                                Ok(()) => {},
                                                Err(e) => log::error!("Failed to set gap (esp32s3cam): {:?}", e),
                                            }
                                            #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                                            match crate::boards::base::set_gap(x, y) {
                                                Ok(()) => {},
                                                Err(e) => log::error!("Failed to set gap (base): {:?}", e),
                                            }
                                            let mut s = sc.lock().unwrap();
                                            if let Err(e) = s.1.set_i32("gap_x", x) { log::error!("Failed to save gap_x: {:?}", e); }
                                            if let Err(e) = s.1.set_i32("gap_y", y) { log::error!("Failed to save gap_y: {:?}", e); }
                                        }
                                    }
                                    "/api/set/rotate" => {
                                        if let Ok(json) = parsed {
                                            if let Some(r) = json.get("rot").and_then(|v| v.as_i64()) {
                                                let n = (r as u8) % 4;
                                                #[cfg(feature = "esp32s3cam")]
                                                if let Err(e) = crate::boards::esp32s3cam::set_rotation_state(n) { log::error!("Failed to set rotation (esp32s3cam): {:?}", e); }
                                                #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                                                if let Err(e) = crate::boards::base::set_rotation_state(n) { log::error!("Failed to set rotation (base): {:?}", e); }
                                                {
                                                    let mut s = sc.lock().unwrap();
                                                    if let Err(e) = s.1.set_u8("disp_rot", n) {
                                                        log::error!("Failed to save disp_rot: {:?}", e);
                                                    }
                                                }

                                                // If no gap values stored and rotation==1, apply and persist a default
                                                let need_default_gap = {
                                                    let s = sc.lock().unwrap();
                                                    let gx = s.1.get_i32("gap_x").ok().flatten();
                                                    let gy = s.1.get_i32("gap_y").ok().flatten();
                                                    gx.is_none() && gy.is_none() && n == 1
                                                };
                                                if need_default_gap {
                                                    // Persist default logical gap (maps to applied Y=80 when swapped)
                                                    {
                                                        let mut s = sc.lock().unwrap();
                                                        if let Err(e) = s.1.set_i32("gap_x", 80) { log::error!("Failed to save default gap_x: {:?}", e); }
                                                        if let Err(e) = s.1.set_i32("gap_y", 5) { log::error!("Failed to save default gap_y: {:?}", e); }
                                                    }
                                                    #[cfg(feature = "esp32s3cam")]
                                                    if let Err(e) = crate::boards::esp32s3cam::set_gap(80, 0) { log::error!("Failed to apply default gap for rot=1: {:?}", e); }
                                                    #[cfg(all(not(feature = "esp32s3cam"), feature = "boards", not(feature = "_no_default")))]
                                                    if let Err(e) = crate::boards::base::set_gap(80, 0) { log::error!("Failed to apply default gap for rot=1: {:?}", e); }
                                                }

                                                // Notify main UI loop to re-render and flush with the new orientation
                                                if let Err(e) = evt_tx_conn.send(crate::app::Event::Redraw).await {
                                                    log::error!("Failed to enqueue Redraw event from HTTP handler: {:?}", e);
                                                }
                                            }
                                        }
                                    }
                                    "/api/set/settings" => {
                                        if let Ok(json) = parsed {
                                            let ssid = json.get("ssid").and_then(|v| v.as_str()).unwrap_or("");
                                            let pass = json.get("pass").and_then(|v| v.as_str()).unwrap_or("");
                                            let server_url = json.get("server_url").and_then(|v| v.as_str()).unwrap_or("");
                                            let mut s = sc.lock().unwrap();
                                            if let Err(e) = s.1.set_str("ssid", ssid) { log::error!("Failed to save ssid: {:?}", e); }
                                            if let Err(e) = s.1.set_str("pass", pass) { log::error!("Failed to save pass: {:?}", e); }
                                            if let Err(e) = s.1.set_str("server_url", server_url) { log::error!("Failed to save server_url: {:?}", e); }
                                            // update in-memory copy too
                                            s.0.ssid = ssid.to_string();
                                            s.0.pass = pass.to_string();
                                            s.0.server_url = server_url.to_string();
                                        }
                                    }
                                    _ => {}
                                }

                                let ok = "{\"ok\":true}";
                                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", ok.len(), ok);
                                let _ = socket.write_all(resp.as_bytes()).await;
                                return;
                            }

                            // default: 404
                            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
                            let _ = socket.write_all(resp.as_bytes()).await;
                        });
                    }
                });
            });

        if let Err(e) = spawn_result {
            log::error!("Failed to spawn http server thread: {:?}", e);
        }
    }

    chat_ui.set_state("Connecting to server...".to_string());
    chat_ui.set_text("".to_string());
    chat_ui.render_to_target(framebuffer.as_mut())?;
    framebuffer.flush()?;

    log_heap();

    chat_ui.set_state("Failed to connect to server".to_string());
    let server_url_clone = { let s = setting.lock().unwrap(); s.0.server_url.clone() };
    chat_ui.set_text(format!(
        "Please check your server URL: {}\nPress K0 to open settings",
        server_url_clone
    ));

    // Resolve hostname -> IP synchronously to avoid Tokio spawning blocking resolver threads
    // (which can fail on constrained systems by creating pthreads).
    let server_url_resolved = (|| {
        // Simple parse for ws:// or wss://
        let url = server_url_clone.as_str();
        if let Some(rest) = url.strip_prefix("ws://") {
            let scheme = "ws";
            let default_port = 80u16;
            let slash_pos = rest.find('/').unwrap_or(rest.len());
            let host_port = &rest[..slash_pos];
            let path = &rest[slash_pos..];
            let (host, port) = if let Some(colon_pos) = host_port.rfind(':') {
                let h = &host_port[..colon_pos];
                if let Ok(p) = host_port[colon_pos+1..].parse::<u16>() { (h, p) } else { (host_port, default_port) }
            } else {
                (host_port, default_port)
            };
            match (host, port).to_socket_addrs() {
                Ok(mut iter) => {
                    if let Some(sock) = iter.next() {
                        let ip = sock.ip();
                        let port = sock.port();
                        return format!("{}://{}:{}{}", scheme, ip, port, path);
                    }
                }
                Err(_) => {}
            }
            return server_url_clone.clone();
        } else if let Some(rest) = server_url_clone.as_str().strip_prefix("wss://") {
            let scheme = "wss";
            let default_port = 443u16;
            let slash_pos = rest.find('/').unwrap_or(rest.len());
            let host_port = &rest[..slash_pos];
            let path = &rest[slash_pos..];
            let (host, port) = if let Some(colon_pos) = host_port.rfind(':') {
                let h = &host_port[..colon_pos];
                if let Ok(p) = host_port[colon_pos+1..].parse::<u16>() { (h, p) } else { (host_port, default_port) }
            } else {
                (host_port, default_port)
            };
            match (host, port).to_socket_addrs() {
                Ok(mut iter) => {
                    if let Some(sock) = iter.next() {
                        let ip = sock.ip();
                        let port = sock.port();
                        return format!("{}://{}:{}{}", scheme, ip, port, path);
                    }
                }
                Err(_) => {}
            }
            return server_url_clone.clone();
        }
        server_url_clone.clone()
    })();

    let server = b.block_on(ws::Server::new(dev_id, server_url_resolved));
    if server.is_err() {
        log::info!("Failed to connect to server: {:?}", server.err());
        chat_ui.render_to_target(framebuffer.as_mut())?;
        framebuffer.flush()?;
        #[cfg(feature = "esp32s3cam")]
        {
            b.block_on(button.wait_for_falling_edge()).unwrap();
            setting.lock().unwrap().1.set_u8("state", 1).unwrap();
            unsafe { esp_idf_svc::sys::esp_restart() }
        }
        #[cfg(not(feature = "esp32s3cam"))]
        {
            // No local button available: set setup state and reboot immediately
            setting.lock().unwrap().1.set_u8("state", 1).unwrap();
            unsafe { esp_idf_svc::sys::esp_restart() }
        }
    }

    let server = server.unwrap();

    // 1. CHIAMATA DIRETTA AUDIO (board-specific signatures)
    #[cfg(feature = "box")]
    {
        crate::boards::start_audio_workers(
            peripherals.i2s0,
            peripherals.pins.gpio21, // BCLK
            peripherals.pins.gpio47, // DIN
            peripherals.pins.gpio14, // DOUT
            peripherals.pins.gpio13, // WS
            rx1,
            evt_tx.clone(),
        )?;
    }

    #[cfg(all(feature = "boards", not(feature = "_no_default"), not(feature = "cube"), not(feature = "cube2")))]
    {
        crate::boards::start_audio_workers(
            peripherals.i2s1,
            peripherals.pins.gpio14, // out_clk (BCLK)
            peripherals.pins.gpio46, // out_ws (LRC)
            peripherals.pins.gpio41, // dout
            peripherals.i2s0,
            peripherals.pins.gpio2,  // in_clk (SCK)
            peripherals.pins.gpio1,  // in_ws (WS)
            peripherals.pins.gpio42, // din (SD)
            rx1,
            evt_tx.clone(),
        )?;
    }

    #[cfg(feature = "cube")]
    {
        crate::boards::start_audio_workers(
            peripherals.i2s1,
            peripherals.pins.gpio5,
            peripherals.pins.gpio6,
            peripherals.pins.gpio7,
            peripherals.i2s0,
            peripherals.pins.gpio4,
            peripherals.pins.gpio15,
            peripherals.pins.gpio16,
            rx1,
            evt_tx.clone(),
        )?;
    }

    #[cfg(feature = "cube2")]
    {
        crate::boards::start_audio_workers(
            peripherals.i2s1,
            peripherals.pins.gpio14,
            peripherals.pins.gpio46,
            peripherals.pins.gpio41,
            peripherals.i2s0,
            peripherals.pins.gpio2,
            peripherals.pins.gpio1,
            peripherals.pins.gpio42,
            rx1,
            evt_tx.clone(),
        )?;
    }

    #[cfg(feature = "esp32s3cam")]
    {
        crate::boards::start_audio_workers(
            peripherals.i2s1,
            peripherals.pins.gpio14,
            peripherals.pins.gpio46,
            peripherals.pins.gpio41,
            peripherals.i2s0,
            peripherals.pins.gpio2,
            peripherals.pins.gpio1,
            peripherals.pins.gpio42,
            rx1,
            evt_tx.clone(),
        )?;
    }

    // 2. CHIAMATA DIRETTA PER I PULSANTI (board-specific signatures)
    #[cfg(feature = "box")]
    {
        crate::boards::start_btn_worker(&b, peripherals.pins.gpio3, evt_tx.clone())?;
    }

    #[cfg(all(feature = "boards", not(feature = "_no_default"), not(feature = "esp32s3cam"), not(feature = "cube"), not(feature = "cube2")))]
    {
        crate::boards::start_btn_worker(
            &b,
            peripherals.pins.gpio9,
            peripherals.pins.gpio3,
            evt_tx.clone(),
        )?;
    }

    #[cfg(feature = "cube")]
    {
        crate::boards::start_btn_worker(&b, peripherals.pins.gpio10, peripherals.pins.gpio39, evt_tx.clone())?;
    }

    #[cfg(feature = "cube2")]
    {
        crate::boards::start_btn_worker(&b, peripherals.pins.gpio40, peripherals.pins.gpio39, evt_tx.clone())?;
    }

    #[cfg(feature = "esp32s3cam")]
    {
        // gpio0 is consumed by the local `button` PinDriver in main.rs; do not call
        // the board helper which would take ownership again and cause a move error.
        // The main code already handles the button events using `button`.
    }

    let ws_task = app::main_work(server, tx1, evt_rx, &mut framebuffer, &mut chat_ui);

    #[cfg(feature = "esp32s3cam")]
    {
        b.spawn(async move {
            loop {
                let _ = button.wait_for_falling_edge().await;
                log::info!("Button k0 pressed {:?}", button.get_level());

                let r = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    button.wait_for_rising_edge(),
                )
                .await;
                match r {
                    Ok(_) => {
                        if evt_tx
                            .send(app::Event::Event(app::Event::K0))
                            .await
                            .is_err()
                        {
                            log::error!("Failed to send K0 event");
                            break;
                        }
                    }
                    Err(_) => {
                        if evt_tx
                            .send(app::Event::Event(app::Event::K0_))
                            .await
                            .is_err()
                        {
                            log::error!("Failed to send K0 event");
                            break;
                        }
                    }
                }
            }
        });
    }

    b.block_on(async move {
        let r = ws_task.await;
        if let Err(e) = r {
            log::error!("Error: {:?}", e);
        } else {
            log::info!("WebSocket task finished successfully");
        }
    });
    log::error!("WebSocket task finished");
    unsafe { esp_idf_svc::sys::esp_restart() }
}

pub fn log_heap() {
    unsafe {
        use esp_idf_svc::sys::{heap_caps_get_free_size, MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM};

        log::info!(
            "Free SPIRAM heap size: {}KB",
            heap_caps_get_free_size(MALLOC_CAP_SPIRAM) / 1024
        );
        log::info!(
            "Free INTERNAL heap size: {}KB",
            heap_caps_get_free_size(MALLOC_CAP_INTERNAL) / 1024
        );
    }
}

fn print_stack_high() {
    let stack_high =
        unsafe { esp_idf_svc::sys::uxTaskGetStackHighWaterMark2(std::ptr::null_mut()) };
    log::info!("Stack high: {}", stack_high);
}
