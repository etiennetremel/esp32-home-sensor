#![no_std]
#![no_main]

use static_cell::StaticCell;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};

use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{self as hal};
use esp_mbedtls::Tls;
use esp_println::logger::init_logger;

use hal::{
    i2c::master::{BusTimeout, I2c},
    rng::Rng,
    time::Rate,
    timer::timg::TimerGroup,
    uart::{RxConfig, Uart},
    Async,
};

extern crate alloc;

pub mod config;
pub mod constants;
pub mod cstr;
mod firmware_update;
mod measurement;
mod mqtt;
pub mod sensors;
pub mod transport;
mod wifi;

use config::CONFIG;
use constants::*;
use firmware_update::FirmwareUpdate;
use measurement::Measurement;
use sensors::Sensors;
use wifi::Wifi;

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, Async>>> = StaticCell::new();
static TLS: StaticCell<Tls<'static>> = StaticCell::new();
static STACK: StaticCell<Mutex<NoopRawMutex, Stack<'static>>> = StaticCell::new();

static RX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>> = StaticCell::new();
static TX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>> = StaticCell::new();

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Info);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let rng = Rng::new(peripherals.RNG);

    esp_alloc::heap_allocator!(size: HEAP_SIZE);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let timg1 = TimerGroup::new(peripherals.TIMG1);

    esp_hal_embassy::init(timg0.timer0);

    // possibly high transient required at init
    // https://github.com/esp-rs/esp-hal/issues/1626
    Timer::after(Duration::from_millis(1000)).await;

    let mut sensors = Sensors::new();

    if cfg!(feature = "bme280") || cfg!(feature = "scd30") {
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

        if cfg!(feature = "bme280") {
            sensors.new_bme280(I2cDevice::new(i2c_bus)).await.unwrap();
        }

        if cfg!(feature = "scd30") {
            sensors.new_scd30(I2cDevice::new(i2c_bus)).await.unwrap();
        }
    }

    if cfg!(feature = "sds011") {
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

        sensors.new_sds011(uart).await.unwrap();
    }

    let wifi = Wifi::new(
        peripherals.WIFI,
        timg1.timer0,
        peripherals.RADIO_CLK,
        rng.clone(),
        spawner,
    )
    .await
    .unwrap();

    wifi.connect().await.unwrap();

    let mut tls = Tls::new(peripherals.SHA)
        .unwrap()
        .with_hardware_rsa(peripherals.RSA);

    tls.set_debug(0);

    let tls_shared = TLS.init(tls);

    let stack_shared = Mutex::new(wifi.stack);
    let stack_shared = STACK.init(stack_shared);

    let rx_buf = RX_BUF.init(Mutex::new([0; RX_BUFFER_SIZE]));
    let tx_buf = TX_BUF.init(Mutex::new([0; TX_BUFFER_SIZE]));

    let firmware_update = FirmwareUpdate::new(stack_shared, tls_shared, rx_buf, tx_buf).unwrap();
    let measurement = Measurement::new(stack_shared, tls_shared, rx_buf, tx_buf, sensors).unwrap();

    spawner.spawn(main_task(firmware_update, measurement)).ok();
}

#[embassy_executor::task]
async fn main_task(mut firmware_update: FirmwareUpdate, mut measurement: Measurement) {
    let mut update_counter = 0;

    loop {
        // Only check for firmware updates periodically
        if cfg!(feature = "ota")
            && update_counter
                >= FIRMWARE_CHECK_INTERVAL / CONFIG.measurement_interval_seconds as u64
        {
            update_counter = 0;
            if let Err(e) = firmware_update.check().await {
                log::error!("Firmware update error: {:?}", e);
                Timer::after(Duration::from_secs(
                    CONFIG.measurement_interval_seconds.into(),
                ))
                .await;
                continue;
            }
        }

        // Take measurements each cycle
        if let Err(e) = measurement.take().await {
            log::error!("Measurement error: {:?}", e);
        }

        if cfg!(feature = "ota") {
            update_counter += 1;
        }

        Timer::after(Duration::from_secs(
            CONFIG.measurement_interval_seconds.into(),
        ))
        .await;
    }
}
