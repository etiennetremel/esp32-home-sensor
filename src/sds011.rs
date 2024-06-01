use log::info;

use embedded_io_async::{Read, Write};

use sds011_nostd_rs::{
    Config as Sds011Config, DeviceID as Sds011DeviceID, DeviceMode as Sds011DeviceMode,
    Sds011 as Sds011Sensor,
};

#[derive(Debug)]
pub enum Error {
    InitFailure,
    MeasurementFailure,
}

#[derive(Debug)]
pub struct Sds011Measurement {
    pub pm2_5: f32,
    pub pm10: f32,
}

pub struct Sds011<S> {
    sensor: Sds011Sensor<S>,
}

impl<S> Sds011<S>
where
    S: Read + Write,
{
    pub async fn new(serial: S) -> Result<Self, Error> {
        info!("Initialising SDS011...");

        let mut sensor = Sds011Sensor::new(
            serial,
            Sds011Config {
                id: Sds011DeviceID {
                    id1: 0xFF,
                    id2: 0xFF,
                },
                mode: Sds011DeviceMode::Passive,
            },
        );

        sensor.init().await.map_err(|_| Error::InitFailure).ok();

        info!("Initialised SDS011");

        Ok(Self { sensor })
    }

    pub async fn measure(&mut self) -> Result<Sds011Measurement, Error> {
        match self.sensor.read_sample().await {
            Ok(data) => Ok(Sds011Measurement {
                pm2_5: data.pm2_5,
                pm10: data.pm10,
            }),
            Err(_) => Err(Error::MeasurementFailure),
        }
    }
}
