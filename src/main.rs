#![no_std]
#![no_main]
#![feature(generic_arg_infer)]
#![feature(type_alias_impl_trait)]

use core::fmt::Write;
use heapless::String;

use embassy_executor::Executor;
use embassy_executor::_export::StaticCell;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Ipv4Address, Stack, StackResources};
use embassy_time::{Duration, Timer};
use embedded_svc::wifi::{ClientConfiguration, Configuration, Wifi};

use esp32_hal as hal;
use esp_backtrace as _;
use esp_println::logger::init_logger;
use esp_println::println;
use esp_wifi::initialize;
use esp_wifi::wifi::{WifiController, WifiDevice, WifiEvent, WifiMode, WifiState};

use hal::clock::{ClockControl, CpuClock};
use hal::gpio::IO;
use hal::i2c::I2C;
use hal::Delay;
use hal::Rng;
use hal::{embassy, peripherals::Peripherals, prelude::*, timer::TimerGroup, Rtc};
use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::publish_packet::QualityOfService,
    utils::rng_generator::CountingRng,
};

use shared_bus::BusManagerSimple;

mod sensor;

use sensor::Sensor;

#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_psk: &'static str,
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

macro_rules! singleton {
    ($val:expr) => {{
        type T = impl Sized;
        static STATIC_CELL: StaticCell<T> = StaticCell::new();
        let (x,) = STATIC_CELL.init(($val,));
        x
    }};
}

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

#[entry]
fn main() -> ! {
    init_logger(log::LevelFilter::Info);

    let peripherals = Peripherals::take();
    let mut system = peripherals.DPORT.split();

    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let clocks = ClockControl::configure(system.clock_control, CpuClock::Clock240MHz).freeze();
    let delay = Delay::new(&clocks);

    let timg0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    let mut wdt = timg0.wdt;
    let mut rtc = Rtc::new(peripherals.RTC_CNTL);

    // Disable MWDT and RWDT (Watchdog) flash boot protection
    wdt.disable();
    rtc.rwdt.disable();

    {
        let timg1 = TimerGroup::new(peripherals.TIMG1, &clocks);
        initialize(
            timg1.timer0,
            Rng::new(peripherals.RNG),
            system.radio_clock_control,
            &clocks,
        )
        .unwrap();
    }

    let (wifi, _) = peripherals.RADIO.split();
    let (wifi_interface, controller) = esp_wifi::wifi::new_with_mode(wifi, WifiMode::Sta);

    embassy::init(&clocks, timg0.timer0);

    unsafe {
        // setup i2c bus
        let i2c = I2C::new(
            peripherals.I2C0,
            io.pins.gpio21,
            io.pins.gpio22,
            100u32.kHz(),
            &mut system.peripheral_clock_control,
            &clocks,
        );

        let bus = BusManagerSimple::new(i2c);
        let sensor = Sensor::new(core::mem::transmute(bus.acquire_i2c()), delay);

        let config = embassy_net::Config::Dhcp(Default::default());

        let seed = 1234; // very random, very secure seed

        // initialize network stack
        let stack = &*singleton!(Stack::new(
            wifi_interface,
            config,
            singleton!(StackResources::<3>::new()),
            seed
        ));

        let executor = EXECUTOR.init(Executor::new());
        executor.run(|spawner| {
            spawner.spawn(connection(controller)).ok();
            spawner.spawn(net_task(&stack)).ok();
            spawner.spawn(measure(&stack, sensor)).ok();
        });
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("Start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::StaConnected => {
                // wait until we're no longer connected
                controller.wait_for_event(WifiEvent::StaDisconnected).await;
                Timer::after(Duration::from_millis(5000)).await
            }
            _ => {}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: CONFIG.wifi_ssid.into(),
                password: CONFIG.wifi_psk.into(),
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
async fn net_task(stack: &'static Stack<WifiDevice<'static>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn measure(stack: &'static Stack<WifiDevice<'static>>, mut sensor: Sensor) {
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        Timer::after(Duration::from_millis(1_000)).await;

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        socket.set_timeout(Some(embassy_net::SmolDuration::from_secs(60)));

        // TODO: parse IP from config
        let ip_addr = Ipv4Address::new(192, 168, 94, 27);

        let socket_addr = (ip_addr, CONFIG.mqtt_port);

        println!("Connecting to MQTT server...");
        let r = socket.connect(socket_addr).await;
        if let Err(e) = r {
            println!("Connect error: {:?}", e);
            continue;
        }
        println!("Connected to MQTT server");

        println!("Initialising MQTT connection");
        let mut mqtt_rx_buffer = [0; 256];
        let mut mqtt_tx_buffer = [0; 256];
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
        client.connect_to_broker().await.unwrap();

        loop {
            match sensor.measure() {
                Ok(measurement) => {
                    println!(
                        "Measured {:.2}C, {:.2}% RH, {:.2}hPa",
                        measurement.temperature, measurement.humidity, measurement.pressure
                    );

                    let mut data: String<128> = String::new();
                    write!(
                        &mut data,
                        "weather,location={} temperature={:.2},humidity={:.2},pressure={:.2}",
                        CONFIG.location,
                        measurement.temperature,
                        measurement.humidity,
                        measurement.pressure
                    )
                    .unwrap();

                    println!("Publishing: {:?}", data);
                    client
                        .send_message(
                            CONFIG.mqtt_topic,
                            data.as_bytes(),
                            QualityOfService::QoS0,
                            false,
                        )
                        .await
                        .unwrap();

                    println!("Message published");
                }
                Err(e) => println!("Error {:?}", e),
            }

            // take measurement every 30seconds
            Timer::after(Duration::from_secs(30)).await;
        }
    }
}
