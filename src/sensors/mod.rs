#![allow(async_fn_in_trait)]

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use heapless::FnvIndexMap;

use crate::hal::{
    i2c::I2c,
    peripherals::{I2C0, UART2},
    uart::Uart,
    Async,
};

pub mod bme280;
pub mod scd30;
pub mod sds011;

use crate::sensors::{bme280::Bme280, scd30::Scd30, sds011::Sds011};

#[derive(Debug)]
pub enum SensorError {
    InitFailure,
    MeasurementFailure,
    Bme280NoTemperatureData,
    Bme280NoHumidityData,
    Bme280NoPressureData,
}

#[derive(Default, Debug)]
pub struct SensorData {
    pub data: FnvIndexMap<&'static str, f32, 16>,
}

impl SensorData {
    pub fn add_measurement(&mut self, key: &'static str, value: f32) {
        self.data.insert(key, value).ok();
    }
}

pub trait Sensor {
    async fn measure(&mut self, data: &mut SensorData) -> Result<(), SensorError>;
}

pub struct Sensors {
    pub bme280: Option<Bme280<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>>>,
    pub sds011: Option<Sds011<Uart<'static, UART2, Async>>>,
    pub scd30: Option<Scd30<I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>>>,
}

impl Default for Sensors {
    fn default() -> Self {
        Self::new()
    }
}

impl Sensors {
    pub fn new() -> Self {
        Self {
            bme280: None,
            sds011: None,
            scd30: None,
        }
    }

    pub async fn new_bme280(
        &mut self,
        i2c: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ) -> Result<(), SensorError> {
        self.bme280 = Some(Bme280::new(i2c).await?);
        Ok(())
    }

    pub async fn new_scd30(
        &mut self,
        i2c: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ) -> Result<(), SensorError> {
        self.scd30 = Some(Scd30::new(i2c).await?);
        Ok(())
    }

    pub async fn new_sds011(
        &mut self,
        uart: Uart<'static, UART2, Async>,
    ) -> Result<(), SensorError> {
        self.sds011 = Some(Sds011::new(uart).await?);
        Ok(())
    }

    pub async fn measure(&mut self) -> Result<SensorData, SensorError> {
        let mut sensor_data = SensorData::default();

        if let Some(ref mut bme280) = self.bme280 {
            bme280.measure(&mut sensor_data).await?;
        }

        if let Some(ref mut scd30) = self.scd30 {
            scd30.measure(&mut sensor_data).await?;
        }

        if let Some(ref mut sds011) = self.sds011 {
            sds011.measure(&mut sensor_data).await?;
        }

        Ok(sensor_data)
    }
}
