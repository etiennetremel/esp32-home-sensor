#![no_std]
#![no_main]
#![feature(generic_arg_infer)]
#![feature(type_alias_impl_trait)]

use core::fmt::{Error, Write};
use core::str::FromStr;
use heapless::String;
use log::info;
use static_cell::make_static;

use embassy_executor::Spawner;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack, StackResources};
use embassy_time::{Delay, Duration, Timer};

use esp_backtrace as _;
use esp_hal as hal;
use esp_println::logger::init_logger;
use esp_wifi::{
    initialize,
    wifi::{
        ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiStaDevice,
        WifiState,
    },
    EspWifiInitFor,
};

use hal::{
    clock::ClockControl,
    embassy::{self},
    gpio::IO,
    i2c::I2C,
    peripherals::{Peripherals, I2C0, UART2},
    prelude::*,
    rng::Rng,
    timer::TimerGroup,
    uart::{TxRxPins, Uart},
    Async,
};

use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::publish_packet::QualityOfService,
    utils::rng_generator::CountingRng,
};

mod sensor;

use sensor::Sensor;

const READ_BUF_SIZE: usize = 10;
const AT_CMD: u8 = 0xAB;
const MEASUREMENT_INTERVAL_SECONDS: u64 = 5 * 60; // 5minutes

#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_psk: &'static str,
    // smoltcp currently doesn't have a way of giving a hostname through DHCP
    #[default("esp32")]
    hostname: &'static str,
    #[default("")]
    location: &'static str,
    #[default("")]
    mqtt_hostname: &'static str,
    #[default(1883)]
    mqtt_port: u16,
    #[default("")]
    mqtt_username: &'static str,
    #[default("")]
    mqtt_password: &'static str,
    #[default("sensor")]
    mqtt_topic: &'static str,
}

#[main]
async fn main(spawner: Spawner) {
    init_logger(log::LevelFilter::Info);

    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();

    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let clocks = ClockControl::max(system.clock_control).freeze();
    let delay = Delay;

    let timer = TimerGroup::new(peripherals.TIMG1, &clocks, None).timer0;

    let timer_group0 = TimerGroup::new_async(peripherals.TIMG0, &clocks);
    embassy::init(&clocks, timer_group0);

    // possibly high transient required at init
    // https://github.com/esp-rs/esp-hal/issues/1626
    Timer::after(Duration::from_millis(1000)).await;

    let init = initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .unwrap();

    let wifi = peripherals.WIFI;
    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    // setup i2c bus
    let i2c = I2C::new_async(
        peripherals.I2C0,
        io.pins.gpio21,
        io.pins.gpio22,
        100u32.kHz(),
        &clocks,
    );

    // setup uart
    let pins = TxRxPins::new_tx_rx(
        io.pins.gpio17.into_push_pull_output(),
        io.pins.gpio16.into_floating_input(),
    );

    let uart_config = esp_hal::uart::config::Config {
        baudrate: 9600,
        data_bits: esp_hal::uart::config::DataBits::DataBits8,
        parity: esp_hal::uart::config::Parity::ParityNone,
        stop_bits: esp_hal::uart::config::StopBits::STOP1,
        ..esp_hal::uart::config::Config::default()
    };

    let mut uart = Uart::new_async_with_config(peripherals.UART2, uart_config, Some(pins), &clocks);
    uart.set_at_cmd(esp_hal::uart::config::AtCmdConfig::new(
        None, None, None, AT_CMD, None,
    ));
    uart.set_rx_fifo_full_threshold(READ_BUF_SIZE as u16)
        .unwrap();

    let sensor = Sensor::new(i2c, uart, delay).await.unwrap();

    // initialize network stack
    let mut dhcp_config = embassy_net::DhcpConfig::default();
    dhcp_config.hostname = Some(String::<32>::from_str(CONFIG.hostname).unwrap());
    let config = embassy_net::Config::dhcpv4(dhcp_config);

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = &*make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed
    ));

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(stack)).ok();
    spawner.spawn(measure(stack, sensor)).ok();
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!(
        "Start connection task, device capabilities: {:?}",
        controller.get_capabilities()
    );
    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }

        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: CONFIG.wifi_ssid.try_into().unwrap(),
                password: CONFIG.wifi_psk.try_into().unwrap(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            info!("Starting wifi");
            controller.start().await.unwrap();
            info!("Wifi started!");
        }
        info!("About to connect to {:?}...", CONFIG.wifi_ssid);

        match controller.connect().await {
            Ok(_) => info!("Wifi connected!"),
            Err(e) => {
                info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn measure(
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
    mut sensor: Sensor<I2C<'static, I2C0, Async>, Uart<'static, UART2, Async>, Delay>,
) {
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    info!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            info!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        let measurement = match sensor.measure().await {
            Ok(measurement) => measurement,
            Err(e) => {
                info!("Error taking measurement: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };
        let args = [
            #[cfg(feature = "bme280")]
            ("temperature", measurement.temperature),
            #[cfg(feature = "bme280")]
            ("humidity", measurement.humidity),
            #[cfg(feature = "bme280")]
            ("pressure", measurement.pressure),
            #[cfg(feature = "sds011")]
            ("air_quality_pm2_5", measurement.air_quality_pm2_5),
            #[cfg(feature = "sds011")]
            ("air_quality_pm10", measurement.air_quality_pm10),
        ];

        let message = match format_mqtt_message(&args) {
            Ok(message) => message,
            Err(e) => {
                info!("Error formating MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }
        };

        let mut rx_buffer = [0; 4096];
        let mut tx_buffer = [0; 4096];

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

        info!("Publishing: {:?}", message);
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

        info!("Message published");

        // Wait until next measurement
        Timer::after(Duration::from_secs(MEASUREMENT_INTERVAL_SECONDS)).await;
    }
}

fn format_mqtt_message(args: &[(&str, f32)]) -> Result<String<256>, Error> {
    let mut payload: String<256> = String::new();

    #[cfg(feature = "json")]
    {
        write!(payload, "{{\"location\": \"{}\"", CONFIG.location)?;
        for (key, value) in args {
            write!(payload, ", \"{}\": \"{:.2}\"", key, value)?;
        }
        write!(payload, "}}")?;
    }

    #[cfg(feature = "influx")]
    {
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
