# Over the Air firmware upgrades (OTA)

Over-the-Air (OTA) updates refer to the process of remotely updating the
firmware or software on a device using a wireless connection, such as Wi-Fi,
cellular, or LoRa. This method allows you to patch bugs, add new features, or
fix security vulnerabilities without needing physical access to the device.

This repository provides an example implementation of OTA updates using the
[esp-hal-ota][esp-hal-ota] library. To utilize this, an OTA server must be
deployed where each device can download the latest firmware.

Configuring the OTA server is straightforward. Include the hostname and port of
the OTA server in your configuration file as follows:

```toml
ota_hostname = "my-ota.example.com"
ota_port = 443
```

During runtime, the device will contact the OTA server, which will respond with
the latest firmware version, size, and CRC. The device will then compare this
version with its current firmware version. If there is a version mismatch, the
device will download and apply the update.

## Releasing new firmware through OTA

For this example, we compile the code into a binary and push it using
[Oras][oras] to an OCI registry (in this case, [Harbor][harbor]).

Since secrets and configurations are embedded into the binary, you will need to
create a separate repository for each ESP32 board.

Follow these steps to release new firmware:

1. **Update the Package Version:**
   Bump the package version in the `Cargo.toml` file. For example:
   ```toml
   [package]
   name = "esp32_home_sensor"
   version = "0.1.2"
   ```
   *Note: The package version is used by the chip to determine if its firmware
   needs to be updated when querying the OTA server for the latest changes.*
2. **Modify configuration**
   Update the `cfg.toml` file with the correct configuration for the ESP32.
3. **Build, Save, and Push the Image:**
   Execute the following commands to compile, save the image, and push it to
   the OCI registry:
   ```bash
   . $HOME/export-esp.sh
   
   # this is used to push to the registry, it should match the one defined in
   # the cfg.toml
   export DEVICE_ID=esp32-outdoor
   
   # compile
   cargo build --release --features influx,bme280,tls,mtls,ota --no-default-features
   
   # save as binary image
   espflash save-image --chip esp32 ./target/xtensa-esp32-none-elf/release/esp32_home_sensor ./firmware.bin
   
   # push to OCI registry
   oras push "my-registry.example.com:443/my-repository/${DEVICE_ID}:0.1.2" \
       firmware.bin:application/vnd.espressif.esp32.firmware.v1+binary
   ```

**Notes**: when flashing the initial program with OTA support, make sure to include
the following partitions parameters:

```bash
cargo espflash flash \
    --release \
    --features influx,tls,mtls,ota,bme280 \
    --no-default-features \
    -T ./partitions.csv \
    --erase-parts otadata
```

## OTA server

Refer to the [etiennetremel/otaflux][otaflux] repository for the implementation
of the OTA server and refer to the [etiennetremel/homie-lab][homie-lab]
repository for the infrastructure setup.

[OtaFlux][otaflux] is an OTA (Over-the-Air) firmware update server that
fetches, caches, and serves firmware binaries from an OCI-compatible container
registry.

<!-- page links -->
[esp-hal-ota]: https://github.com/filipton/esp-hal-ota/
[harbor]: https://goharbor.io
[homie-lab]: https://github.com/etiennetremel/homie-lab
[oras]: https://oras.land
[otaflux]: https://github.com/etiennetremel/otaflux
