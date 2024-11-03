use bme280_rs::{AsyncBme280, Oversampling, SensorMode};
use embassy_time::Delay;
use log::info;

use super::{Sensor, SensorData, SensorError};

pub struct Bme280<I2c> {
    sensor: AsyncBme280<I2c, Delay>,
}

impl<I2C: embedded_hal_async::i2c::I2c> Bme280<I2C> {
    pub async fn new(i2c: I2C) -> Result<Self, SensorError> {
        info!("Initialising BME280...");
        let mut sensor = AsyncBme280::new(i2c, Delay);
        sensor.init().await.map_err(|_| SensorError::InitFailure)?;

        sensor
            .set_sampling_configuration(
                bme280_rs::Configuration::default()
                    .with_temperature_oversampling(Oversampling::Oversample1)
                    .with_pressure_oversampling(Oversampling::Oversample1)
                    .with_humidity_oversampling(Oversampling::Oversample1)
                    .with_sensor_mode(SensorMode::Normal),
            )
            .await
            .map_err(|_| SensorError::InitFailure)?;

        info!("Initialised BME280");

        Ok(Self { sensor })
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> Sensor for Bme280<I2C> {
    async fn measure(&mut self, data: &mut SensorData) -> Result<(), SensorError> {
        match self.sensor.read_sample().await {
            Ok(sample) => {
                data.add_measurement(
                    "temperature",
                    sample
                        .temperature
                        .ok_or(SensorError::Bme280NoTemperatureData)?,
                );
                data.add_measurement(
                    "humidity",
                    sample.humidity.ok_or(SensorError::Bme280NoHumidityData)?,
                );
                data.add_measurement(
                    "pressure",
                    sample.pressure.ok_or(SensorError::Bme280NoPressureData)?,
                );
                Ok(())
            }
            Err(_) => Err(SensorError::MeasurementFailure),
        }
    }
}
