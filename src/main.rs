#![no_std]
#![no_main]
#![feature(generic_arg_infer)]
#![feature(type_alias_impl_trait)]

use core::fmt::Write;
use core::str::FromStr;
use heapless::String;
use static_cell::make_static;

use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Stack, StackResources};
use embassy_time::{Duration, Timer};

use esp_backtrace as _;
use esp_hal as hal;
use esp_println::logger::init_logger;
use esp_println::println;
use esp_wifi::wifi::{ClientConfiguration, Configuration};
use esp_wifi::wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState};
use esp_wifi::{initialize, EspWifiInitFor};

use hal::clock::ClockControl;
use hal::gpio::IO;
use hal::i2c::I2C;
use hal::Delay;
use hal::Rng;
use hal::{
    embassy::{self},
    peripherals::Peripherals,
    prelude::*,
    timer::TimerGroup,
};

use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::publish_packet::QualityOfService,
    utils::rng_generator::CountingRng,
};

mod sensor;

use sensor::Sensor;

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
    let delay = Delay::new(&clocks);

    let timer = TimerGroup::new(peripherals.TIMG1, &clocks).timer0;

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

    let timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    embassy::init(&clocks, timer_group0);

    // setup i2c bus
    let i2c = I2C::new(
        peripherals.I2C0,
        io.pins.gpio21,
        io.pins.gpio22,
        100u32.kHz(),
        &clocks,
    );

    let sensor: Sensor = Sensor::new(i2c, delay);

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
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
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
            println!("Starting wifi");
            controller.start().await.unwrap();
            println!("Wifi started!");
        }
        println!("About to connect to {:?}...", CONFIG.wifi_ssid);

        match controller.connect().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
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
async fn measure(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>, mut sensor: Sensor) {
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        let mut rx_buffer = [0; 4096];
        let mut tx_buffer = [0; 4096];

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        socket.set_timeout(Some(embassy_time::Duration::from_secs(60)));

        let host_addr = match stack
            .dns_query(CONFIG.mqtt_hostname, DnsQueryType::A)
            .await
            .map(|a| a[0])
        {
            Ok(address) => address,
            Err(e) => {
                println!("DNS lookup for MQTT host failed with error: {e:?}");
                continue;
            }
        };

        let socket_addr = (host_addr, CONFIG.mqtt_port);

        println!("Connecting to MQTT server...");
        let r = socket.connect(socket_addr).await;
        if let Err(e) = r {
            println!("Connect error: {e:?}");
            continue;
        }
        println!("Connected to MQTT server");

        println!("Initialising MQTT connection");
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
            println!("Couldn't connect to MQTT broker: {e:?}");
            Timer::after(Duration::from_secs(10)).await;
            continue;
        }

        loop {
            let measurement = match sensor.measure() {
                Ok(measurement) => measurement,
                Err(e) => {
                    println!("Error taking measurement: {e:?}");
                    Timer::after(Duration::from_secs(10)).await;
                    continue;
                }
            };

            println!(
                "Measured {:.2} C, {:.2} % RH, {:.2} Pa",
                measurement.temperature, measurement.humidity, measurement.pressure
            );

            let mut data: String<128> = String::new();

            #[cfg(feature = "influx")]
            if let Err(e) = write!(
                &mut data,
                "weather,location={} temperature={:.2},humidity={:.2},pressure={:.2}",
                CONFIG.location,
                measurement.temperature,
                measurement.humidity,
                measurement.pressure
            ) {
                println!("Error generating MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }

            #[cfg(feature = "json")]
            if let Err(e) = write!(
                &mut data,
                "{{ \"location\": \"{}\", \"temperature\": {:.2},\"humidity\": {:.2}, \"pressure\": {:.2} }}",
                CONFIG.location,
                measurement.temperature,
                measurement.humidity,
                measurement.pressure
            ) {
                println!("Error generating MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                continue;
            }

            println!("Publishing: {:?}", data);

            if let Err(e) = client
                .send_message(
                    CONFIG.mqtt_topic,
                    data.as_bytes(),
                    QualityOfService::QoS0,
                    false,
                )
                .await
            {
                println!("Error publishing MQTT message: {e:?}");
                Timer::after(Duration::from_secs(10)).await;
                break;
            }

            println!("Message published");

            // take measurement every 30seconds
            Timer::after(Duration::from_secs(30)).await;
        }
    }
}
