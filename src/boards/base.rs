use esp_idf_svc::{
    hal::{
        gpio::*,
        i2s::{I2S0, I2S1},
        spi::SPI3,
    },
    sys::EspError,
};

const AUDIO_STACK_SIZE: usize = 15 * 1024;
pub const AFE_AEC_OFFSET: usize = 256;

pub fn afe_config(afe_config: &mut esp_idf_svc::sys::esp_sr::afe_config_t) {
    afe_config.agc_init = true;
    afe_config.agc_mode = esp_idf_svc::sys::esp_sr::afe_agc_mode_t_AFE_AGC_MODE_WEBRTC;
}

// 🔊 Ingressi e Uscite Audio mappati SOLO sui pin audio dedicati della tua board
pub fn start_audio_workers(
    out_i2s: I2S1,
    out_clk: Gpio14,  // BCLK Amplificatore -> Corretto
    out_ws: Gpio46,   // LRC Amplificatore -> Corretto
    dout: Gpio41,     // DIN Amplificatore (Condiviso con cautela o gestito via hardware)
    in_i2s: I2S0,
    in_clk: Gpio2,   // SCK Microfono -> Corretto
    in_ws: Gpio1,    // WS Microfono -> Corretto
    din: Gpio42,       // SD Microfono -> Corretto
    rx: crate::audio::PlayerRx,
    tx: crate::audio::EventTx,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let worker = crate::audio::BoardsAudioWorker {
        out_i2s,
        out_ws: out_ws.into(),
        out_clk: out_clk.into(),
        dout: dout.into(),
        out_mclk: None,

        in_i2s,
        in_ws: in_ws.into(),
        in_clk: in_clk.into(),
        din: din.into(),
        in_mclk: None,
    };

    let r = std::thread::Builder::new()
        .stack_size(AUDIO_STACK_SIZE)
        .spawn(move || {
            ::log::info!(
                "Starting audio worker thread in core {:?}",
                esp_idf_svc::hal::cpu::core()
            );
            let r = worker.run(rx, tx);
            if let Err(e) = r {
                ::log::error!("Audio worker error: {:?}", e);
            }
        })
        .map_err(|e| anyhow::anyhow!("Failed to spawn audio worker thread: {:?}", e))?;

    Ok(r)
}

// 🔘 Pulsanti fisici Xiaozhi (POW = GPIO 9, Wake = GPIO 3)
pub fn start_btn_worker(
    rt: &tokio::runtime::Runtime,
    vol_up_btn: Gpio9,       
    vol_down_btn: Gpio3,     
    evt_tx: crate::audio::EventTx,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let mut vol_up_btn = esp_idf_svc::hal::gpio::PinDriver::input(vol_up_btn)?;
    vol_up_btn.set_pull(esp_idf_svc::hal::gpio::Pull::Up)?;
    vol_up_btn.set_interrupt_type(esp_idf_svc::hal::gpio::InterruptType::PosEdge)?;

    let mut vol_down_btn = esp_idf_svc::hal::gpio::PinDriver::input(vol_down_btn)?;
    vol_down_btn.set_pull(esp_idf_svc::hal::gpio::Pull::Up)?;
    vol_down_btn.set_interrupt_type(esp_idf_svc::hal::gpio::InterruptType::PosEdge)?;

    Ok(rt.spawn(async move {
        loop {
            tokio::select! {
                _ = vol_up_btn.wait_for_falling_edge() => {
                    ::log::info!("Volume up button pressed");
                    let r = evt_tx.send(crate::app::Event::Event(crate::app::Event::VOL_UP)).await;
                    if let Err(e) = r {
                        ::log::error!("Failed to send volume up event: {:?}", e);
                    }
                }
                _ = vol_down_btn.wait_for_falling_edge() => {
                    ::log::info!("Volume down button pressed");
                    let r = evt_tx.send(crate::app::Event::Event(crate::app::Event::VOL_DOWN)).await;
                    if let Err(e) = r {
                        ::log::error!("Failed to send volume down event: {:?}", e);
                    }
                }
            }
        }
    }))
}

pub const DISPLAY_WIDTH: usize = 240;
pub const DISPLAY_HEIGHT: usize = 240;

static mut ESP_LCD_PANEL_HANDLE: esp_idf_svc::sys::esp_lcd_panel_handle_t = std::ptr::null_mut();
static mut ESP_LCD_PANEL_IO_HANDLE: esp_idf_svc::sys::esp_lcd_panel_io_handle_t = std::ptr::null_mut();

// Rotation state: 0=0°, 1=90°, 2=180°, 3=270°
static mut DISPLAY_ROTATION_STATE: u8 = 0;
// Stored logical gap values (as set by user / NVS). We reapply these taking rotation into account.
static mut PANEL_GAP_X: i32 = 0;
static mut PANEL_GAP_Y: i32 = 0;

unsafe fn apply_stored_gap() -> Result<(), esp_idf_svc::sys::EspError> {
    use esp_idf_svc::sys::*;
    if ESP_LCD_PANEL_HANDLE.is_null() {
        ::log::warn!("apply_stored_gap: panel handle is null");
        return Ok(());
    }

    // When axes are swapped by rotation (state 1 or 3), swap gap values
    let swap = matches!(DISPLAY_ROTATION_STATE, 1 | 3);
    let (applied_x, applied_y) = if swap {
        (PANEL_GAP_Y, PANEL_GAP_X)
    } else {
        (PANEL_GAP_X, PANEL_GAP_Y)
    };

    ::log::info!("apply_stored_gap: applying gap x={}, y={} (swap={})", applied_x, applied_y, swap);
    esp!(esp_lcd_panel_set_gap(ESP_LCD_PANEL_HANDLE, applied_x, applied_y))?;
    Ok(())
}

/// Rotate the panel 90 degrees clockwise (to the right).
pub fn rotate_right() -> Result<(), EspError> {
    use esp_idf_svc::sys::*;
    unsafe {
        if ESP_LCD_PANEL_HANDLE.is_null() {
            ::log::error!("rotate_right: panel handle is null");
            return Ok(());
        }

        DISPLAY_ROTATION_STATE = (DISPLAY_ROTATION_STATE + 1) % 4;
        apply_rotation_state(DISPLAY_ROTATION_STATE)?;
        ::log::info!("rotate_right: rotation state now {}", DISPLAY_ROTATION_STATE);
    }
    Ok(())
}

/// Rotate the panel 90 degrees counter-clockwise (to the left).
pub fn rotate_left() -> Result<(), EspError> {
    use esp_idf_svc::sys::*;
    unsafe {
        if ESP_LCD_PANEL_HANDLE.is_null() {
            ::log::error!("rotate_left: panel handle is null");
            return Ok(());
        }

        DISPLAY_ROTATION_STATE = (DISPLAY_ROTATION_STATE + 3) % 4; // -1 mod 4
        apply_rotation_state(DISPLAY_ROTATION_STATE)?;
        ::log::info!("rotate_left: rotation state now {}", DISPLAY_ROTATION_STATE);
    }
    Ok(())
}

/// Return the current rotation state (0..3)
pub fn get_rotation_state() -> u8 {
    unsafe { DISPLAY_ROTATION_STATE }
}

/// Set rotation state directly (0..3) and apply it to the panel
pub fn set_rotation_state(state: u8) -> Result<(), EspError> {
    use esp_idf_svc::sys::*;
    unsafe {
        if ESP_LCD_PANEL_HANDLE.is_null() {
            ::log::error!("set_rotation_state: panel handle is null");
            return Ok(());
        }
        DISPLAY_ROTATION_STATE = state % 4;
        apply_rotation_state(DISPLAY_ROTATION_STATE)?;
        ::log::info!("set_rotation_state: rotation state now {}", DISPLAY_ROTATION_STATE);
    }
    Ok(())
}

unsafe fn apply_rotation_state(state: u8) -> Result<(), EspError> {
    use esp_idf_svc::sys::*;
    let panel = ESP_LCD_PANEL_HANDLE;

    // Compute swap/mirror from state
    // Alternate mapping to match panel MADCTL on some variants:
    // state 0: swap=false, mx=false, my=false
    // state 1 (90°): swap=true, mx=false, my=true
    // state 2 (180°): swap=false, mx=true, my=true
    // state 3 (270°): swap=true, mx=true, my=false
    let swap = matches!(state, 1 | 3);
    let mx = matches!(state, 2 | 3);
    let my = matches!(state, 1 | 2);

    ::log::info!("apply_rotation_state: state={} swap={} mx={} my={}", state, swap, mx, my);

    // Apply using esp-lcd helper calls which update MADCTL internally
    esp!(esp_lcd_panel_swap_xy(panel, swap))?;
    esp!(esp_lcd_panel_mirror(panel, mx, my))?;

    // Reapply stored gap values for current rotation
    if let Err(e) = apply_stored_gap() {
        ::log::warn!("apply_rotation_state: failed to reapply stored gap: {:?}", e);
    }

    Ok(())
}

// 📺 Inizializzazione SPI per lo schermo Xiaozhi (SDA=20, SCL=19)
pub fn init_spi(_spi: SPI3, mosi: Gpio20, clk: Gpio19) -> Result<(), EspError> {
    use esp_idf_svc::hal::spi::Spi;
    use esp_idf_svc::sys::*;
    const GPIO_NUM_NC: i32 = -1;

    let mut buscfg = spi_bus_config_t::default();
    buscfg.__bindgen_anon_1.mosi_io_num = mosi.pin();
    buscfg.__bindgen_anon_2.miso_io_num = GPIO_NUM_NC;
    buscfg.sclk_io_num = clk.pin();
    buscfg.__bindgen_anon_3.quadwp_io_num = GPIO_NUM_NC;
    buscfg.__bindgen_anon_4.quadhd_io_num = GPIO_NUM_NC;
    buscfg.max_transfer_sz = (DISPLAY_WIDTH * DISPLAY_HEIGHT * std::mem::size_of::<u16>()) as i32;
    esp!(unsafe { spi_bus_initialize(SPI3::device(), &buscfg, spi_common_dma_t_SPI_DMA_CH_AUTO,) })
}

// 📺 Inizializzazione linee di controllo Schermo (CS=45, DC=47, RST=21)
pub fn init_lcd(cs: Gpio45, dc: Gpio47, rst: Gpio21) -> Result<(), EspError> {
    use esp_idf_svc::sys::*;

    let mut panel_io: esp_lcd_panel_io_handle_t = std::ptr::null_mut();
    let mut io_config = esp_lcd_panel_io_spi_config_t::default();
    io_config.cs_gpio_num = cs.pin();
    io_config.dc_gpio_num = dc.pin();
    // Use a conservative pixel clock and queue depth; try SPI mode 3 for some panels
    io_config.spi_mode = 3;
    // Default to 10MHz for ST7789; if you see noise, lower to 5MHz
    io_config.pclk_hz = 10 * 1000 * 1000;
    io_config.trans_queue_depth = 20;
    io_config.lcd_cmd_bits = 8;
    io_config.lcd_param_bits = 8;

    // Save reset GPIO number now (we will not move `rst`) and toggle via sys calls
    let rst_gpio_num = rst.pin();

    // Manual reset before creating panel IO: toggle reset pin using sys API (avoids moving `rst`)
    {
        use std::time::Duration;
        use std::thread::sleep;
        ::log::info!("Manual reset BEFORE panel IO: toggling reset GPIO via sys calls");
        let rst_num = rst_gpio_num as _;
        unsafe {
            esp!(gpio_set_direction(rst_num, gpio_mode_t_GPIO_MODE_OUTPUT))?;
            esp!(gpio_set_level(rst_num, 0))?;
        }
        sleep(Duration::from_millis(50));
        unsafe { esp!(gpio_set_level(rst_num, 1))?; }
        sleep(Duration::from_millis(120));
        ::log::info!("Manual reset BEFORE panel IO done");
    }

    // create panel io (SPI)
    // DEBUG flag: set to false to enable panel initialization (was used during isolation)
    const DEBUG_SKIP_PANEL_INIT: bool = false;
    if DEBUG_SKIP_PANEL_INIT {
        ::log::warn!("DEBUG: skipping panel IO and panel creation (DEBUG_SKIP_PANEL_INIT=true)");
        return Ok(());
    }

    ::log::info!("Creating panel IO (esp_lcd_new_panel_io_spi)...");
    esp!(unsafe {
        esp_lcd_new_panel_io_spi(spi_host_device_t_SPI3_HOST as _, &io_config, &mut panel_io)
    })?;
    unsafe {
        ESP_LCD_PANEL_IO_HANDLE = panel_io;
    }
    ::log::info!("panel IO created");

    let mut panel_config = esp_lcd_panel_dev_config_t::default();
    let mut panel: esp_lcd_panel_handle_t = std::ptr::null_mut();

    panel_config.reset_gpio_num = rst_gpio_num;
    panel_config.data_endian = lcd_rgb_data_endian_t_LCD_RGB_DATA_ENDIAN_LITTLE;
    panel_config.__bindgen_anon_1.rgb_ele_order = lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_RGB;
    panel_config.bits_per_pixel = 16;

    ::log::info!("Creating ST7789 panel (esp_lcd_new_panel_st7789)...");
    esp!(unsafe { esp_lcd_new_panel_st7789(panel_io, &panel_config, &mut panel) })?;
    ::log::info!("ST7789 panel created");

    unsafe {
        ESP_LCD_PANEL_HANDLE = panel;
    }

    const DISPLAY_MIRROR_X: bool = false;
    const DISPLAY_MIRROR_Y: bool = false;
    const DISPLAY_SWAP_XY: bool = false;
    const DISPLAY_INVERT_COLOR: bool = true;

    // Manual reset sequence: toggle the reset GPIO explicitly (helps if the panel driver reset hangs)
    {
        use std::time::Duration;
        use std::thread::sleep;
        ::log::info!("Manual reset: toggling reset GPIO via sys calls");
        let rst_num = rst_gpio_num as _;
        unsafe {
            esp!(gpio_set_direction(rst_num, gpio_mode_t_GPIO_MODE_OUTPUT))?;
            esp!(gpio_set_level(rst_num, 0))?;
        }
        sleep(Duration::from_millis(50));
        unsafe { esp!(gpio_set_level(rst_num, 1))?; }
        sleep(Duration::from_millis(120));
        ::log::info!("Manual reset done");
    }

    unsafe {
        ::log::info!("Calling esp_lcd_panel_reset(panel)...");
        esp!(esp_lcd_panel_reset(panel))?;
        ::log::info!("Calling esp_lcd_panel_init(panel)...");
        esp!(esp_lcd_panel_init(panel))?;
        ::log::info!("Setting panel gap and forcing color mode/orientation...");
        esp!(esp_lcd_panel_set_gap(panel, 0, 0))?;
        // Force COLMOD (0x3A) = RGB565 and MADCTL (0x36) = 0x00 (orientation)
        let colmod: [u8; 1] = [0x05];
        esp!(esp_lcd_panel_io_tx_param(panel_io, 0x3A, colmod.as_ptr().cast(), colmod.len()))?;
        let madctl: [u8; 1] = [0x00];
        esp!(esp_lcd_panel_io_tx_param(panel_io, 0x36, madctl.as_ptr().cast(), madctl.len()))?;
        ::log::info!("Calling esp_lcd_panel_invert_color...");
        esp!(esp_lcd_panel_invert_color(panel, DISPLAY_INVERT_COLOR))?;
        ::log::info!("Calling esp_lcd_panel_swap_xy...");
        esp!(esp_lcd_panel_swap_xy(panel, DISPLAY_SWAP_XY))?;
        ::log::info!("Calling esp_lcd_panel_mirror...");
        esp!(esp_lcd_panel_mirror(
            panel,
            DISPLAY_MIRROR_X,
            DISPLAY_MIRROR_Y
        ))?;
        ::log::info!("Turning display on (esp_lcd_panel_disp_on_off)...");
        esp!(esp_lcd_panel_disp_on_off(panel, true))?;
        ::log::info!("Panel init and ON complete");
    }

    Ok(())
}

/// Set raw MADCTL (0x36) byte on the panel IO (allows experimenting with orientation flags)
pub fn set_madctl(mad: u8) -> Result<(), EspError> {
    unsafe {
        use esp_idf_svc::sys::*;
        if ESP_LCD_PANEL_IO_HANDLE.is_null() {
            ::log::error!("set_madctl: panel IO handle is null");
            return Ok(());
        }
        let madctl: [u8; 1] = [mad];
        esp!(esp_lcd_panel_io_tx_param(ESP_LCD_PANEL_IO_HANDLE, 0x36, madctl.as_ptr().cast(), madctl.len()))?;
        if !ESP_LCD_PANEL_HANDLE.is_null() {
            esp!(esp_lcd_panel_disp_on_off(ESP_LCD_PANEL_HANDLE, false))?;
            {
                use std::thread::sleep;
                use std::time::Duration;
                sleep(Duration::from_millis(50));
            }
            esp!(esp_lcd_panel_disp_on_off(ESP_LCD_PANEL_HANDLE, true))?;
        }
    }
    Ok(())
}

/// Set panel XY gap (column/row offset). Useful to correct physical panel alignment.
pub fn set_gap(x_gap: i32, y_gap: i32) -> Result<(), EspError> {
    unsafe {
        use esp_idf_svc::sys::*;
        if ESP_LCD_PANEL_HANDLE.is_null() {
            ::log::error!("set_gap: panel handle is null");
            return Ok(());
        }
        // Store logical gap values and apply taking current rotation into account
        PANEL_GAP_X = x_gap;
        PANEL_GAP_Y = y_gap;
        if let Err(e) = apply_stored_gap() {
            ::log::warn!("set_gap: failed to apply stored gap: {:?}", e);
        }

        esp!(esp_lcd_panel_disp_on_off(ESP_LCD_PANEL_HANDLE, false))?;
        {
            use std::thread::sleep;
            use std::time::Duration;
            sleep(Duration::from_millis(50));
        }
        esp!(esp_lcd_panel_disp_on_off(ESP_LCD_PANEL_HANDLE, true))?;
    }
    Ok(())
}

pub fn flush_display(color_data: &[u8], x_start: i32, y_start: i32, x_end: i32, y_end: i32) -> i32 {
    unsafe {
        use esp_idf_svc::sys::*;

        // Basic validation: ensure rectangle is valid
        if x_start >= x_end || y_start >= y_end {
            ::log::warn!(
                "flush_display invalid rectangle: x_start={} x_end={} y_start={} y_end={}",
                x_start,
                x_end,
                y_start,
                y_end
            );
            return ESP_ERR_INVALID_ARG;
        }

        let width = (x_end - x_start) as usize;
        let height = (y_end - y_start) as usize;
        let row_bytes = width * std::mem::size_of::<u16>();

        // Allocate a DMA-capable temporary buffer for one row
        let dma_ptr = heap_caps_malloc(row_bytes, MALLOC_CAP_DMA) as *mut u8;
        if dma_ptr.is_null() {
            ::log::warn!("flush_display: failed to allocate DMA buffer ({} bytes)", row_bytes);
            return ESP_ERR_NO_MEM;
        }

        let mut last_e: i32 = 0;
        for row in 0..height {
            let src_offset = row * row_bytes;
            std::ptr::copy_nonoverlapping(
                color_data.as_ptr().add(src_offset),
                dma_ptr.add(0),
                row_bytes,
            );

            let y0 = y_start + row as i32;
            let y1 = y0 + 1;

            let e = esp_lcd_panel_draw_bitmap(
                ESP_LCD_PANEL_HANDLE,
                x_start,
                y0,
                x_end,
                y1,
                dma_ptr.cast(),
            );
            if e != 0 {
                let rot = get_rotation_state();
                ::log::warn!("flush_display draw_bitmap error at row {}: {} - coords x_start={}, y0={}, x_end={}, y1={}, width={}, height={}, rot={}", row, e, x_start, y0, x_end, y1, width, height, rot);
            }
            last_e = e;
        }

        heap_caps_free(dma_ptr.cast());
        last_e
    }
}

#[macro_export]
macro_rules! start_hal {
    ($peripherals:ident, $evt_tx:ident) => {{
        crate::boards::base::init_spi(
            $peripherals.spi3,
            $peripherals.pins.gpio20, // Mosi / SDA dello schermo
            $peripherals.pins.gpio19,  // Clk / SCL dello schermo
        )?;
        crate::boards::base::init_lcd(
            $peripherals.pins.gpio45,  // CS dello schermo
            $peripherals.pins.gpio47,  // DC dello schermo
            $peripherals.pins.gpio21,  // RES dello schermo
        )?;

        // Proviamo ad inizializzare l'espansore I2C XL9555 (utile su alcune varianti Xiaozhi)
        // XL9555 I/O expander initialization
        // NOTE: on some board variants the XL9555 uses GPIO45/GPIO48 (I2C SCL/SDA).
        // Those pins are also used for the SPI LCD (CS=GPIO45, BL=GPIO48) on the
        // ST7789 240x240 variant. Initializing the expander will reconfigure those
        // pins for I2C and can break the display. Disable by default and enable
        // only for known variants that actually have the XL9555 present.
        const ENABLE_XL9555: bool = true;
        if ENABLE_XL9555 {
            unsafe {
                use esp_idf_svc::sys::hal_driver;
                // myiic_init/xl9555_init can be called directly from C bindings.
                // Wrap with panic-catching to avoid Rust panics escaping, but
                // C errors should be handled in the C driver.
                let _ = std::panic::catch_unwind(|| {
                    let _ = hal_driver::myiic_init();
                    let _ = hal_driver::xl9555_init();
                    let _ = hal_driver::xl9555_pin_write(hal_driver::LCD_BL_IO as _, 1);
                });
            }
        } else {
            ::log::info!("XL9555 init skipped (ENABLE_XL9555=false).\nIf your board uses XL9555 enable it in code.");
        }

        // Gestione Retroilluminazione (BLK) agganciata al tuo GPIO 48 (wrapper PWM/GPIO)
        let __bl_gpio_num = esp_idf_svc::hal::gpio::Pin::pin(&$peripherals.pins.gpio48);
        let mut _backlight = crate::boards::PwmBacklight::new(
            $peripherals.pins.gpio48.into(),
            __bl_gpio_num,
        );
        if let Err(e) = _backlight.set(20) {
            ::log::error!("PwmBacklight set failed: {:?}", e);
        }
    }};
}

#[macro_export]
macro_rules! start_audio_workers {
    ($peripherals:ident, $rx:expr, $evt_tx:expr, $tokio_rt:expr) => {{
        crate::boards::base::start_audio_workers(
            $peripherals.i2s1,
            $peripherals.pins.gpio21, // BCLK Altoparlante
            $peripherals.pins.gpio46, // LRC Altoparlante
            $peripherals.pins.gpio45, // DIN Altoparlante
            $peripherals.i2s0,
            $peripherals.pins.gpio41, // SCK Microfono
            $peripherals.pins.gpio42, // WS Microfono
            $peripherals.pins.gpio2,  // SD Microfono
            $rx,
            $evt_tx,
        )?;
        crate::boards::base::start_btn_worker(
            $tokio_rt,
            $peripherals.pins.gpio9,  // Tasto POW
            $peripherals.pins.gpio3,  // Tasto Wake
            $evt_tx,
        )?;
    }};
}
