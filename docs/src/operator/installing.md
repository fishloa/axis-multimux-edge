# Installing

Grab the `.eap` for your camera's firmware and architecture from the
[GitHub Releases page](https://github.com/fishloa/axis-multimux-edge/releases) —
see [Supported Devices](supported-devices.md) for which file matches your
camera. Each release also ships a `SHA256SUMS` file to verify the download.

## Option 1: web UI

On the camera's web interface: **Apps → Add app**, upload the `.eap` file,
then start it from the Apps list.

## Option 2: `cargo-acap-sdk` CLI

Install the installer tool once:

```bash
cargo install --locked --git https://github.com/AxisCommunications/acap-rs \
  --rev 8e58acb8f0617253ad21fb71ac319fea19454a38 cargo-acap-sdk
```

Then, with the `.eap` file downloaded locally:

```bash
export AXIS_DEVICE_IP=<camera-ip> AXIS_DEVICE_USER=root AXIS_DEVICE_PASS=<pw>
cargo-acap-sdk install --host "$AXIS_DEVICE_IP" --user root --pass "$AXIS_DEVICE_PASS"
cargo-acap-sdk start   --host "$AXIS_DEVICE_IP" --user root --pass "$AXIS_DEVICE_PASS"
```

`install` uploads the `.eap` over VAPIX; `start` launches it.

## Verify the install

1. The app shows **running** in the camera's Apps list.
2. `curl -u <user>:<pw> https://<cam>/local/multimuxedge/hls/cam/media.m3u8`
   returns an LL-HLS media playlist (look for `#EXT-X-PART`,
   `#EXT-X-PART-INF`, `#EXT-X-SERVER-CONTROL`).
3. The admin settings page loads at `https://<cam>/local/multimuxedge/`.

If any of these fail, see [Troubleshooting](troubleshooting.md).
