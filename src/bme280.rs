use log::info;

use embassy_time::Delay;
use embedded_hal_async::i2c::I2c;

use bme280_rs::{AsyncBme280, Oversampling, SensorMode};

#[derive(Debug)]
pub enum Error {}

#[derive(Debug)]
pub struct Bme280Measurement {
    pub humidity: f32,
    pub pressure: f32,
    pub temperature: f32,
}

pub struct Bme280<I2C> {
    sensor: AsyncBme280<I2C, Delay>,
}

impl<I2C> Bme280<I2C>
where
    I2C: I2c,
{
    pub async fn new(i2c: I2C) -> Result<Self, Error> {
        info!("Initialising BME280...");
        let mut sensor = AsyncBme280::new(i2c, Delay);
        sensor.init().await.unwrap();
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
        let sample = self.sensor.read_sample().await.unwrap();

        Ok(Bme280Measurement {
            temperature: sample.temperature.unwrap(),
            humidity: sample.humidity.unwrap(),
            pressure: sample.pressure.unwrap(),
        })
    }
}
