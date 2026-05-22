use anyhow::{anyhow, Result};

/// Minimal RMT-based WS2812 (NeoPixel) helper for a single pixel (GRB).
/// Uses the ESP-IDF RMT API (raw bindings) to send a 24-bit GRB frame.
/// Note: GPIO used must not be driven by another peripheral at the same time.
pub fn write_pixel(gpio_num: i32, r: u8, g: u8, b: u8) -> Result<()> {
    unsafe {
        use esp_idf_svc::sys as sys;

        // Prepare TX config
        let mut tx_cfg = sys::rmt_tx_config_t::default();
        tx_cfg.carrier_freq_hz = 0; // no carrier
        tx_cfg.carrier_level = sys::rmt_carrier_level_t_RMT_CARRIER_LEVEL_LOW;
        tx_cfg.idle_level = sys::rmt_idle_level_t_RMT_IDLE_LEVEL_LOW;
        tx_cfg.carrier_duty_percent = 0;
        tx_cfg.loop_count = 0;
        tx_cfg.carrier_en = false;
        tx_cfg.loop_en = false;
        tx_cfg.idle_output_en = true;

        let mut cfg = sys::rmt_config_t::default();
        cfg.rmt_mode = sys::rmt_mode_t_RMT_MODE_TX;
        cfg.channel = sys::rmt_channel_t_RMT_CHANNEL_0; // use channel 0
        cfg.gpio_num = gpio_num as sys::gpio_num_t;
        // APB ~80MHz; clk_div=8 -> 10MHz RMT tick => 100ns per tick
        cfg.clk_div = 8;
        cfg.mem_block_num = 1;
        cfg.flags = 0;
        cfg.__bindgen_anon_1.tx_config = tx_cfg;

        let rc = sys::rmt_config(&cfg);
        if rc != 0 {
            return Err(anyhow!("rmt_config failed: {}", rc));
        }
        ::log::info!("rmt_config ok (channel={})", cfg.channel);

        let rc = sys::rmt_set_gpio(cfg.channel, cfg.rmt_mode, cfg.gpio_num, false);
        if rc != 0 {
            return Err(anyhow!("rmt_set_gpio failed: {}", rc));
        }
        ::log::info!("rmt_set_gpio ok (gpio={})", gpio_num);

        // Timing in ticks @10MHz (100ns per tick)
        let t0h = 4u32; // ~400ns
        let t0l = 9u32; // ~900ns
        let t1h = 8u32; // ~800ns
        let t1l = 5u32; // ~500ns

        // GRB order for WS2812
        let bytes = [g, r, b];
        let mut items: Vec<sys::rmt_item32_t> = Vec::with_capacity(24);

        for byte in &bytes {
            for bit in (0..8).rev() {
                let bit_set = ((*byte >> bit) & 1) != 0;
                let (d0, _l0, d1, _l1) = if bit_set {
                    (t1h, 1u32, t1l, 0u32)
                } else {
                    (t0h, 1u32, t0l, 0u32)
                };

                // Build rmt_item with durations/levels
                let mut inner = sys::rmt_item32_t__bindgen_ty_1__bindgen_ty_1::default();
                inner._bitfield_1 =
                    sys::rmt_item32_t__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(d0, 1, d1, 0);

                let union = sys::rmt_item32_t__bindgen_ty_1 { __bindgen_anon_1: inner };
                let item = sys::rmt_item32_t { __bindgen_anon_1: union };
                items.push(item);
            }
        }

        // Install legacy RMT driver for this channel, then send the items (blocking)
        let rc = sys::rmt_driver_install(cfg.channel, 0, 0);
        if rc != 0 {
            return Err(anyhow!("rmt_driver_install failed: {}", rc));
        }
        ::log::info!("rmt_driver_install ok (channel={})", cfg.channel);

        let ret = sys::rmt_write_items(cfg.channel, items.as_ptr(), items.len() as i32, true);
        if ret != 0 {
            // Cleanup driver on failure
            let _ = sys::rmt_driver_uninstall(cfg.channel);
            return Err(anyhow!("rmt_write_items failed: {}", ret));
        }
        ::log::info!("rmt_write_items ok (items={})", items.len());

        // Latch time > 50us
        std::thread::sleep(std::time::Duration::from_millis(1));

        // Uninstall driver to free resources; remove this if you plan to write often.
        let _ = sys::rmt_driver_uninstall(cfg.channel);
    }

    Ok(())
}
