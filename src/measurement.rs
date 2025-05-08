use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use esp_mbedtls::Tls;
use heapless::String;
use static_cell::StaticCell;

use crate::config::CONFIG;
use crate::constants::*;
use crate::mqtt::Mqtt;
use crate::sensors::{SensorData, Sensors};
use crate::transport::Transport;

static MQTT_RX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; MQTT_RX_BUFFER_SIZE]>> = StaticCell::new();
static MQTT_TX_BUF: StaticCell<Mutex<NoopRawMutex, [u8; MQTT_TX_BUFFER_SIZE]>> = StaticCell::new();

#[derive(Debug)]
pub enum Error {
    Sensor,
    Transport,
    Mqtt,
    Format,
}

pub struct Measurement {
    stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
    tls: &'static Tls<'static>,
    rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
    tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
    mqtt_rx_buf: &'static Mutex<NoopRawMutex, [u8; MQTT_RX_BUFFER_SIZE]>,
    mqtt_tx_buf: &'static Mutex<NoopRawMutex, [u8; MQTT_TX_BUFFER_SIZE]>,
    sensors: Sensors,
}

impl Measurement {
    pub fn new(
        stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
        tls: &'static Tls<'static>,
        rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
        tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
        sensors: Sensors,
    ) -> Result<Self, Error> {
        let mqtt_rx_buf = MQTT_RX_BUF.init(Mutex::new([0; MQTT_RX_BUFFER_SIZE]));
        let mqtt_tx_buf = MQTT_TX_BUF.init(Mutex::new([0; MQTT_TX_BUFFER_SIZE]));

        Ok(Self {
            stack,
            tls,
            rx_buf,
            tx_buf,
            mqtt_rx_buf,
            mqtt_tx_buf,
            sensors,
        })
    }

    pub async fn take(&mut self) -> Result<(), Error> {
        // Measure sensor data first
        let sensor_data = self.sensors.measure().await.map_err(|_| Error::Sensor)?;
        log::debug!("Sensor data received: {:?}", sensor_data);

        // Format MQTT message
        let message = format_mqtt_message(&sensor_data).map_err(|_| Error::Format)?;
        log::debug!("Formatted MQTT message: {}", message);

        // Acquire locks for shared resources only when needed
        let stack_guard = self.stack.lock().await;
        let mut rx_buf = self.rx_buf.lock().await;
        let mut tx_buf = self.tx_buf.lock().await;

        // Create transport session
        let transport = Transport::new(
            *stack_guard,
            self.tls,
            &mut *rx_buf,
            &mut *tx_buf,
            CONFIG.mqtt_hostname,
            CONFIG.mqtt_port,
        )
        .await
        .map_err(|_| Error::Transport)?;

        // Create MQTT client
        let mut mqtt_rx_buf = self.mqtt_rx_buf.lock().await;
        let mut mqtt_tx_buf = self.mqtt_tx_buf.lock().await;
        let mut mqtt = Mqtt::new(transport, &mut *mqtt_tx_buf, &mut *mqtt_rx_buf)
            .await
            .map_err(|_| Error::Mqtt)?;

        // Publish MQTT message
        mqtt.send_message(CONFIG.mqtt_topic, message.as_bytes())
            .await
            .map_err(|_| Error::Mqtt)?;

        // Explicitly disconnect
        mqtt.disconnect().await;

        log::info!("MQTT data published successfully");
        Ok(())
    }
}

fn format_mqtt_message(sensor_data: &SensorData) -> Result<String<256>, core::fmt::Error> {
    use core::fmt::Write;
    let mut payload: String<256> = String::new();

    #[cfg(feature = "json")]
    {
        write!(payload, "{{\"location\": \"{}\"", CONFIG.location)?;
        for (key, value) in sensor_data.data.iter() {
            write!(payload, ", \"{}\": \"{:.2}\"", key, value)?;
        }
        write!(payload, "}}")?;
    }

    #[cfg(feature = "influx")]
    {
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
