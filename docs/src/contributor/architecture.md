# Architecture

```text
VDO (libvdo) --coded AU + ts--> VdoIngestSession --> transmux Sample/TrackSpec
    --> media_plane::ingress::IngestDriver --> multimux::supervise_driver
    --> multimux::source::advance_route --> RouteHandle (RAM window)
    --> axum (multimux::origin nested /hls  +  admin routes)  on 127.0.0.1:2999
    <-- camera web server reverse-proxies (manifest.json) with VAPIX access levels
```

- **`src/convert.rs`** — pure (host-testable): VDO Annex-B access unit →
  `transmux::Sample`/`TrackSpec`, in-band SPS/PPS/VPS extraction (avcC / hvcC).
- **`src/vdo_source.rs`** (`device`-gated) — drives `vdo::StreamBuilder` /
  `RunningStream::next_buffer`, implements `media_plane::ingress::IngestSession`
  directly (no `Dialer`: VDO is a local capture, not a dial-out). Driven by a
  plain `IngestDriver` the caller (the bin) builds itself, not
  `multimux::pipeline::run_pipeline` (removed from multimux upstream).
- **`src/admin.rs`** — `Config` + `ConfigStore` (`axparameter` on device) +
  the `/admin/config` + `/admin/status` routes. See
  [Module Map](module-map.md) for the full breakdown.
- **`src/bin/multimux-edge.rs`** (`device`-gated) — the ACAP entrypoint: loads
  config, runs the capture pipeline on a dedicated OS thread (VDO
  `next_buffer` blocks) via `multimux::supervise_driver`/
  `multimux::source::advance_route`, serves the origin + admin on
  `127.0.0.1:2999`.
- **`manifest.json`** — ACAP `reverseProxy` (`hls` = viewer, `admin` = admin)
  + `settingPage` (`index.html`, served from the app's `html/` directory). See
  [Security](../operator/security.md) for what the access levels mean.

See [Module Map](module-map.md) for file-by-file detail and
[Building](building.md) for the host/device split this architecture forces
on the build.
