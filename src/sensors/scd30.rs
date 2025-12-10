use embassy_time::Delay;
use embassy_time::{Duration, Timer};
use embedded_hal_async::i2c::I2c;
use libscd::asynchronous::scd30::Scd30 as Scd30Sensor;
use log::{info, error};

use super::{Sensor, SensorData, SensorError};
use crate::config::CONFIG;

const AMBIENT_PRESSURE: u16 = 1013;
/// Maximum number of retries for SCD30 initialization
const MAX_INIT_RETRIES: u8 = 5;
/// Maximum time to wait for sensor data to be ready (in milliseconds)
const DATA_READY_TIMEOUT_MS: u64 = 30_000;

pub struct Scd30<I2C> {
    sensor: Scd30Sensor<I2C, Delay>,
}

impl<I2C: I2c> Scd30<I2C> {
    pub async fn new(i2c: I2C) -> Result<Self, SensorError> {
        info!("Initialising Scd30...");
        let mut sensor = Scd30Sensor::new(i2c, Delay);

        Timer::after(Duration::from_millis(1000)).await;

        info!("Stopping continuous measurement...");
        let mut retries = 0;
        loop {
            match sensor.stop_continuous_measurement().await {
                Ok(_) => break,
                Err(e) => {
                    retries += 1;
                    if retries >= MAX_INIT_RETRIES {
                        error!("SCD30: Failed to stop continuous measurement after {} retries: {:?}", MAX_INIT_RETRIES, e);
                        return Err(SensorError::InitFailure);
                    }
                    info!("Error occurred: {:?}. Retry {}/{} in 5 seconds...", e, retries, MAX_INIT_RETRIES);
                    Timer::after(Duration::from_millis(5000)).await;
                }
            }
        }

        Timer::after(Duration::from_millis(1000)).await;
        sensor
            .set_measurement_interval(CONFIG.measurement_interval_seconds)
            .await
            .map_err(|e| {
                error!("SCD30: Failed to set measurement interval: {:?}", e);
                SensorError::InitFailure
            })?;

        Timer::after(Duration::from_millis(100)).await;
        sensor
            .start_continuous_measurement(AMBIENT_PRESSURE)
            .await
            .map_err(|e| {
                error!("SCD30: Failed to start continuous measurement: {:?}", e);
                SensorError::InitFailure
            })?;

        info!("Initialised Scd30");

        Ok(Self { sensor })
    }
}

impl<I2C: I2c> Sensor for Scd30<I2C> {
    async fn measure(&mut self, data: &mut SensorData) -> Result<(), SensorError> {
        // Wait until data is ready with timeout
        let start = embassy_time::Instant::now();
        let timeout = Duration::from_millis(DATA_READY_TIMEOUT_MS);

        loop {
            if start.elapsed() > timeout {
                error!("SCD30: Timeout waiting for data ready after {}ms", DATA_READY_TIMEOUT_MS);
                return Err(SensorError::MeasurementFailure);
            }

            match self.sensor.data_ready().await {
                Ok(true) => break,
                Ok(false) => Timer::after(Duration::from_millis(100)).await,
                Err(e) => {
                    error!("SCD30: Error checking data ready: {:?}", e);
                    return Err(SensorError::MeasurementFailure);
                }
            }
        }

        match self.sensor.read_measurement().await {
            Ok(sample) => {
                data.add_measurement("temperature", sample.temperature);
                data.add_measurement("humidity", sample.humidity);
                data.add_measurement("co2", sample.co2 as f32);
                Ok(())
            }
            Err(e) => {
                error!("SCD30: Error reading measurement: {:?}", e);
                Err(SensorError::MeasurementFailure)
            }
        }
    }
}
