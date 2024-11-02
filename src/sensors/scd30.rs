use embassy_time::Delay;
use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;
use libscd::asynchronous::scd30::Scd30 as Scd30Sensor;
use log::info;

use super::{Sensor, SensorData, SensorError};
use crate::config::CONFIG;

const AMBIENT_PRESSURE: u16 = 1013;

pub struct Scd30<I2C> {
    sensor: Scd30Sensor<I2C, Delay>,
}

impl<I2C: I2c> Scd30<I2C> {
    pub async fn new(i2c: I2C) -> Result<Self, SensorError> {
        info!("Initialising Scd30...");
        let mut sensor = Scd30Sensor::new(i2c, Delay);

        Timer::after(Duration::from_millis(1000)).await;

        info!("Stopping continuous measurement...");
        loop {
            match sensor.stop_continuous_measurement().await {
                Ok(_) => break,
                Err(e) => {
                    info!("Error occurred: {:?}. Retrying in 5 seconds...", e);
                    Timer::after(Duration::from_millis(5000)).await;
                }
            }
        }

        Timer::after(Duration::from_millis(1000)).await;
        sensor
            .set_measurement_interval(CONFIG.measurement_interval_seconds)
            .await
            .map_err(|_| SensorError::InitFailure)
            .ok();

        Timer::after(Duration::from_millis(100)).await;
        sensor
            .start_continuous_measurement(AMBIENT_PRESSURE)
            .await
            .map_err(|_| SensorError::InitFailure)
            .ok();

        info!("Initialised Scd30");

        Ok(Self { sensor })
    }
}

impl<I2C: I2c> Sensor for Scd30<I2C> {
    async fn measure(&mut self, data: &mut SensorData) -> Result<(), SensorError> {
        while !self.sensor.data_ready().await.unwrap() {
            Timer::after(Duration::from_millis(100)).await;
        }

        match self.sensor.measurement().await {
            Ok(sample) => {
                data.add_measurement("temperature", sample.temperature);
                data.add_measurement("humidity", sample.humidity);
                data.add_measurement("co2", sample.co2 as f32);
                Ok(())
            }
            Err(_) => Err(SensorError::MeasurementFailure),
        }
    }
}
