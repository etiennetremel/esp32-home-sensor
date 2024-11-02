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
        self.bme280 = Some(Bme280::new(i2c).await.unwrap());
        Ok(())
    }

    pub async fn new_scd30(
        &mut self,
        i2c: I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>,
    ) -> Result<(), SensorError> {
        self.scd30 = Some(Scd30::new(i2c).await.unwrap());
        Ok(())
    }

    pub async fn new_sds011(
        &mut self,
        uart: Uart<'static, UART2, Async>,
    ) -> Result<(), SensorError> {
        self.sds011 = Some(Sds011::new(uart).await.unwrap());
        Ok(())
    }
}
