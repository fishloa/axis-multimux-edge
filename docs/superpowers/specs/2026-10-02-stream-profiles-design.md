# Stream profiles — design

Date: 2026-10-02 · Status: draft for review · Builds on: v0.2.0
(Multimux Edge rename, multimux 0.11, axum 0.8)

## Goal

Let an operator expose the camera's own **stream profiles** (the presets under
*Settings → Stream → Stream profiles*, e.g. `ACC_Medium`) as clean LL-HLS URLs,
configured from the app's admin page and applied **live**, without restarting
the app.

```
/local/multimuxedge/hls/medium/media.m3u8   →  camera profile "ACC_Medium"
/local/multimuxedge/hls/media.m3u8          →  the default stream
```

Audience: an open-source showcase of the rust-broadcast / multimux stack, so
the admin UI and API must look finished and be self-documenting.

## Decisions (agreed)

| Topic | Decision |
|---|---|
| What a "profile" is | An existing **Axis stream profile** on the camera, read via VAPIX. The app does not define its own presets (except the built-in fallback below). |
| Naming | An operator-chosen **name** maps to one camera profile. The name *is* the URL segment. No separate slug concept, no ad-hoc `profile/<raw name>` route. |
| Default | One mapping may be marked default; it serves the bare `/hls/media.m3u8`. If none is marked, the bare URL serves the built-in **main** preset (today's channel/resolution/fps/codec settings). |
| When streams run | **On demand.** First request for a name starts its capture; it stops after an idle timeout. |
| Config changes | **Live.** Saving the config applies immediately — no restart. |
| Capacity | Bounded by a configurable **encode cap** (see "Encoder capacity" below). |
| Admin UI look | **Multimux Edge's own brand** (option A) — camera-UI-familiar layout, not Axis's styling. |
| API docs | **OpenAPI via `utoipa`**, served by the app and rendered on the docs site. |

Out of scope: profile selection by query string, serving unmapped raw profile
names, audio, per-stream LL-HLS tuning, defining profiles from within the app,
bundling Swagger UI on the camera.

## Spike findings (2026-10-02, P1448-LE, ARTPEC-6, AXIS OS 11.11)

Throwaway probes; no spike code is kept.

1. **Reading profiles from inside the app is feasible.** The camera exposes
   `com.axis.HTTPConf1.VAPIXServiceAccounts1.GetCredentials(s) → s` on the
   system D-Bus (`http-confd`). Not yet called end to end (calling it as root
   would create a real account); first device build verifies it.
2. **Live routing works without changing multimux.** An outer axum handler
   for `/hls/{name}/*` looks the name up in a shared map and forwards the
   request (path rewritten to `/{name}/…`) to a per-stream router built with
   `multimux::origin::router` over a single-stream `AppState`. Add → served;
   remove → 404; `AppState::new` is safe to call repeatedly (shared
   Prometheus recorder). Empty stream answers 503. Quirk: multimux answers
   **500** for an unknown `*.m3u8` under a stream — to raise with the
   rust-broadcast session; not a blocker.
3. **Encoder capacity is a count of distinct concurrent encodes, not pixel
   rate.** Measured via the camera's native RTSP server, our app stopped:

   | Concurrent distinct encodes | Result |
   |---|---|
   | 1–2 (any mix, incl. 4K + 1080p, 4K + 720p) | full 25 fps |
   | 3+ (incl. 4 × 360p) | every stream drops to ~15–18 fps |

   So "one 4K plus one other" is fine; a third encode slows everything. Other
   consumers (e.g. AXIS Camera Station recording) share the same budget.

## Config model

The whole config stays one JSON value in the existing axparameter `Config`
parameter (same store, same `GET`/`POST /admin/config`). New shape:

```json
{
  "main": {
    "channel": 0, "width": 1920, "height": 1080, "framerate": 30,
    "codec": "h264"
  },
  "streams": [
    { "name": "medium", "profile": "ACC_Medium" },
    { "name": "4k",     "profile": "ACC_High" }
  ],
  "default_stream": "medium",
  "max_encodes": 2,
  "idle_timeout_secs": 30,
  "target_duration_secs": 4.0,
  "part_target_ms": 500,
  "window_segments": 8
}
```

- `main` is the built-in fallback preset — today's top-level capture fields,
  moved under one key.
- `streams[].name`: `^[a-z0-9][a-z0-9-]{0,31}$`, unique. Reserved: `main`
  (always means the built-in preset, so it can never be shadowed) and
  `media.m3u8`.
- `default_stream`: `null` or the name of an entry in `streams`.
- `max_encodes`: 1–8, default **2** (from the spike).
- `idle_timeout_secs`: 5–600, default 30.
- LL-HLS tuning stays global and applies to every stream.
- **No `port` setting.** The app always listens on `127.0.0.1:2999`, a code
  constant that must equal `manifest.json`'s `reverseProxy` target (a host test
  enforces this). A configurable port could only break the camera's proxy to
  the app, and with it the admin page needed to undo the change. Every
  setting applies live.

**Migration:** a stored config in the old flat shape (no `streams` key) is
read as `main` = its capture fields, `streams` = `[]`. A stored `port` is ignored. The first save writes
the new shape. Validation errors return 400 with a field-level message and
leave the stored config unchanged.

**First-run truncation bug fix:** shipped separately in v0.2.0 (`add` empty,
then `set` the default; a stored `{` or empty value reads as unset).

## URLs

Under `/local/multimuxedge/hls/`:

| Path | Serves |
|---|---|
| `<name>/media.m3u8` (+ segments) | mapping `<name>` |
| `main/media.m3u8` | built-in main preset |
| `media.m3u8` | `default_stream` if set, else main |

The bare URL redirects (302) to `<resolved name>/media.m3u8`, so relative
segment URIs in the playlist keep working and each stream has exactly one
canonical path. Unknown name → 404.

## Components

New and changed units, each testable on its own. `device`-gated means it
only builds in the ACAP SDK (as today).

| Unit | File | Responsibility | Host-testable |
|---|---|---|---|
| Config v2 | `src/admin.rs` (types split to `src/config.rs`) | Types, validation, old-shape migration, `utoipa::ToSchema` | yes |
| Profile string parser | `src/profile.rs` | Parse `resolution=1920x1080&fps=25&videocodec=h264&…` into capture settings; report unsupported keys | yes |
| Profile source | `src/vapix.rs` (device) | Get service-account credentials over D-Bus; call `streamprofile.cgi` `list`; cache briefly | trait + fake on host |
| Stream registry | `src/registry.rs` | Name → (resolved settings, capture state, per-stream router); rebuild on config change; idle timers; encode cap | yes (fake capture) |
| Capture worker | `src/bin/multimux-edge.rs` + `src/vdo_source.rs` (device) | Today's `run_vdo_capture`, parameterised per stream, started and stopped by the registry | no (device) |
| HLS front router | `src/routing.rs` | `/hls/{name}/*` forwarding (spike pattern), bare-URL redirect, "touch" for idle tracking, 503 on cap | yes |
| Admin API | `src/admin.rs` | Existing config/status endpoints + `GET /admin/profiles` + `GET /admin/openapi.json` | yes |
| Admin UI | `html/index.html` (+ bundled css/js) | Redesigned page, below | manual + device |

## Data flow

1. **Request** `GET /hls/medium/media.m3u8` → front router → registry
   `touch("medium")`.
2. Registry: if `medium` is idle, resolve its profile (cached profile list),
   check the encode cap, start a capture worker, create the stream's
   `RouteHandle` + router, mark it *starting*. If over the cap → 503 with
   `Retry-After` and reason `encoder busy (2/2 encodes in use)`.
3. The request is forwarded to the stream's router. Until the first segment
   exists, multimux returns 503 and the player retries — expected start-up
   latency of one segment.
4. Every request touches the stream; an idle sweeper stops a stream after
   `idle_timeout_secs` with no touches and frees its encode.
5. **Config save** (`POST /admin/config`) → validate → store → registry
   `apply(new_config)`: streams whose mapping changed or was removed are
   stopped; new mappings are registered idle. No restart.

**Shared encodes:** two names mapped to the same profile, or the same
resolved settings, share one capture and one encode. The registry keys
captures by resolved settings, not by name.

## Profile → capture settings

The vdo builder (acap-rs fork, rev `b1f674c`) supports: codec, channel,
resolution, framerate, GOP length. Mapping:

| Profile key | Capture setting |
|---|---|
| `videocodec` (`h264`/`h265`) | codec. Anything else → unsupported, the mapping shows an error |
| `resolution` (`WxH`) | width/height |
| `fps` | framerate (`0` or missing → camera default capture rate) |
| `camera` | VDO channel (to verify on device: profile `camera=1` → VDO channel 1) |
| `videokeyframeinterval` | `gop_length` |
| `compression`, `videobitrate`, `videomaxbitrate`, `videozprofile`, `audio`, others | **not applied**; listed as "ignored settings" next to the mapping in the UI |

Follow-up (not in this spec): add compression/bitrate to the acap-rs fork's
`StreamBuilder` so more profile keys apply.

## Admin UI

One page, `html/index.html`, all assets bundled in the `.eap` (no CDN;
cameras are often offline). Multimux Edge wordmark, light/dark theme from
`prefers-color-scheme`, laid out like a camera settings UI: a left sidebar
and one content panel per section.

- **Streams.** A table with columns name · camera profile (dropdown filled
  from `GET /admin/profiles`) · default (radio) · status · URL (copy button) ·
  remove. "Add stream" row. Inline validation. "Ignored settings" chips per
  row. A missing profile shows a warning on its row.
- **Main preset.** Today's channel/resolution/fps/codec fields.
- **Encoder.** `max_encodes`, `idle_timeout_secs`, plus the measured guidance
  ("2 full-rate encodes on ARTPEC-6; more slows every stream").
- **LL-HLS.** Today's tuning fields.
- **Status.** Live captures: name(s), resolved settings, state
  (idle/starting/running/error), fps, last error; encodes in use N/M.
  Polled every 5 s.
- **About.** Version, links to docs/repo/API reference.
- The player page gets a stream picker (default, main, each mapping).

One Save button applies live and shows "Applied". No inline `style=""` attributes; all styling lives in
the page's stylesheet.

## API and OpenAPI

| Method + path | Notes |
|---|---|
| `GET /admin/config` | v2 shape |
| `POST /admin/config` | v2 shape; 400 with `{field, message}` list on validation failure; applies live |
| `GET /admin/status` | adds `streams: [{names, settings, state, fps, last_error}]`, `encodes: {in_use, max}` |
| `GET /admin/profiles` | `[{name, description, parameters, parsed, ignored_keys}]` from the camera |
| `GET /admin/openapi.json` | generated by `utoipa` from the types and handlers |

Annotate types with `utoipa::ToSchema` and handlers with `#[utoipa::path]`.
Assemble with `#[derive(OpenApi)]`. utoipa 6.x, framework-agnostic core only
(no `utoipa-axum`; axum 0.8, as multimux 0.11 requires). The docs site gets an "API
reference" page that renders the spec, and CI writes `openapi.json` into the
site build via a small host-built binary or test, so docs never drift.
All endpoints stay behind the camera's `admin` reverse-proxy access level, as
today.

## Error handling

| Situation | Behaviour |
|---|---|
| VAPIX credentials or profile list unavailable | Mapped streams return 503 "profile source unavailable"; the main preset still works; status and UI show the reason |
| Mapped profile deleted on the camera | That mapping returns 503 "profile ACC_X not found"; the UI flags the row |
| Unsupported codec in profile | Mapping errors (503 + UI flag); the other mappings are unaffected |
| Encode cap reached | 503 + `Retry-After: 5`; status shows N/M |
| Capture fails at runtime | Existing supervisor backoff per stream; per-stream `last_error` |
| Invalid config POST | 400, nothing stored or applied |
| Config backend broken | As today: run on defaults (main only), report via status |

## Testing

**Host (CI `host` job):**
- Config: validation (names, reserved words, default must exist, ranges),
  old-shape migration, serde round trip.
- Port constant equals the `reverseProxy` target port in `manifest.json`.
- Profile parser: every key in the table, odd inputs, real strings from
  the P1448-LE.
- Registry with a fake capture: on-demand start, idle stop, shared encode
  for identical settings, cap → 503, live apply (change, remove, add,
  default switch).
- Front router: spike pattern as real tests (404/redirect/forward/remove).
- OpenAPI: the spec generates, contains every endpoint, and the snapshot
  matches the committed one.

**Device (P1448-LE, required before "done"):**
1. Fresh install: no config-load error (truncation fix).
2. Map `medium → ACC_Medium`. `/hls/medium/media.m3u8` plays (ffprobe
   1280x720 h264). The bare URL serves the default.
3. Change the mapping live to `ACC_Low` and confirm the new resolution with
   no restart; remove it → 404.
4. One 4K stream plus one other stream both run at 25 fps; a third distinct
   encode → 503 with cap 2.
5. Idle stop: after the timeout, status shows idle and the encode is freed.
6. The admin page works end to end; `openapi.json` is served.

## Open items to verify in implementation

- VAPIX service-account call end to end from inside the app (manifest
  `resources.dbus.requiredMethods`; local VAPIX host as documented by Axis).
- Profile `camera=N` → VDO channel numbering.
- Behaviour when the camera's own consumers (e.g. Camera Station) already
  hold encodes: the cap is ours only, so the UI states that other clients
  share the encoder.
