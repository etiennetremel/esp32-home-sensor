#![no_std]
#![no_main]
#![feature(generic_arg_infer)]
#![feature(type_alias_impl_trait)]

use core::fmt::{Error, Write};
use heapless::String;
use log::info;
use static_cell::StaticCell;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Timer};

extern crate alloc;

use esp_alloc as _;
use esp_backtrace as _;
use esp_hal as hal;
use esp_println::logger::init_logger;
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};

use hal::{
    gpio::Io,
    i2c::I2c,
    peripherals::{I2C0, UART2},
    prelude::*,
    rng::Rng,
    timer::timg::TimerGroup,
    uart::Uart,
    Async,
};

use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::publish_packet::QualityOfService,
    utils::rng_generator::CountingRng,
};

pub mod config;

use config::CONFIG;

mod bme280;
mod scd30;
mod sds011;
mod wifi;

use bme280::Bme280;
use scd30::Scd30;
use sds011::Sds011;
use wifi::Wifi;

const READ_BUF_SIZE: usize = 10;
const AT_CMD: u8 = 0xAB;

// TODO: find a better way to set data points through data model
const SDS011_DATA_POINT: usize = 2;
const SCD30_DATA_POINT: usize = 3;
const BME280_DATA_POINT: usize = 3;
const TOTAL_DATA_POINTS: usize = get_total_data_points();

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, I2c<'_, I2C0, Async>>> = StaticCell::new();

pub struct Sensors<I2C, S> {
    bme280: Option<Bme280<I2C>>,
    sds011: Option<Sds011<S>>,
    scd30: Option<Scd30<I2C>>,
}

#[main]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Info);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let rng = Rng::new(peripherals.RNG);

    esp_alloc::heap_allocator!(72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let timg1 = TimerGroup::new(peripherals.TIMG1);

    esp_hal_embassy::init(timg0.timer0);

    // possibly high transient required at init
    // https://github.com/esp-rs/esp-hal/issues/1626
    Timer::after(Duration::from_millis(1000)).await;

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    let mut sensors = Sensors {
        bme280: None,
        sds011: None,
        scd30: None,
    };

    if cfg!(feature = "bme280") || cfg!(feature = "scd30") {
        let (sda, scl) = (io.pins.gpio21, io.pins.gpio22);

        let i2c = I2c::new_async(peripherals.I2C0, sda, scl, 100u32.kHz());
        let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

        if cfg!(feature = "bme280") {
            sensors.bme280 = Some(Bme280::new(I2cDevice::new(i2c_bus)).await.unwrap());
        }

        if cfg!(feature = "scd30") {
            sensors.scd30 = Some(Scd30::new(I2cDevice::new(i2c_bus)).await.unwrap());
        }
    }

    if cfg!(feature = "sds011") {
        let (uart_tx_pin, uart_rx_pin) = (io.pins.gpio17, io.pins.gpio16);

        let uart_config = esp_hal::uart::config::Config {
            baudrate: 9600,
            data_bits: esp_hal::uart::config::DataBits::DataBits8,
            parity: esp_hal::uart::config::Parity::ParityNone,
            stop_bits: esp_hal::uart::config::StopBits::STOP1,
            ..esp_hal::uart::config::Config::default()
        };
        uart_config.rx_fifo_full_threshold(READ_BUF_SIZE as u16);

        let mut uart =
            Uart::new_async_with_config(peripherals.UART2, uart_config, uart_tx_pin, uart_rx_pin)
                .unwrap();
        uart.set_at_cmd(esp_hal::uart::config::AtCmdConfig::new(
            None, None, None, AT_CMD, None,
        ));

        sensors.sds011 = Some(Sds011::new(uart).await.unwrap());
    }

    let wifi = Wifi::new(
        peripherals.WIFI,
        timg1.timer0,
        peripherals.RADIO_CLK,
        rng,
        spawner,
    )
    .await
    .unwrap();

    wifi.connect().await.unwrap();

    spawner.spawn(measure(wifi.stack, sensors)).ok();
}

#[embassy_executor::task]
async fn measure(
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
    mut sensors: Sensors<
        I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
        Uart<'static, UART2, Async>,
    >,
) {
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    let mut args: [(&'static str, f32); TOTAL_DATA_POINTS] = [("", 0.0); TOTAL_DATA_POINTS];

    loop {
        let mut index = 0;
        if cfg!(feature = "bme280") {
            if let Some(ref mut bme280) = sensors.bme280 {
                match bme280.measure().await {
                    Ok(measurement) => {
                        args[index] = ("temperature", measurement.temperature);
                        index += 1;
                        args[index] = ("humidity", measurement.humidity);
                        index += 1;
                        args[index] = ("pressure", measurement.pressure);
                        index += 1;
                    }
                    Err(e) => {
                        info!("Error taking BME280 measurement: {:?}", e);
                        Timer::after(Duration::from_secs(10)).await;
                        continue;
                    }
                }
            } else {
                // Handle the case where bme280 is None if necessary
                info!("BME280 sensor is not available");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        if cfg!(feature = "scd30") {
            if let Some(ref mut scd30) = sensors.scd30 {
                match scd30.measure().await {
                    Ok(measurement) => {
                        args[index] = ("temperature", measurement.temperature);
                        index += 1;
                        args[index] = ("humidity", measurement.humidity);
                        index += 1;
                        args[index] = ("co2", measurement.co2);
                        index += 1;
                    }
                    Err(e) => {
                        info!("Error taking SCD30 measurement: {:?}", e);
                        Timer::after(Duration::from_secs(10)).await;
                        continue;
                    }
                }
            } else {
                // Handle the case where scd30 is None if necessary
                info!("SCD30 sensor is not available");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        if cfg!(feature = "sds011") {
            if let Some(ref mut sds011) = sensors.sds011 {
                match sds011.measure().await {
                    Ok(measurement) => {
                        args[index] = ("air_quality_pm2_5", measurement.pm2_5);
                        index += 1;
                        args[index] = ("air_quality_pm10", measurement.pm10);
                    }
                    Err(e) => {
                        info!("Error taking SDS011 measurement: {:?}", e);
                        Timer::after(Duration::from_secs(10)).await;
                        continue;
                    }
                }
            } else {
                // Handle the case where bme280 is None if necessary
                info!("SDS011 sensor is not available");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        let message = match format_mqtt_message(&args) {
            Ok(message) => message,
            Err(e) => {
                info!("Error formating MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        let host_addr = match stack
            .dns_query(CONFIG.mqtt_hostname, DnsQueryType::A)
            .await
            .map(|a| a[0])
        {
            Ok(address) => address,
            Err(e) => {
                info!("DNS lookup for MQTT host failed with error: {e:?}");
                continue;
            }
        };

        let socket_addr = (host_addr, CONFIG.mqtt_port);

        info!("Connecting to MQTT server...");
        let r = socket.connect(socket_addr).await;
        if let Err(e) = r {
            info!("Connect error: {e:?}");
            continue;
        }
        info!("Connected to MQTT server");

        info!("Initialising MQTT connection");
        let mut mqtt_rx_buffer = [0; 1024];
        let mut mqtt_tx_buffer = [0; 1024];
        let mut mqtt_config: ClientConfig<5, CountingRng> = ClientConfig::new(
            rust_mqtt::client::client_config::MqttVersion::MQTTv5,
            CountingRng(20000),
        );
        mqtt_config.add_username(CONFIG.mqtt_username);
        mqtt_config.add_password(CONFIG.mqtt_password);

        let mut client = MqttClient::<_, 5, _>::new(
            socket,
            &mut mqtt_tx_buffer,
            256,
            &mut mqtt_rx_buffer,
            256,
            mqtt_config,
        );

        if let Err(e) = client.connect_to_broker().await {
            info!("Couldn't connect to MQTT broker: {e:?}");
            Timer::after(Duration::from_secs(10)).await;
            continue;
        }

        info!(
            "Publishing to topic {:?}, payload: {:?}",
            CONFIG.mqtt_topic, message
        );
        if let Err(e) = client
            .send_message(
                CONFIG.mqtt_topic,
                message.as_bytes(),
                QualityOfService::QoS0,
                false,
            )
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

fn format_mqtt_message(args: &[(&str, f32)]) -> Result<String<256>, Error> {
    let mut payload: String<256> = String::new();

    if cfg!(feature = "json") {
        write!(payload, "{{\"location\": \"{}\"", CONFIG.location)?;
        for (key, value) in args {
            write!(payload, ", \"{}\": \"{:.2}\"", key, value)?;
        }
        write!(payload, "}}")?;
    }

    if cfg!(feature = "influx") {
        write!(payload, "weather,location={}", CONFIG.location)?;
        let mut first = true;
        for (key, value) in args {
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

const fn get_total_data_points() -> usize {
    let mut datapoints: usize = 0;

    if cfg!(feature = "bme280") {
        datapoints += BME280_DATA_POINT;
    }

    if cfg!(feature = "scd30") {
        datapoints += SCD30_DATA_POINT
    }

    if cfg!(feature = "sds011") {
        datapoints += SDS011_DATA_POINT
    }

    datapoints
}
