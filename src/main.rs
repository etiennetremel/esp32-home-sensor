#![no_std]
#![no_main]

use core::fmt::{Error, Write};
use heapless::String;
use log::info;
use static_cell::StaticCell;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};

extern crate alloc;

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

pub mod config;
pub mod cstr;
mod mqtt;
pub mod sensors;
mod transport;
mod wifi;

use config::CONFIG;
use mqtt::Mqtt;
use sensors::{SensorData, Sensors};
use transport::Transport;
use wifi::Wifi;

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'static, Async>>> = StaticCell::new();

const READ_BUF_SIZE: usize = 64;
const AT_CMD: u8 = 0xAB;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Trace);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let rng = Rng::new(peripherals.RNG);

    esp_alloc::heap_allocator!(size: 115 * 1024);

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
            .with_rx(RxConfig::default().with_fifo_full_threshold(READ_BUF_SIZE as u16))
            .with_baudrate(9600)
            .with_stop_bits(hal::uart::StopBits::_1)
            .with_data_bits(hal::uart::DataBits::_8)
            .with_parity(hal::uart::Parity::None);

        let mut uart = Uart::new(peripherals.UART2, uart_config)
            .unwrap()
            .with_tx(tx)
            .with_rx(rx)
            .into_async();

        uart.set_at_cmd(hal::uart::AtCmdConfig::default().with_cmd_char(AT_CMD));

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

    spawner.spawn(measure(wifi.stack, sensors, tls)).ok();
}

#[embassy_executor::task]
async fn measure(stack: Stack<'static>, mut sensors: Sensors, tls: Tls<'static>) {
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    loop {
        let sensor_data = match sensors.measure().await {
            Ok(sensor_data) => sensor_data,
            Err(e) => {
                info!("Error retrieving sensors data: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        let message = match format_mqtt_message(&sensor_data) {
            Ok(message) => message,
            Err(e) => {
                info!("Error formatting MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        let mut mqtt_rx_buffer = [0; 1024];
        let mut mqtt_tx_buffer = [0; 1024];

        info!("Creating TCP session");
        let session = Transport::new(stack, &tls, &mut rx_buffer, &mut tx_buffer)
            .await
            .unwrap();

        info!("Creating MQTT client");
        let mut mqtt = Mqtt::new(session, &mut mqtt_tx_buffer, &mut mqtt_rx_buffer)
            .await
            .unwrap();

        info!(
            "Publishing to topic {:?}, payload: {:?}",
            CONFIG.mqtt_topic, message
        );
        if let Err(e) = mqtt
            .send_message(CONFIG.mqtt_topic, message.as_bytes())
            .await
        {
            info!("Error publishing MQTT message: {e:?}");
            Timer::after(Duration::from_secs(10)).await;
            break;
        }

        info!(
            "Message published, waiting {:?} seconds",
            CONFIG.measurement_interval_seconds
        );
        Timer::after(Duration::from_secs(
            CONFIG.measurement_interval_seconds.into(),
        ))
        .await;
    }
}

fn format_mqtt_message(sensor_data: &SensorData) -> Result<String<256>, Error> {
    let mut payload: String<256> = String::new();

    if cfg!(feature = "json") {
        write!(payload, "{{\"location\": \"{}\"", CONFIG.location)?;
        for (key, value) in sensor_data.data.iter() {
            write!(payload, ", \"{}\": \"{:.2}\"", key, value)?;
        }
        write!(payload, "}}")?;
    }

    if cfg!(feature = "influx") {
        write!(payload, "weather,location={}", CONFIG.location)?;
        let mut first = true;
        for (key, value) in sensor_data.data.iter() {
            if first {
                write!(payload, " {}={:.2}", key, value)?;
                first = false;
            } else {
                write!(payload, ",{}={:.2}", key, value)?;
            }
        }
    }

    Ok(payload)
}
