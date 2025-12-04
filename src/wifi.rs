use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer, with_timeout};

use esp_hal::rng::Rng;
use esp_radio::{
    wifi::{ClientConfig, Config, ModeConfig, WifiController, WifiDevice, WifiEvent, WifiStaState},
    Controller,
};

use core::str::FromStr;
use heapless::String;
use log::info;
use static_cell::StaticCell;

use crate::config::CONFIG;

static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

pub struct Wifi {
    pub stack: Stack<'static>,
}

#[derive(Debug)]
pub enum Error {}

impl Wifi {
    pub async fn new(
        init: &'static Controller<'static>,
        wifi: esp_hal::peripherals::WIFI<'static>,
        rng: Rng,
        spawner: Spawner,
    ) -> Result<Self, Error> {
        let (controller, interfaces) = esp_radio::wifi::new(init, wifi, Config::default()).unwrap();

        let mut dhcp_config = embassy_net::DhcpConfig::default();
        dhcp_config.hostname = Some(String::<32>::from_str(CONFIG.device_id).unwrap());

        let seed = (rng.random() as u64) << 32 | rng.random() as u64;
        let config = embassy_net::Config::dhcpv4(dhcp_config);

        let resources = RESOURCES.init(StackResources::new());
        let (stack, runner) = embassy_net::new(interfaces.sta, config, resources, seed);

        spawner.spawn(connection(controller)).ok();
        spawner.spawn(net_task(runner)).ok();

        Ok(Self { stack })
    }

    pub async fn connect(&self) -> Result<(), Error> {
        info!("Waiting for network stack to be ready...");
        loop {
            if self.stack.is_link_up() && self.stack.is_config_up() {
                break;
            }
            Timer::after(Duration::from_millis(500)).await;
        }

        info!("Waiting to get IP address...");
        loop {
            if let Some(config) = self.stack.config_v4() {
                info!("Got IP: {}", config.address);
                break;
            }
            Timer::after(Duration::from_millis(500)).await;
        }

        Ok(())
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!(
        "Start connection task, device capabilities: {:?}",
        controller.capabilities()
    );
    loop {
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }

        if !matches!(controller.is_started(), Ok(true)) {
            info!("Connecting to wifi with SSID: {:?}", CONFIG.wifi_ssid);
            let client_config = ClientConfig::default()
                .with_ssid(CONFIG.wifi_ssid.into())
                .with_password(CONFIG.wifi_psk.into());
            let config = ModeConfig::Client(client_config);
            controller.set_config(&config).unwrap();
            info!("Starting wifi");
            controller.start_async().await.unwrap();
            info!("Wifi started!");
        }

        info!("About to connect to {:?}...", CONFIG.wifi_ssid);
        match with_timeout(Duration::from_secs(10), controller.connect_async()).await {
            Ok(Ok(_)) => info!("Wifi connected!"),
            Ok(Err(e)) => {
                info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
            Err(_) => {
                info!("Wifi connection timed out");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
