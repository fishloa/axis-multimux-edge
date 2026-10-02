# Multimux Edge

An **Axis ACAP application** that captures the camera's hardware-encoded
H.264/H.265 stream via **VDO** and serves **LL-HLS on the camera** — no
restream hop.

Target SoCs: **ARTPEC-6 / 7 / 8 / 9**; **H.264** on all, **H.265** on
ARTPEC-7/8/9 only.

## Documentation

Full docs: **https://fishloa.github.io/axis-multimux-edge/**

- [Installing](https://fishloa.github.io/axis-multimux-edge/operator/installing.html)
- [Supported Devices](https://fishloa.github.io/axis-multimux-edge/operator/supported-devices.html)
- [Configuration](https://fishloa.github.io/axis-multimux-edge/operator/configuration.html)
- [Troubleshooting](https://fishloa.github.io/axis-multimux-edge/operator/troubleshooting.html)
- [Security](https://fishloa.github.io/axis-multimux-edge/operator/security.html)
- [Architecture](https://fishloa.github.io/axis-multimux-edge/contributor/architecture.html)
- [Module Map](https://fishloa.github.io/axis-multimux-edge/contributor/module-map.html)
- [Building](https://fishloa.github.io/axis-multimux-edge/contributor/building.html)
- [Releasing](https://fishloa.github.io/axis-multimux-edge/contributor/releasing.html)
- [Testing](https://fishloa.github.io/axis-multimux-edge/contributor/testing.html)
- [API reference (rustdoc)](https://fishloa.github.io/axis-multimux-edge/api/)

## Quickstart

```bash
# Host build/test (no ACAP SDK needed)
cargo test --locked

# Install a released .eap on a camera
export AXIS_DEVICE_IP=<camera-ip> AXIS_DEVICE_USER=root AXIS_DEVICE_PASS=<pw>
cargo-acap-sdk install --host "$AXIS_DEVICE_IP" --user root --pass "$AXIS_DEVICE_PASS"
cargo-acap-sdk start   --host "$AXIS_DEVICE_IP" --user root --pass "$AXIS_DEVICE_PASS"
```

Grab a `.eap` from the [Releases page](https://github.com/fishloa/axis-multimux-edge/releases)
— see the [Supported Devices](https://fishloa.github.io/axis-multimux-edge/operator/supported-devices.html)
matrix for which file matches your camera.

## License

MIT OR Apache-2.0.
