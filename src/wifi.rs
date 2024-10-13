use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_time::{Duration, Timer};

use esp_hal::{peripherals, rng::Rng};
use esp_wifi::{
    wifi::{
        ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiStaDevice,
        WifiState,
    },
    EspWifiInitFor, EspWifiTimerSource,
};

use core::str::FromStr;
use heapless::String;
use log::info;
use rand_core::RngCore;
use static_cell::StaticCell;

use crate::config::CONFIG;

// Network stack and resources
static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
static STACK: StaticCell<Stack<WifiDevice<'_, WifiStaDevice>>> = StaticCell::new();

pub struct Wifi {
    pub stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
}

#[derive(Debug)]
pub enum Error {}

impl Wifi {
    pub async fn new(
        wifi: peripherals::WIFI,
        timer: impl EspWifiTimerSource,
        radio_clocks: peripherals::RADIO_CLK,
        mut rng: Rng,
        spawner: Spawner,
    ) -> Result<Self, Error> {
        let init = esp_wifi::init(EspWifiInitFor::Wifi, timer, rng, radio_clocks).unwrap();

        let (wifi_interface, controller) =
            esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

        // initialize network stack
        let mut dhcp_config = embassy_net::DhcpConfig::default();
        dhcp_config.hostname = Some(String::<32>::from_str(CONFIG.hostname).unwrap());

        let seed = rng.next_u32();
        let config = embassy_net::Config::dhcpv4(dhcp_config);

        let resources = RESOURCES.init(StackResources::new());
        let stack = STACK.init(Stack::new(wifi_interface, config, resources, seed.into()));

        spawner.spawn(connection(controller)).ok();
        spawner.spawn(net_task(stack)).ok();

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
        controller.get_capabilities()
    );
    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }

        if !matches!(controller.is_started(), Ok(true)) {
            info!("Connecting to wifi with SSID: {:?}", CONFIG.wifi_ssid);
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
