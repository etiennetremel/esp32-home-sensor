use bme280_rs::{Bme280, Oversampling, SensorMode};
use defmt::{write, Format, Formatter};
use esp_hal::{delay::Delay, i2c::I2C, peripherals::I2C0, Async};

pub struct Sensor {
    bme280: Bme280<I2C<'static, I2C0, Async>, Delay>,
}

#[derive(Debug)]
pub enum Error {
    Bme280Measurement,
}

impl Format for Error {
    fn format(&self, fmt: Formatter) {
        match self {
            Error::Bme280Measurement => write!(fmt, "Bme280 measurement error"),
        }
    }
}

#[derive(Debug)]
pub struct Measurement {
    pub humidity: f32,
    pub pressure: f32,
    pub temperature: f32,
}

impl Sensor {
    pub fn new(i2c: I2C<'static, I2C0, Async>, delay: Delay) -> Sensor {
        let mut bme280 = Bme280::new(i2c, delay);
        bme280.init().unwrap();
        bme280
            .set_sampling_configuration(
                bme280_rs::Configuration::default()
                    .with_temperature_oversampling(Oversampling::Oversample1)
                    .with_pressure_oversampling(Oversampling::Oversample1)
                    .with_humidity_oversampling(Oversampling::Oversample1)
                    .with_sensor_mode(SensorMode::Normal),
            )
            .unwrap();
        Sensor { bme280 }
    }

    pub fn measure(&mut self) -> Result<Measurement, Error> {
        if let Ok(sample) = self.bme280.read_sample() {
            if let (Some(temperature), Some(pressure), Some(humidity)) =
                (sample.temperature, sample.pressure, sample.humidity)
            {
                return Ok(Measurement {
                    temperature,
                    humidity,
                    pressure,
                });
            }
        }
        Err(Error::Bme280Measurement)
    }
}
