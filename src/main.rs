#![no_std]
#![no_main]

#[cfg(any(feature = "bme280", feature = "scd30"))]
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};

use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    self as hal,
    clock::CpuClock,
    ram,
    rng::Rng,
    timer::timg::{MwdtStage, TimerGroup, Wdt},
};
#[cfg(any(feature = "bme280", feature = "scd30"))]
use esp_hal::{Async, i2c::master::{BusTimeout, I2c}, time::Rate};
#[cfg(feature = "sds011")]
use esp_hal::uart::{RxConfig, Uart};
use esp_println::logger::init_logger;
use esp_radio::Controller;

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use static_cell::StaticCell;

extern crate alloc;

pub mod config;
pub mod constants;
pub mod cstr;
mod ota;
mod measurement;
mod mqtt;
pub mod sensors;
pub mod transport;
mod wifi;

use config::CONFIG;
use constants::*;
use ota::Ota;
use measurement::Measurement;
use sensors::Sensors;
use wifi::Wifi;

#[cfg(any(feature = "bme280", feature = "scd30"))]
static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, Async>>> = StaticCell::new();
static STACK: StaticCell<Mutex<NoopRawMutex, Stack<'static>>> = StaticCell::new();

static RX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>> = StaticCell::new();
static TX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>> = StaticCell::new();
static TLS_READ_BUF: StaticCell<Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>> = StaticCell::new();
static TLS_WRITE_BUF: StaticCell<Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>> = StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[esp_rtos::main(stack_size = 32768)]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Info);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let rng = Rng::new();

    // Use reclaimed RAM for the heap - this is critical for WiFi to work properly.
    // The WiFi blob expects memory to be allocated from reclaimed RAM regions.
    // See: https://github.com/esp-rs/esp-hal/blob/esp-radio-v0.17.0/examples/wifi/embassy_dhcp/src/main.rs
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 36 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let mut wdt0 = timg0.wdt;
    wdt0.enable();
    // Set watchdog timeout to accommodate TLS handshakes and sensor operations.
    // TLS 1.3 handshake on ESP32 can take 10-20+ seconds, and sensor measurements may also
    // require significant time (e.g., SCD30 data ready wait).
    wdt0.set_timeout(MwdtStage::Stage0, hal::time::Duration::from_secs(WATCHDOG_TIMEOUT_SECS));

    esp_rtos::start(timg0.timer0);

    // possibly high transient required at init
    // https://github.com/esp-rs/esp-hal/issues/1626
    Timer::after(Duration::from_millis(1000)).await;

    #[cfg_attr(
        not(any(feature = "bme280", feature = "scd30", feature = "sds011")),
        allow(unused_mut)
    )]
    let mut sensors = Sensors::new();

    #[cfg(any(feature = "bme280", feature = "scd30"))]
    {
        let (sda, scl) = (peripherals.GPIO21, peripherals.GPIO22);

        let i2c_config = hal::i2c::master::Config::default()
            .with_frequency(Rate::from_khz(100))
            .with_timeout(BusTimeout::BusCycles(24));

        let i2c = I2c::new(peripherals.I2C0, i2c_config)
            .unwrap()
            .with_sda(sda)
            .with_scl(scl)
            .into_async();

        let i2c_bus = Mutex::new(i2c);
        let i2c_bus = I2C_BUS.init(i2c_bus);

        #[cfg(feature = "bme280")]
        if (sensors.new_bme280(I2cDevice::new(i2c_bus)).await).is_err() {
            log::error!("Failed initializing BME280. Rebooting...");
            esp_hal::system::software_reset();
        }

        #[cfg(feature = "scd30")]
        if (sensors.new_scd30(I2cDevice::new(i2c_bus)).await).is_err() {
            log::error!("Failed initializing SCD30. Rebooting...");
            esp_hal::system::software_reset();
        }
    }

    #[cfg(feature = "sds011")]
    {
        let (tx, rx) = (peripherals.GPIO17, peripherals.GPIO16);

        let uart_config = hal::uart::Config::default()
            .with_rx(RxConfig::default().with_fifo_full_threshold(UART_READ_BUFFER_SIZE as u16))
            .with_baudrate(9600)
            .with_stop_bits(hal::uart::StopBits::_1)
            .with_data_bits(hal::uart::DataBits::_8)
            .with_parity(hal::uart::Parity::None);

        let mut uart = Uart::new(peripherals.UART2, uart_config)
            .unwrap()
            .with_tx(tx)
            .with_rx(rx)
            .into_async();

        uart.set_at_cmd(hal::uart::AtCmdConfig::default().with_cmd_char(UART_AT_CMD));

        if (sensors.new_sds011(uart).await).is_err() {
            log::error!("Failed initializing SDS011. Rebooting...");
            esp_hal::system::software_reset();
        }
    }

    let flash = esp_storage::FlashStorage::new(peripherals.FLASH);

    let esp_radio_ctrl = &*mk_static!(Controller<'static>, esp_radio::init().unwrap());

    let wifi = Wifi::new(
        esp_radio_ctrl,
        peripherals.WIFI,
        rng,
        spawner,
    )
    .await
    .unwrap();

    // Try to connect to WiFi with a timeout. If it fails, reboot.
    // This prevents the device from hanging indefinitely at boot if WiFi is flaky.
    match embassy_time::with_timeout(Duration::from_secs(20), wifi.connect()).await {
        Ok(Ok(_)) => log::info!("Initial WiFi connection successful"),
        Ok(Err(e)) => {
            log::error!("Initial WiFi connection failed: {:?}. Rebooting...", e);
            esp_hal::system::software_reset();
        }
        Err(_) => {
            log::error!("Initial WiFi connection timed out. Rebooting...");
            esp_hal::system::software_reset();
        }
    }

    // Create a 32-byte seed for ChaCha20Rng
    let mut seed = [0u8; 32];
    for chunk in seed.chunks_mut(4) {
        let random_u32 = rng.random();
        chunk.copy_from_slice(&random_u32.to_le_bytes()[..chunk.len()]);
    }
    let chacha_rng = ChaCha20Rng::from_seed(seed);

    let stack_shared = Mutex::new(wifi.stack);
    let stack_shared = STACK.init(stack_shared);

    let rx_buf = RX_BUF.init(Mutex::new([0; RX_BUFFER_SIZE]));
    let tx_buf = TX_BUF.init(Mutex::new([0; TX_BUFFER_SIZE]));
    let tls_read_buf = TLS_READ_BUF.init(Mutex::new([0; TLS_BUFFER_MAX]));
    let tls_write_buf = TLS_WRITE_BUF.init(Mutex::new([0; TLS_BUFFER_MAX]));

    let ota = Ota::new(
        stack_shared,
        chacha_rng.clone(),
        rx_buf,
        tx_buf,
        tls_read_buf,
        tls_write_buf,
        flash,
    )
    .unwrap();
    let measurement = Measurement::new(
        stack_shared,
        chacha_rng,
        rx_buf,
        tx_buf,
        tls_read_buf,
        tls_write_buf,
        sensors,
    )
    .unwrap();

    spawner
        .spawn(main_task(ota, measurement, wdt0))
        .ok();
}

#[embassy_executor::task]
async fn main_task(
    #[cfg_attr(not(feature = "ota"), allow(unused))] mut ota: Ota<'static>,
    mut measurement: Measurement,
    mut wdt: Wdt<esp_hal::peripherals::TIMG0<'static>>,
) {
    // check for firmware update at boot time
    #[cfg(feature = "ota")]
    let mut update_counter: u64 =
        FIRMWARE_CHECK_INTERVAL / CONFIG.measurement_interval_seconds as u64;

    loop {
        // Feed watchdog at start of loop
        wdt.feed();

        // Only check for firmware updates periodically
        #[cfg(feature = "ota")]
        if update_counter >= FIRMWARE_CHECK_INTERVAL / CONFIG.measurement_interval_seconds as u64 {
            update_counter = 0;
            if let Err(e) = ota.check().await {
                log::error!("Firmware update error: {:?}", e);
                // Smart sleep that feeds watchdog
                for _ in 0..CONFIG.measurement_interval_seconds {
                    Timer::after(Duration::from_secs(1)).await;
                    wdt.feed();
                }
                continue;
            }
        }

        // Feed before potentially long measurement/network operation
        wdt.feed();

        // Take measurements each cycle
        if let Err(e) = measurement.take().await {
            log::error!("Measurement error: {:?}", e);
        }

        #[cfg(feature = "ota")]
        {
            update_counter += 1;
        }

        // Smart sleep that feeds watchdog instead of single long sleep
        for _ in 0..CONFIG.measurement_interval_seconds {
            Timer::after(Duration::from_secs(1)).await;
            wdt.feed();
        }
    }
}
