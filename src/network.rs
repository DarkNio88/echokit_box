use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::peripheral,
    http::{client::EspHttpConnection, Method},
    wifi::{AuthMethod, BlockingWifi, EspWifi},
};
use log::info;

pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
) -> anyhow::Result<Box<EspWifi<'static>>> {
    let mut auth_method = AuthMethod::WPA2Personal;
    if ssid.is_empty() {
        anyhow::bail!("Missing WiFi name")
    }
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        info!("Wifi password is empty");
    }
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;

    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&esp_idf_svc::wifi::Configuration::Client(
        esp_idf_svc::wifi::ClientConfiguration {
            ssid: ssid
                .try_into()
                .expect("Could not parse the given SSID into WiFi config"),
            password: pass
                .try_into()
                .expect("Could not parse the given password into WiFi config"),
            auth_method,
            ..Default::default()
        },
    ))?;

    wifi.start()?;

    info!("Connecting wifi...");

    wifi.connect()?;

    info!("Waiting for DHCP lease...");

    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}

#[allow(unused)]
pub fn http_get(url: &str) -> anyhow::Result<EspHttpConnection> {
    let configuration = esp_idf_svc::http::client::Configuration::default();
    let mut conn = EspHttpConnection::new(&configuration)?;
    conn.initiate_request(Method::Get, url, &[])?;

    conn.initiate_response()?;

    Ok(conn)
}

#[allow(unused)]
pub fn http_post(url: &str, data: &[u8]) -> anyhow::Result<EspHttpConnection> {
    let configuration = esp_idf_svc::http::client::Configuration::default();
    let len = data.len().to_string();
    let mut conn = EspHttpConnection::new(&configuration)?;
    conn.initiate_request(Method::Post, url, &[("Content-Length", &len)])?;

    let mut offset = 0;

    while offset < data.len() {
        offset += conn.write(&data[offset..])?;
        log::info!("Wrote {} bytes", offset);
    }

    conn.initiate_response()?;

    Ok(conn)
}

pub fn ota_update_from_url(url: &str) -> anyhow::Result<()> {
    use std::{ffi::CStr, ptr};

    log::info!("Starting OTA from URL: {}", url);

    let mut conn = http_get(url)?;

    // Find the next OTA partition
    let update_partition = unsafe { esp_idf_svc::sys::esp_ota_get_next_update_partition(ptr::null()) };
    if update_partition.is_null() {
        anyhow::bail!("No OTA partition available");
    }

    // Begin OTA (size unknown / sequential writes)
    let mut update_handle: esp_idf_svc::sys::esp_ota_handle_t = 0;
    let res = unsafe {
        esp_idf_svc::sys::esp_ota_begin(
            update_partition,
            esp_idf_svc::sys::OTA_WITH_SEQUENTIAL_WRITES as usize,
            &mut update_handle,
        )
    };

    if res != esp_idf_svc::sys::ESP_OK {
        let name = unsafe { CStr::from_ptr(esp_idf_svc::sys::esp_err_to_name(res)) }
            .to_string_lossy()
            .into_owned();
        anyhow::bail!("esp_ota_begin failed: {} ({})", res, name);
    }

    // Stream the HTTP body into the OTA partition
    let mut buf = [0u8; 4096];

    loop {
        match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let write_err = unsafe { esp_idf_svc::sys::esp_ota_write(update_handle, buf.as_ptr() as *const _, n) };
                if write_err != esp_idf_svc::sys::ESP_OK {
                    unsafe { esp_idf_svc::sys::esp_ota_abort(update_handle) };
                    let name = unsafe { CStr::from_ptr(esp_idf_svc::sys::esp_err_to_name(write_err)) }
                        .to_string_lossy()
                        .into_owned();
                    anyhow::bail!("esp_ota_write failed: {} ({})", write_err, name);
                }
            }
            Err(e) => {
                unsafe { esp_idf_svc::sys::esp_ota_abort(update_handle) };
                anyhow::bail!("HTTP read error during OTA: {:?}", e);
            }
        }
    }

    // Finalize OTA
    let end_res = unsafe { esp_idf_svc::sys::esp_ota_end(update_handle) };
    if end_res != esp_idf_svc::sys::ESP_OK {
        let name = unsafe { CStr::from_ptr(esp_idf_svc::sys::esp_err_to_name(end_res)) }
            .to_string_lossy()
            .into_owned();
        anyhow::bail!("esp_ota_end failed: {} ({})", end_res, name);
    }

    let set_res = unsafe { esp_idf_svc::sys::esp_ota_set_boot_partition(update_partition) };
    if set_res != esp_idf_svc::sys::ESP_OK {
        let name = unsafe { CStr::from_ptr(esp_idf_svc::sys::esp_err_to_name(set_res)) }
            .to_string_lossy()
            .into_owned();
        anyhow::bail!("esp_ota_set_boot_partition failed: {} ({})", set_res, name);
    }

    log::info!("OTA write complete; new partition configured. Reboot to apply.");

    Ok(())
}
