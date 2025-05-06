ESP-32 home sensor
==================

> ESP32 DevKit v1 home sensor using the UART and I2C protocols. It supports
> various sensors to measure temperature, humidity and pressure. Such as the
> BME281, SCD30, SDS011, etc.
> Measurements are sent over TLS and the MQTTv5 protocol to [Mosquitto MQTT
> broker][mosquitto] and consumed by [Telegraf][telegraf] to persist into
> [InfluxDB][influxdb].
> It also supports over-the-air (OTA) automatic firmware upgrade.

## Overview

```mermaid
flowchart LR
    OutdoorSds011(fa:fa-microchip Sensor<br><small><i>SDS011</i></small>)
    OutdoorBme280(fa:fa-microchip Sensor<br><small><i>BME280</i></small>)
    Outdoor(fa:fa-microchip ESP32<br /><small><i>outdoor front</i></small>)
    LivingRoomBme280(fa:fa-microchip Sensor<br><small><i>BME280</i></small>)
    LivingRoom(fa:fa-microchip ESP32<br /><small><i>living room</i></small>)
    BedRoomScd30(fa:fa-microchip Sensor<br><small><i>SCD30</i></small>)
    BedRoom(fa:fa-microchip ESP32<br /><small><i>bedroom</i></small>)
    MQTT(fa:fa-tower-broadcast Mosquitto MQTT)
    Telegraf(fa:fa-gear Telegraf)
    InfluxDB(fa:fa-database InfluxDB)
    InfluxDashboards(fa:fa-chart-line Influx<br>Dashboards)
    HomeAssistant(fa:fa-house HomeAssistant)
    OtaServer(fa:fa-upload<br>OTA Server<br>Firmware update)

    subgraph Areas
    direction TB
    subgraph Outdoor Area
    direction LR
    OutdoorBme280--Read-->Outdoor
    OutdoorSds011--Read-->Outdoor
    end

    subgraph Living Room Area
    LivingRoomBme280--Read-->LivingRoom
    end

    subgraph Bed Room Area
    BedRoomScd30--Read-->BedRoom
    end
    end

    subgraph Influx Stack
    Telegraf-->InfluxDB
    InfluxDB-->InfluxDashboards
    end

    Outdoor-.Check version.->OtaServer
    Outdoor--Publish-->MQTT
    LivingRoom--Publish-->MQTT
    LivingRoom-.Check version.->OtaServer
    BedRoom--Publish-->MQTT
    BedRoom-.Check version.->OtaServer
    MQTT--Subscribe-->Telegraf
    MQTT-.Subscribe.->HomeAssistant
```

## Getting started

### ESP32

#### Requirements

- [espflash][espflash]
- [espmonitor][espmonitor]
- [espup][espup]

#### Pin-out

| Sensor | Pin Sensor  | ESP32 DevKit v1 Pin |
|--------|-------------|---------------------|
| BME280 | SDA         | GPIO 21 / D21       |
| BME280 | SCL         | GPIO 22 / D22       |
| BME280 | GND         | GND                 |
| BME280 | 3.3v        | 3v3                 |
| SCD30  | SDA         | GPIO 21 / D21       |
| SCD30  | SCL         | GPIO 22 / D22       |
| SCD30  | GND         | GND                 |
| SCD30  | 3.3v        | 3v3                 |
| SDS011 | RX          | GPIO 17 / U2:TXD    |
| SDS011 | TX          | GPIO 16 / U2:RXD    |
| SDS011 | GND         | GND                 |
| SDS011 | 5v          | 3v3                 |

Note: I2C devices share the same I2C bus (BME280, SCD30).

#### Configuration

Before flashing the device, you will need to configure parameters in the
`./cfg.toml` file, for example:

```toml
wifi_ssid = "my-wifi"
wifi_psk = "wifi-password"
hostname = "esp32-outdoor"
mqtt_hostname = "homie.local"
mqtt_port = 1883
mqtt_username = "esp32-outdoor"
mqtt_password = "someranddompassword"
mqtt_topic = "sensors"
location = "outdoor"

# optional
measurement_interval_seconds = 300

ota_hostname = "my-ota.example.com"
ota_port = 443

tls_ca = """
-----BEGIN CERTIFICATE-----
// your CA certificate here
-----END CERTIFICATE-----
"""
tls_key = """
-----BEGIN RSA PRIVATE KEY-----
// your private key here
-----END RSA PRIVATE KEY-----
"""
tls_cert = """
-----BEGIN CERTIFICATE-----
// your certificate here
-----END CERTIFICATE-----
```

#### Development

Connect the device via USB, then run the following command to run flash and
retrieve logs from the device using espmonitor:

```bash
espup install

. $HOME/export-esp.sh

cargo run --release

# or run specific features/sensors
cargo run --release --features json,bme280 --no-default-features
```

#### Flashing

Connect the device via USB, then flash it with the following command:

```bash
. $HOME/export-esp.sh
cargo espflash flash --release
```

#### Available features

The following Cargo features allow you to enable/disable sensors and select the
MQTT message format to use:

| Feature | Description                       | Default |
|---------|-----------------------------------|---------|
| bme280  | Enable BME280 sensor              | yes     |
| sds011  | Enable SDS011 sensor              | no      |
| influx  | Set MQTT payload format to Influx | yes     |
| json    | Set MQTT payload format to Json   | no      |
| tls     | Use TLS to connect to the MQTT    | yes     |
| mtls    | Use MTLS to connect to the MQTT   | yes     |
| ota     | Use over the air firmware upgrade | ota     |

For example, to only enable BME280 with JSON format:

```bash
. $HOME/export-esp.sh
# enable BME280, SDS011 with MQTT message in JSON format (default)
cargo espflash flash --release

# enable BME280 with MQTT message in INFLUX format
cargo espflash flash --release --features influx,bme280 --no-default-features
```

#### TLS

To enable `TLS` (mqtts), update the `cfg.toml` config to include the CA certificate:

```toml
tls_ca = """
-----BEGIN CERTIFICATE-----
// your CA certificate here
-----END CERTIFICATE-----
"""
```

Then make sure to enable the `tls` feature when you flash:
```bash
cargo run --release --features influx,bme280,tls --no-default-features
```

#### mTLS

To enable `mTLS`, update the `cfg.toml` config to include the CA, client
certificate and private key:

```toml
tls_ca = """
-----BEGIN CERTIFICATE-----
// your CA certificate here
-----END CERTIFICATE-----
"""
tls_key = """
-----BEGIN RSA PRIVATE KEY-----
// your private key here
-----END RSA PRIVATE KEY-----
"""
tls_cert = """
-----BEGIN CERTIFICATE-----
// your certificate here
-----END CERTIFICATE-----
```

Then make sure to enable both `tls` and `mtls` features when you flash:
```bash
cargo run --release --features influx,bme280,tls,mtls --no-default-features
```

### Setup infrastructure using Docker

Docker compose is used to setup the infrastructure. It is composed of 4 services:

- InfluxDB - persistent storage and basic dash-boarding
- Mosquitto - MQTT broker
- Telegraf - consume MQTT messages and store them in the database
- A bash job to setup InfluxDB user/token

First, review/adjust environment variables in `./infra/influxdb.env`. Then, for
each device (one per line) define a username/password in `./infra/mqtt.auth`
file. For example:

```yaml
esp32-outdoor:a19sn#sA94k!a5o10
esp32-indoor:a93KifAoBf7#01-jl
```

After what, run `docker compose up -d` to deploy the infrastructure.

Note, you might have to restart all containers to get the environment variables
re-populated into the container after being generated during initial boot.

You can visualise metrics by importing `./infra/influx-dashboard.json` as
dashboard into InfluxDB.

![InfluxDB dashboard](./dashboard.png)

### Setup Home Assistant or other providers

It's possible to change the payload format of the MQTT message to JSON instead
of Influx by using the `--features json`.

Refer to the [Home Assistant documentation](./docs/home-assistant.md) for
details on how to set this up.

![Home Assistant dashboard](./home-assistant.png)

### Over the Air firmware upgrades (OTA)

OTA (Over-the-Air) refers to remotely updating the firmware or software on a
device using a wireless connection (like Wi-Fi, cellular, or LoRa). This lets
you patch bugs, add features, or fix security issues without physically
accessing the device.

This repository provide an example implementation of OTA using the
[esp-hal-ota][esp-hal-ota] library. It requires an OTA server to be deployed
somewhere where each device can download the last firmware from.

Configuration is straight forward, include the hostname and port of the OTA
server in your configuration:

```toml
ota_hostname = "my-ota.example.com"
ota_port = 443
```

At runtime, the device contacts the OTA server, which responds with the latest
version, firmware size, and CRC. The device compares the version to its current
firmware version and downloads and applies the update if there is a version
mismatch.

```bash
. $HOME/export-esp.sh

cargo build --release --features influx,bme280,tls,mtls,ota --no-default-features
espflash save-image --chip esp32 ./target/xtensa-esp32-none-elf/release/esp32_home_sensor ../ota/firmware.bin
```

<!-- page links -->
[esp-hal-ota]: https://github.com/filipton/esp-hal-ota/
[espflash]: https://esp-rs.github.io/book/tooling/espflash.html
[espmonitor]: https://esp-rs.github.io/book/tooling/espmonitor.html
[espup]: https://esp-rs.github.io/book/installation/installation.html#espup
[mosquitto]: https://mosquitto.org
[telegraf]: https://www.influxdata.com/time-series-platform/telegraf/
[influxdb]: https://www.influxdata.com
