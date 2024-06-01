use log::info;

use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;

use bme280_rs::{AsyncBme280, Oversampling, SensorMode};

#[derive(Debug)]
pub enum Error {
    InitFailure,
    MeasurementFailure,
}

#[derive(Debug)]
pub struct Bme280Measurement {
    pub humidity: f32,
    pub pressure: f32,
    pub temperature: f32,
}

pub struct Bme280<I2C, D> {
    sensor: AsyncBme280<I2C, D>,
}

impl<I2C, D> Bme280<I2C, D>
where
    I2C: I2c,
    D: DelayNs,
{
    pub async fn new(i2c: I2C, delay: D) -> Result<Self, Error> {
        info!("Initialising BME280...");
        let mut sensor = AsyncBme280::new(i2c, delay);
        sensor.init().await.map_err(|_| Error::InitFailure).ok();
        sensor
            .set_sampling_configuration(
                bme280_rs::Configuration::default()
                    .with_temperature_oversampling(Oversampling::Oversample1)
                    .with_pressure_oversampling(Oversampling::Oversample1)
                    .with_humidity_oversampling(Oversampling::Oversample1)
                    .with_sensor_mode(SensorMode::Normal),
            )
            .await
            .ok();

        info!("Initialised BME280");

        Ok(Self { sensor })
    }

    pub async fn measure(&mut self) -> Result<Bme280Measurement, Error> {
        match self.sensor.read_sample().await {
            Ok(sample) => {
                if let (Some(temperature), Some(pressure), Some(humidity)) =
                    (sample.temperature, sample.pressure, sample.humidity)
                {
                    Ok(Bme280Measurement {
                        temperature,
                        humidity,
                        pressure,
                    })
                } else {
                    Err(Error::MeasurementFailure)
                }
            }
            Err(_) => Err(Error::MeasurementFailure),
        }
    }
}
