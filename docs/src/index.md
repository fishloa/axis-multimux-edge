# Multimux Edge

An **Axis ACAP application** that captures the camera's hardware-encoded
H.264/H.265 stream via **VDO** and serves **LL-HLS on the camera** — no
restream hop. It reuses the [`multimux`](https://crates.io/crates/multimux)
library (the same LL-HLS segmenter + RAM rolling window + axum origin +
blocking-reload that the standalone `multimux-cli` server uses), fed by a
`VdoIngestSession` instead of RTSP.

Target SoCs: **ARTPEC-6 / 7 / 8 / 9**; **H.264** on all of them, **H.265** on
ARTPEC-7/8/9 only. See [Supported Devices](operator/supported-devices.md)
for the full firmware/architecture matrix.

## Two guides, one project

- **[Operator Guide](operator/installing.md)** — installing the `.eap` on a
  camera, supported device matrix, configuration reference, troubleshooting
  playback, and the reverse-proxy security model.
- **[Contributor Guide](contributor/architecture.md)** — how the capture
  pipeline works, the module map, the host/device build split, the release
  process, and how the project is tested.

## API reference

Generated rustdoc for the host-testable library (`convert`, `admin`, `error`
modules) is published alongside this guide at [`/api/`](api/index.html).

## Source

[github.com/fishloa/axis-multimux-edge](https://github.com/fishloa/axis-multimux-edge) —
MIT OR Apache-2.0.
