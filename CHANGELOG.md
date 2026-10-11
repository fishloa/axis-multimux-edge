# Changelog

All notable changes to Multimux Edge (formerly `axis-origin`) are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0] - 2026-10-11

### Added

- **Stream profiles.** Map URL names to the camera's own stream profiles
  (`hls/medium/media.m3u8` → `ACC_Medium`) on the admin page or via
  `POST /admin/config`. Streams start on the first request, stop when idle,
  and share one encode when they resolve to the same settings. A
  configurable encode cap (default 2) answers 503 instead of slowing every
  stream down.
- `GET /admin/profiles` and an OpenAPI description at
  `GET /admin/openapi.json`; API reference on the docs site.
- Redesigned admin page (Multimux Edge look, live status, light/dark).
- **Profile encoder keys.** `compression`, `rotation`, `mirror`,
  `videobitratemode` (vbr/mbr/abr), `videomaxbitrate`, the ABR target and
  retention, and the Zipstream GOP/fps modes and maximum GOP length are now
  applied, mapped to VDO the way the camera's RTSP server maps them, instead
  of being ignored.
- **Encode sharing.** A stream joins an encode the camera is already running
  for the same camera, codec, resolution and frame rate (e.g. for a VMS over
  RTSP) instead of starting a second one. Joined streams run at full rate,
  don't slow the camera's other clients, don't count against
  `max_encodes`, and report `shared_encode: true` in `/admin/status`.

### Changed

- **Breaking:** config v2. Capture fields move under `main`; older stored
  configs are migrated on load. `port` is removed (the app always listens
  on 2999, the manifest's reverse-proxy target).
- **Breaking:** the stream URL `hls/cam/media.m3u8` is now `hls/main/…`
  (or the bare `hls/media.m3u8`, which redirects to the default stream).
- **Breaking:** `GET /admin/status` now reports `encodes` and a `streams`
  list; the single-pipeline fields moved into each stream entry.
- Config changes apply immediately; no restart.
- Profiles without `camera=` now stream from camera 1 (the VAPIX default,
  as the camera's RTSP server does), not the main preset's channel.
- The app no longer forces a 1-second key-frame interval. It sets one only
  when the profile sets `videokeyframeinterval`, waits for the first key
  frame however long it takes (warning every 10 s), and asks the camera
  for one at start.

### Fixed

- 4K streams no longer capped at ~18 fps. The cap came from running a
  second 4K encode next to the one the camera already ran for its RTSP
  clients; the stream now joins that encode (25 fps on the P1448-LE).
- A capture now stops within about a second even if its camera channel
  stops producing frames; before, it waited for the next frame and kept
  its encode open.
- A panic now restarts the app (the ACAP respawns it) instead of leaving a
  capture that looked running but delivered nothing.

## [0.2.0] - 2026-10-05

### Breaking

- **Renamed to Multimux Edge.** The ACAP `appName` is now `multimuxedge`
  (was `axisorigin`), with display name "Multimux Edge"; the crate and binary
  are `multimux-edge`; the repository moved to
  `github.com/fishloa/axis-multimux-edge`. Because `appName` changed:
  - The camera treats this as a new app. Uninstall `axisorigin` before
    installing `multimuxedge`; it does not upgrade in place.
  - Saved settings do not carry over. The axparameter group is keyed by
    `appName`, so the new app starts on defaults.
  - URLs move from `/local/axisorigin/...` to `/local/multimuxedge/...`
    (stream: `/local/multimuxedge/hls/cam/media.m3u8`).

### Changed

- **Dependencies bumped:** multimux 0.11, transmux 0.25, media-plane 0.5,
  broadcast-common 9.4 and axum 0.8 (required by multimux 0.11), plus
  every other dependency to its latest compatible version. acap-rs
  re-pinned to the fork rebased on upstream `5eed2e2`; firmware-12 builds
  use ACAP Native SDK 12.11.0.

### Fixed

- **Fresh installs no longer start with a broken config.** libaxparameter
  truncated the first-run default (written via `add`) to `{`, so a new
  install reported `config load: stored config is not valid JSON` until the
  first save. The default is now written with `set`, and a stored `{` or
  empty value is treated as "nothing stored yet".
- **The config store has never worked on any camera** (issue #955, blocks
  #954's H.265 hardware verification). `AxParameterStore::store` called
  `axparameter::Parameter::set("Config", …)` on a parameter that was never
  `add`ed — confirmed on an ARTPEC-8 camera:
  `param.cgi?action=list&group=axis-origin` returned `Error -1 getting
  param in group`. `AxParameterStore::new` now calls `Parameter::add` (with
  `Config::default()` as the initial value) if the parameter doesn't exist
  yet, matching the vendored `axparameter_example` app's own
  add-then-ignore-`ParamAdded` idiom so a second start (every restart, since
  `manifest.json` sets `runMode: "respawn"`) doesn't fail just because the
  parameter now exists.
- **A broken config backend was indistinguishable from an unconfigured
  one.** `ConfigStore::load` used to discard the backend's error and return
  `Config::default()` either way, which is why the parameter-store bug above
  went unnoticed for a month: the app *looked* like it was running fine.
  `load` now returns a `LoadOutcome` (`Stored`/`Unset`/`Broken(reason)`)
  instead of a bare `Config`; a `Broken` outcome is surfaced through
  `/admin/status`'s `last_error` (via a new `StatusHandle::set_config_error`
  slot, kept separate from the capture pipeline's own `last_error` so a
  pipeline retry can't silently erase it).
- **`/admin/status` reported `current_segment`/`current_part`/`frames` as
  permanent zeros while media was flowing** — measured on the same camera:
  the LL-HLS playlist's `#EXT-X-MEDIA-SEQUENCE` climbed from 2 to 5 over 12
  seconds while `/admin/status` stood still at `0`/`0`/`0`. `StatusHandle`
  was never updated by the capture pipeline. The VDO capture loop
  (`run_vdo_capture` in the `axis-origin` binary) now increments the frame
  counter once per `feed()` call and updates the segment/part position from
  the program's `Trunk` (`last_closed_segment` + `parts_in_segment`) on every
  iteration.

### Changed

- `ConfigStore::load` returns `admin::LoadOutcome` instead of `Config`
  (breaking change to this crate's internal, `publish = false` trait — no
  crates.io consumer is affected).
