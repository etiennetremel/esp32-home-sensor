#!/usr/bin/env bash
# This script is used to generate the telegraf.env file and does the following:
# - create a Telegraf user with a random password in InfluxDB
# - generate one or pull existing telegraf MQTT password from the mqtt.auth
#   file and store it as TELEGRAF_MQTT_PASSWORD
# - generate an INFLUX_TOKEN for Telegraf to use to write to InfluxDB

set -e

INFLUXDB_TELEGRAF_USERNAME=telegraf
INFLUXDB_TELEGRAF_PASSWORD=$(openssl rand -base64 24)
TELEGRAF_USERNAME=telegraf
TELEGRAF_PASSWORD=$(openssl rand -base64 24)

if [ ! -f telegraf.env ]
then
  echo "File telegraf.env is missing"
  touch ./telegraf.env
fi

if ! grep -q TELEGRAF_MQTT_USERNAME ./telegraf.env
then
  echo "Adding TELEGRAF_MQTT_USERNAME to telegraf.env"
  echo "TELEGRAF_MQTT_USERNAME=$TELEGRAF_USERNAME" >> ./telegraf.env
fi

if ! grep -q TELEGRAF_MQTT_PASSWORD ./telegraf.env
then
  echo "Adding TELEGRAF_MQTT_PASSWORD to telegraf.env"
  if ! grep -q "^$TELEGRAF_USERNAME:" ./mqtt.auth
  then
    echo "Telegraf user not found in mqtt.auth, generating it"
    echo "$TELEGRAF_USERNAME:$TELEGRAF_PASSWORD" >> ./mqtt.auth
  else
    TELEGRAF_PASSWORD=$(awk -F':' -v "username=$TELEGRAF_USERNAME" '$1 == username {print $2}' ./mqtt.auth)
  fi
  echo "TELEGRAF_MQTT_PASSWORD=$TELEGRAF_PASSWORD" >> ./telegraf.env
fi

if ! grep -q INFLUX_TOKEN ./telegraf.env
then
  user=$(influx user list \
    --host http://influxdb:8086 \
      --token "$DOCKER_INFLUXDB_INIT_ADMIN_TOKEN" \
    | grep "$TELEGRAF_USERNAME" || true)
  if [ -z "$user" ]
  then
    echo "User $TELEGRAF doesn't exist, creating..."
    influx user create \
      --host http://influxdb:8086 \
      --org "$DOCKER_INFLUXDB_INIT_ORG" \
      --name "$TELEGRAF_USERNAME" \
      --password "$TELEGRAF_PASSWORD" \
      --token "$DOCKER_INFLUXDB_INIT_ADMIN_TOKEN"
  fi

  bucket_id=$(influx bucket list \
    --host http://influxdb:8086 \
    --org "$DOCKER_INFLUXDB_INIT_ORG" \
    --token "$DOCKER_INFLUXDB_INIT_ADMIN_TOKEN" \
    | awk -v "bucket=$DOCKER_INFLUXDB_INIT_BUCKET" '$2 == bucket {print $1}')

  if [ -z "$bucket_id" ]
  then
    echo "Bucket $DOCKER_INFLUXDB_INIT_BUCKET not found, exiting."
    exit 1
  fi

  echo "Found bucket id ${bucket_id}"

  echo "InfluxDB token doesn't exist for Telegraf, generating one"
  influx_token=$(influx auth create \
    --host http://influxdb:8086 \
    --org "$DOCKER_INFLUXDB_INIT_ORG" \
    --user "$TELEGRAF_USERNAME" \
    --token "$DOCKER_INFLUXDB_INIT_ADMIN_TOKEN" \
    --write-bucket "$bucket_id" \
    | awk 'FNR==2 {print $2}')

  echo "INFLUX_TOKEN=$influx_token" >> telegraf.env
fi
