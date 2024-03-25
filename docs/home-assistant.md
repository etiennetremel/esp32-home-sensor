# Home assistant

## Getting started

The following steps provide a way to connect the sensor to [Home
Assistance][ha].

1. install [Home Assistant][ha-install]
2. install the Mosquitto MQTT broker add-on: Settings > Add-ons > Add-on
   store > Mosquitto broker
3. install the File editor add-on: Settings > Add-ons > Add-on store > File
   editor
4. install the MQTT integration: Settings > Devices & services > Add
   integration > MQTT
5. edit the `/homeassistant/configuration.yaml` file using the File editor
   add-on with the following sensors:

   ```yaml
   mqtt:
     sensor:
       - name: Leaving room temperature
         state_topic: "home/sensor/leaving-room/environment"
         device_class: "temperature"
         unit_of_measurement: "°C"
         value_template: "{{value_json.temperature | round(1)}}"
       - name: Leaving room pressure
         state_topic: "home/sensor/leaving-room/environment"
         device_class: "pressure"
         unit_of_measurement: "hpa"
         value_template: "{{value_json.pressure | round(1)}}"
       - name: Leaving room humidity
         state_topic: "home/sensor/leaving-room/environment"
         device_class: "humidity"
         unit_of_measurement: "%"
         value_template: "{{value_json.humidity | round(1)}}"
   ```
6. reload the Yaml configuration: Developer tools > All yaml configuration
7. create a dedicated user for the sensor: Settings > People > Add person.
   Make sure the user can login with credentials, then use the credentials 
   when you will be flashing the chip.

## Flashing the chip

Now that Home Assistant is correctly setup, update the `cfg.toml` in the root
of this repository with the corresponding MQTT values, e.g.:

```toml
[esp32_home_sensor]
wifi_ssid = "wifi-ssid"
wifi_psk = "wifi-password"
hostname = "esp32-leaving-room"
mqtt_hostname = "homeassistant"
mqtt_port = 1883
mqtt_username = "esp32"
mqtt_password = "esp32"
mqtt_topic = "home/sensor/leaving-room/environment"
location = "leaving-room"
```

Flash the chip:

```bash
. $HOME/export-esp.sh
cargo espflash flash --release --features json
```

<!-- page links -->
[ha]: https://www.home-assistant.io
[ha-install]: https://www.home-assistant.io/installation/
