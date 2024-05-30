#[cfg(feature = "bme280")]
use bme280_rs::{AsyncBme280, Oversampling, SensorMode};

use core::marker::PhantomData;
use log::info;

use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use embedded_io_async::{Read, Write};

#[cfg(feature = "sds011")]
use sds011_nostd_rs::{
    Config as Sds011Config, DeviceID as Sds011DeviceID, DeviceMode as Sds011DeviceMode, Sds011,
};

pub struct Sensor<I2C, S, D> {
    #[cfg(feature = "bme280")]
    bme280: AsyncBme280<I2C, D>,
    #[cfg(feature = "sds011")]
    sds011: Sds011<S>,
    // Placeholder to ensure the type parameters are used
    _marker: PhantomData<(I2C, S, D)>,
}

#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "bme280")]
    Bme280InitFailure,
    #[cfg(feature = "bme280")]
    Bme280Measurement,

    #[cfg(feature = "sds011")]
    Sds011InitFailure,
    #[cfg(feature = "sds011")]
    Sds011Measurement,
}

#[derive(Debug)]
pub struct Measurement {
    #[cfg(feature = "bme280")]
    pub humidity: f32,
    #[cfg(feature = "bme280")]
    pub pressure: f32,
    #[cfg(feature = "bme280")]
    pub temperature: f32,

    #[cfg(feature = "sds011")]
    pub air_quality_pm2_5: f32,
    #[cfg(feature = "sds011")]
    pub air_quality_pm10: f32,
}

impl<I2C, S, D> Sensor<I2C, S, D>
where
    I2C: I2c,
    S: Read + Write,
    D: DelayNs,
{
    pub async fn new(i2c: I2C, uart: S, delay: D) -> Result<Self, Error> {
        info!("Initialising sensors...");

        #[cfg(feature = "bme280")]
        let mut bme280 = AsyncBme280::new(i2c, delay);
        #[cfg(feature = "bme280")]
        bme280
            .init()
            .await
            .map_err(|_| Error::Bme280InitFailure)
            .ok();
        #[cfg(feature = "bme280")]
        bme280
            .set_sampling_configuration(
                bme280_rs::Configuration::default()
                    .with_temperature_oversampling(Oversampling::Oversample1)
                    .with_pressure_oversampling(Oversampling::Oversample1)
                    .with_humidity_oversampling(Oversampling::Oversample1)
                    .with_sensor_mode(SensorMode::Normal),
            )
            .await
            .ok();

        #[cfg(feature = "bme280")]
        info!("Initialised BME280");

        #[cfg(feature = "sds011")]
        let mut sds011 = Sds011::new(
            uart,
            Sds011Config {
                id: Sds011DeviceID {
                    id1: 0xFF,
                    id2: 0xFF,
                },
                mode: Sds011DeviceMode::Passive,
            },
        );
        #[cfg(feature = "sds011")]
        sds011
            .init()
            .await
            .map_err(|_| Error::Sds011InitFailure)
            .ok();

        #[cfg(feature = "sds011")]
        info!("Initialised SDS011");

        Ok(Self {
            #[cfg(feature = "bme280")]
            bme280,

            #[cfg(feature = "sds011")]
            sds011,

            _marker: PhantomData,
        })
    }

    #[cfg(feature = "bme280")]
    pub async fn measure_environment(&mut self) -> Result<(f32, f32, f32), Error> {
        match self.bme280.read_sample().await {
            Ok(sample) => {
                if let (Some(temperature), Some(pressure), Some(humidity)) =
                    (sample.temperature, sample.pressure, sample.humidity)
                {
                    Ok((temperature, humidity, pressure))
                } else {
                    Err(Error::Bme280Measurement)
                }
            }
            Err(_) => Err(Error::Bme280Measurement),
        }
    }

    #[cfg(feature = "sds011")]
    pub async fn measure_air_quality(&mut self) -> Result<(f32, f32), Error> {
        match self.sds011.read_sample().await {
            Ok(data) => Ok((data.pm2_5, data.pm10)),
            Err(_) => Err(Error::Sds011Measurement),
        }
    }

    pub async fn measure(&mut self) -> Result<Measurement, Error> {
        #[cfg(feature = "bme280")]
        let (temperature, humidity, pressure) = self.measure_environment().await?;

        #[cfg(feature = "sds011")]
        let (pm2_5, pm10) = self.measure_air_quality().await?;

        Ok(Measurement {
            #[cfg(feature = "bme280")]
            temperature,
            #[cfg(feature = "bme280")]
            humidity,
            #[cfg(feature = "bme280")]
            pressure,

            #[cfg(feature = "sds011")]
            air_quality_pm2_5: pm2_5,
            #[cfg(feature = "sds011")]
            air_quality_pm10: pm10,
        })
    }
}
