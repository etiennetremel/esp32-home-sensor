# TLS/mTLS Configuration

This document describes the TLS and mTLS (mutual TLS) configuration for the
ESP32 home sensor, along with common issues and their solutions.

## Overview

The ESP32 connects to an MQTT broker (NanoMQ) using TLS 1.3 with mutual
authentication. This requires:

- **CA Certificate**: To verify the server's certificate
- **Client Certificate**: ECDSA P-256, presented to the server for authentication
- **Client Private Key**: SEC1 (PEM) format, ECDSA P-256

Note: NanoMQ should be compiled with the OpenSSL engine on. For reference, see
https://github.com/etiennetremel/nanomq-openssl.

## Certificate Requirements

### Client Certificate and Key

The client certificate must use ECDSA with the P-256 curve. The private key
must be in SEC1 format (PEM header: `-----BEGIN EC PRIVATE KEY-----`).

Generate using cert-manager (Kubernetes) with:

```yaml
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: esp32
spec:
  secretName: esp32-tls
  issuerRef:
    name: selfsigned-ca
    kind: ClusterIssuer
  commonName: esp32
  privateKey:
    algorithm: ECDSA
    encoding: PKCS1  # SEC1 format for EC keys
    size: 256        # P-256 curve
  usages:
    - digital signature
    - client auth
```

### Verifying Key Format

The private key DER should be 121 bytes for P-256 and start with `30 77`:

```bash
# Check key type and format
openssl ec -in certs/tls.key -noout -text

# Check DER structure (should show SEQUENCE length 119)
openssl asn1parse -in certs/tls.key -inform PEM

# Verify DER output size (should be 121 bytes)
openssl ec -in certs/tls.key -outform DER | wc -c
```

## embedded-tls Configuration

This project uses [embedded-tls](https://github.com/drogue-iot/embedded-tls)
for TLS 1.3 support on the ESP32. Key configuration points:

### Cargo.toml

```toml
embedded-tls = {
  git = "https://github.com/drogue-iot/embedded-tls",
  branch = "main",
  default-features = false,
  features = ["log", "alloc"]
}
```

The `alloc` feature is **required** to enable RSA signature algorithms in the
ClientHello. Without this, servers using RSA certificates will reject the
connection.

### Cipher Suite Selection

**Use `Aes128GcmSha256`, not `Aes256GcmSha384`.**

There is a bug in embedded-tls where the `client_cert_verify` function uses a
fixed-size buffer (`heapless::Vec<u8, 130>`) that is too small for SHA384:

| Cipher Suite | Hash Size | Buffer Needed | Result |
|--------------|-----------|---------------|--------|
| Aes128GcmSha256 | 32 bytes | 64 + 34 + 32 = 130 | Works |
| Aes256GcmSha384 | 48 bytes | 64 + 34 + 48 = 146 | `EncodeError` |


## Buffer Sizes

The TLS buffers must be at least 16640 bytes for TLS 1.3 handshakes:

```rust
pub const TLS_BUFFER_MAX: usize = 16640;  // 16384 + 256 overhead
```

## References

- [embedded-tls repository](https://github.com/drogue-iot/embedded-tls)
- [TLS 1.3 RFC 8446](https://datatracker.ietf.org/doc/html/rfc8446)
- [SEC1 EC Key Format](https://www.secg.org/sec1-v2.pdf)
