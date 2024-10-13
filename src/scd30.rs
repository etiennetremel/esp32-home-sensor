use log::info;

use embassy_time::Delay;
use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;

use libscd::asynchronous::scd30::Scd30 as Scd30Sensor;

use crate::config::CONFIG;

const AMBIENT_PRESSURE: u16 = 1013;

#[derive(Debug)]
pub enum Error {}

#[derive(Debug)]
pub struct Scd30Measurement {
    pub humidity: f32,
    pub co2: f32,
    pub temperature: f32,
}

pub struct Scd30<I2C> {
    sensor: Scd30Sensor<I2C, Delay>,
}

impl<I2C> Scd30<I2C>
where
    I2C: I2c,
{
    pub async fn new(i2c: I2C) -> Result<Self, Error> {
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

        info!("Reading firmware version");
        let (major, minor) = sensor.read_firmware_version().await.unwrap();

        info!("Using Scd30 with firmware {major}.{minor}");

        Timer::after(Duration::from_millis(1000)).await;
        sensor
            .set_measurement_interval(CONFIG.measurement_interval_seconds)
            .await
            .unwrap();
        Timer::after(Duration::from_millis(100)).await;
        sensor
            .start_continuous_measurement(AMBIENT_PRESSURE)
            .await
            .unwrap();

        info!("Initialised Scd30");

        Ok(Self { sensor })
    }

    pub async fn measure(&mut self) -> Result<Scd30Measurement, Error> {
        while !self.sensor.data_ready().await.unwrap() {
            Timer::after(Duration::from_millis(100)).await;
        }

        let m = self.sensor.measurement().await.unwrap();

        Ok(Scd30Measurement {
            temperature: m.temperature,
            humidity: m.humidity,
            co2: m.co2 as f32,
        })
    }
}
