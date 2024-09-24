#![no_std]
#![no_main]
#![feature(generic_arg_infer)]
#![feature(type_alias_impl_trait)]

use core::fmt::{Error, Write};
use heapless::String;
use log::info;

use embassy_executor::Spawner;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack};
use embassy_time::{Delay, Duration, Timer};

use esp_alloc as _;
use esp_backtrace as _;
use esp_hal as hal;
use esp_println::logger::init_logger;
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};

use hal::{
    gpio::Io,
    i2c::I2C,
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
mod sds011;
mod wifi;

use bme280::Bme280;
use sds011::Sds011;
use wifi::Wifi;

const READ_BUF_SIZE: usize = 10;
const AT_CMD: u8 = 0xAB;

// TODO: find a better way to set data points through data model
const SDS011_DATA_POINT: usize = 2;
const BME280_DATA_POINT: usize = 3;
const TOTAL_DATA_POINTS: usize = get_total_data_points();

pub struct Sensors<I2C, S, D> {
    bme280: Option<Bme280<I2C, D>>,
    sds011: Option<Sds011<S>>,
}

#[main]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Info);

    let peripherals = esp_hal::init({
        let mut hal_config = esp_hal::Config::default();
        hal_config.cpu_clock = CpuClock::max();
        hal_config
    });

    let rng = Rng::new(peripherals.RNG);

    esp_alloc::heap_allocator!(72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let timg1 = TimerGroup::new(peripherals.TIMG1);

    esp_hal_embassy::init(timg0.timer0);

    // possibly high transient required at init
    // https://github.com/esp-rs/esp-hal/issues/1626
    Timer::after(Duration::from_millis(1000)).await;

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    let delay = Delay;

    let mut sensors = Sensors {
        bme280: None,
        sds011: None,
    };

    if cfg!(feature = "bme280") {
        let i2c = I2C::new_async(
            peripherals.I2C0,
            io.pins.gpio21,
            io.pins.gpio22,
            100u32.kHz(),
        );
        sensors.bme280 = Some(Bme280::new(i2c, delay).await.unwrap());
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
    mut sensors: Sensors<I2C<'static, I2C0, Async>, Uart<'static, UART2, Async>, Delay>,
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
        Timer::after(Duration::from_secs(CONFIG.measurement_interval_seconds)).await;
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

    if cfg!(feature = "sds011") {
        datapoints += SDS011_DATA_POINT
    }

    datapoints
}
