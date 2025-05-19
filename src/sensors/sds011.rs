use embedded_io_async::{Read, Write};
use log::info;
use sds011_nostd_rs::{
    Config as Sds011Config, DeviceID as Sds011DeviceID, DeviceMode as Sds011DeviceMode,
    Sds011 as Sds011Sensor,
};

use super::{Sensor, SensorData, SensorError};

pub struct Sds011<S> {
    sensor: Sds011Sensor<S>,
}

impl<S: Read + Write> Sds011<S> {
    pub async fn new(serial: S) -> Result<Self, SensorError> {
        info!("Initialising SDS011...");
        let mut sensor = Sds011Sensor::new(
            serial,
            Sds011Config {
                id: Sds011DeviceID {
                    id1: 0xFF,
                    id2: 0xFF,
                },
                mode: Sds011DeviceMode::Active,
            },
        );

        sensor
            .init()
            .await
            .map_err(|_| SensorError::InitFailure)
            .ok();

        info!("Initialised SDS011");

        Ok(Self { sensor })
    }
}

impl<S: Read + Write> Sensor for Sds011<S> {
    async fn measure(&mut self, data: &mut SensorData) -> Result<(), SensorError> {
        match self.sensor.read_sample().await {
            Ok(sample) => {
                data.add_measurement("air_quality_pm2_5", sample.pm2_5);
                data.add_measurement("air_quality_pm10", sample.pm10);
                Ok(())
            }
            Err(_) => Err(SensorError::MeasurementFailure),
        }
    }
}
