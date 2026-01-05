use embedded_io_async::{Read, Write};
use rust_mqtt::{
    buffer::AllocBuffer,
    client::{
        options::{ConnectOptions, DisconnectOptions, PublicationOptions},
        Client,
    },
    config::KeepAlive,
    types::{MqttBinary, MqttString, QoS, TopicName},
    Bytes,
};

use crate::config::CONFIG;
use crate::constants::{MQTT_MAX_SUBSCRIBES, MQTT_RECEIVE_MAXIMUM, MQTT_SEND_MAXIMUM};

#[derive(Debug)]
pub enum Error {
    ConnectionFailed,
    PublishMessageFailed,
}

pub struct Mqtt<'a, T>
where
    T: Read + Write,
{
    client: Client<'a, T, AllocBuffer, MQTT_MAX_SUBSCRIBES, MQTT_RECEIVE_MAXIMUM, MQTT_SEND_MAXIMUM>,
}

impl<'a, T> Mqtt<'a, T>
where
    T: Read + Write,
{
    pub async fn new(transport: T, buffer: &'a mut AllocBuffer) -> Result<Self, Error> {
        let mut client =
            Client::<'_, T, AllocBuffer, MQTT_MAX_SUBSCRIBES, MQTT_RECEIVE_MAXIMUM, MQTT_SEND_MAXIMUM>::new(buffer);

        let connect_options = ConnectOptions {
            clean_start: true,
            keep_alive: KeepAlive::Seconds(30),
            session_expiry_interval: Default::default(),
            user_name: Some(
                MqttString::try_from(CONFIG.mqtt_username).map_err(|_| Error::ConnectionFailed)?,
            ),
            password: Some(
                MqttBinary::try_from(CONFIG.mqtt_password).map_err(|_| Error::ConnectionFailed)?,
            ),
            will: None,
        };

        let client_id =
            MqttString::try_from(CONFIG.device_id).map_err(|_| Error::ConnectionFailed)?;

        match client
            .connect(transport, &connect_options, Some(client_id))
            .await
        {
            Ok(_) => {
                log::info!("MQTT connected to broker successfully");
            }
            Err(e) => {
                log::error!("MQTT connect_to_broker failed: {:?}", e);
                return Err(Error::ConnectionFailed);
            }
        }

        Ok(Self { client })
    }

    pub async fn send_message(&mut self, topic: &str, message: &[u8]) -> Result<(), Error> {
        let topic_str = MqttString::try_from(topic).map_err(|_| Error::PublishMessageFailed)?;
        // SAFETY: Topic names from config are valid (no wildcards)
        let topic_name = unsafe { TopicName::new_unchecked(topic_str) };

        let pub_options = PublicationOptions {
            retain: false,
            topic: topic_name,
            qos: QoS::AtLeastOnce,
        };

        match self
            .client
            .publish(&pub_options, Bytes::from(message))
            .await
        {
            Ok(_) => {
                match self.client.poll().await {
                    Ok(_) => {
                        log::debug!("Message published and acknowledged");
                    }
                    Err(e) => {
                        log::warn!("Failed to receive publish acknowledgment: {:?}", e);
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to publish message: {:?}", e);
                return Err(Error::PublishMessageFailed);
            }
        }

        Ok(())
    }

    pub async fn disconnect(mut self) {
        let disconnect_options = DisconnectOptions {
            publish_will: false,
            session_expiry_interval: None,
        };
        let _ = self.client.disconnect(&disconnect_options).await;
    }
}
