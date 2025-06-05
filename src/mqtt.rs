use embedded_io_async::{Read, Write};
use rust_mqtt::{
    client::{client::MqttClient, client_config::ClientConfig},
    packet::v5::publish_packet::QualityOfService,
    utils::rng_generator::CountingRng,
};

use crate::config::CONFIG;

#[derive(Debug)]
pub enum Error {
    ConnectionFailed,
    PublishMessageFailed,
}

pub struct Mqtt<'a, T: Read + Write> {
    client: MqttClient<'a, T, 5, CountingRng>,
}

impl<'a, T: Read + Write> Mqtt<'a, T> {
    pub async fn new(
        transport: T,
        tx_buffer: &'a mut [u8],
        rx_buffer: &'a mut [u8],
    ) -> Result<Self, Error> {
        let mut config: ClientConfig<5, CountingRng> = ClientConfig::new(
            rust_mqtt::client::client_config::MqttVersion::MQTTv5,
            CountingRng(20000),
        );
        config.add_username(CONFIG.mqtt_username);
        config.add_password(CONFIG.mqtt_password);

        let mut client =
            MqttClient::<T, 5, CountingRng>::new(transport, tx_buffer, 256, rx_buffer, 256, config);

        if (client.connect_to_broker().await).is_err() {
            return Err(Error::ConnectionFailed);
        }

        Ok(Self { client })
    }

    pub async fn send_message(&mut self, topic: &str, message: &[u8]) -> Result<(), Error> {
        if (self
            .client
            .send_message(topic, message, QualityOfService::QoS1, false)
            .await)
            .is_err()
        {
            return Err(Error::PublishMessageFailed);
        }

        Ok(())
    }

    pub async fn disconnect(mut self) {
        let _ = self.client.disconnect().await;
    }
}
