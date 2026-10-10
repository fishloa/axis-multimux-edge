# Stream Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve camera stream profiles at operator-chosen LL-HLS URLs (`/hls/medium/media.m3u8` → profile `ACC_Medium`), captured on demand, configured live from a redesigned admin page with an OpenAPI-documented API.

**Architecture:** A `Registry` maps names to capture settings (from camera profiles via VAPIX, or the built-in `main` preset), starts one VDO capture per distinct settings on first request, stops it when idle, and enforces an encode cap. A front router forwards `/hls/{name}/…` to a per-capture multimux origin router (pattern proven in the spike). The config becomes v2 (mappings, default, cap, idle timeout); `POST /admin/config` applies it live. `utoipa` generates `openapi.json`.

**Tech Stack:** Rust 2024 (toolchain 1.97), axum 0.8, tokio, tokio-util 0.7, multimux 0.11, serde, utoipa 6, acap-rs fork (`vdo`, `axparameter`, `acap-logging`, `acap-vapix`) at rev `b1f674c7b5fdde8e171911c591cf1ecb0b3296b6`.

**Spec:** `docs/superpowers/specs/2026-10-02-stream-profiles-design.md`

## Global Constraints

- Branch `stream-profiles` (based on `main` at v0.2.0: multimux 0.11, axum 0.8, the first-run config fix already shipped). Commit after every task; never discard uncommitted work.
- Host build has no `device` feature: `cargo test --locked`, `cargo clippy --locked -- -D warnings`, `cargo fmt --all --check` must pass after every task (this is CI's `host` job).
- Device-only code goes behind `#[cfg(feature = "device")]`, as today.
- App listens on `127.0.0.1:2999` always; `APP_PORT` must equal the port in `manifest.json`'s `reverseProxy` targets. No `port` config field.
- Stream names: `^[a-z0-9][a-z0-9-]{0,31}$`, unique; reserved: `main`, `media.m3u8`.
- `max_encodes` 1–8, default 2. `idle_timeout_secs` 5–600, default 30.
- Every setting applies live via `POST /admin/config`.
- Admin UI: Multimux Edge brand (not Axis styling), all assets bundled in `html/` (no CDN), **zero inline `style=""` attributes**, light/dark via `prefers-color-scheme`.
- No FastCGI; reverse proxy only.
- Commit messages: no `Co-Authored-By` line. End with `Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza`.
- "Done" = flashed on the P1448-LE (192.168.20.26, fw 11.11, armv7hf) and verified (Task 11). Host tests passing is not done.

## Review Focus

1. **Concurrent first requests for an idle stream** (a player fires playlist + init + part requests together) → exactly one capture starts. Test in Task 5.
2. **Config save while a viewer watches an unchanged mapping** → that stream keeps running, not restarted. Test in Task 5.
3. **Mapped profile deleted on the camera** → that name answers 503 "profile … not found", other names unaffected, no 500/panic. Test in Task 5.
4. **LL-HLS blocking-reload query strings** (`?_HLS_msn=12&_HLS_part=3`) survive both the forward and the bare-URL redirect. Test in Task 6.
5. **Bad mapping names** (`Medium`, `my stream`, `main`, duplicate, 33 chars, default pointing at a missing name) → 400 with a field-level message and nothing stored. Test in Task 1.

## Deviations from the spec (decided while planning)

- **Missing profile keys fall back to the `main` preset** for codec, resolution and camera/channel (Axis profiles may omit keys meaning "camera default"); a missing `fps` means 0 = camera default rate, as the spec says.
- **`GET /admin/status` shape is replaced**, not extended: per-pipeline fields (`running`, `current_segment`, …) move into each entry of `streams`; top level keeps `last_error` (config-backend error) and adds `encodes`. The single-pipeline fields no longer mean anything with several captures. Recorded in CHANGELOG as breaking.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/config.rs` | new | Config v2 types, defaults, validation (field errors), old-shape migration, `APP_PORT` |
| `src/profile.rs` | new | `CaptureSettings`; parse a VAPIX profile parameter string into settings + ignored keys |
| `src/profile_source.rs` | new | `ProfileSource` trait, `StaticProfileSource` (tests), VAPIX response parser, device `VapixProfileSource` |
| `src/registry.rs` | new | Name → capture resolution, on-demand start, shared captures, encode cap, idle sweep, live apply, status |
| `src/routing.rs` | new | `/hls` front router: bare-URL redirect, `/{name}/{rest}` forward, error responses |
| `src/openapi.rs` | new | `ApiDoc` (`utoipa::OpenApi`) |
| `src/admin.rs` | modify | Uses `config::Config`; live apply; `/admin/profiles`; new status; `/admin/openapi.json`; first-run fix |
| `src/convert.rs` | modify | `Codec` gains `Hash` |
| `src/vdo_source.rs` | modify | `VdoIngestSession::new(&CaptureSettings)` |
| `src/bin/multimux-edge.rs` | modify | `VdoCaptureFactory`; wire registry, routing, admin, sweeper |
| `src/lib.rs` | modify | New modules |
| `Cargo.toml` | modify | `utoipa`, `acap-vapix` (device) |
| `manifest.json` | modify | `resources.dbus.requiredMethods` |
| `tests/openapi_snapshot.rs` | new | Committed `docs/src/api/openapi.json` matches code |
| `html/index.html`, `html/app.css`, `html/app.js` | rewrite/new | Admin UI |
| `html/player.html` | modify | Stream picker |
| `docs/src/…`, `CHANGELOG.md` | modify | Docs, API reference page, changelog |

---

### Task 1: Config v2

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs`, `src/admin.rs` (replace the `Config` struct, its constants, `Default`, `validate`; keep everything else), `Cargo.toml`
- Test: unit tests in `src/config.rs`; existing tests in `src/admin.rs` updated

**Interfaces:**
- Produces:
  - `pub const APP_PORT: u16 = 2999;`
  - `pub struct MainPreset { pub channel: u32, pub width: u32, pub height: u32, pub framerate: u32, pub codec: String }`
  - `pub struct StreamMapping { pub name: String, pub profile: String }`
  - `pub struct Config { pub main: MainPreset, pub streams: Vec<StreamMapping>, pub default_stream: Option<String>, pub max_encodes: u32, pub idle_timeout_secs: u64, pub target_duration_secs: f64, pub part_target_ms: u32, pub window_segments: usize }` — `Serialize`, `Deserialize` (accepts old flat shape), `Clone`, `PartialEq`, `Debug`, `Default`, `utoipa::ToSchema`
  - `pub struct FieldError { pub field: String, pub message: String }`
  - `impl Config { pub fn validate(&self) -> Result<(), Vec<FieldError>>; pub fn llhls_eq(&self, other: &Config) -> bool }`
  - `pub const MAIN_NAME: &str = "main";`
  - `admin.rs` re-exports: `pub use crate::config::Config;`

- [ ] **Step 1: Add `utoipa` and the module**

`Cargo.toml` `[dependencies]` add:
```toml
utoipa    = "6"
```
`src/lib.rs`: add `pub mod config;` above `pub mod convert;`.

- [ ] **Step 2: Write the failing tests** (bottom of new `src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn with_streams(streams: &[(&str, &str)], default: Option<&str>) -> Config {
        Config {
            streams: streams
                .iter()
                .map(|(n, p)| StreamMapping { name: n.to_string(), profile: p.to_string() })
                .collect(),
            default_stream: default.map(str::to_string),
            ..Config::default()
        }
    }

    fn fields(r: Result<(), Vec<FieldError>>) -> Vec<String> {
        r.unwrap_err().into_iter().map(|e| e.field).collect()
    }

    #[test]
    fn default_is_valid_and_has_spec_values() {
        let c = Config::default();
        assert_eq!(c.max_encodes, 2);
        assert_eq!(c.idle_timeout_secs, 30);
        assert!(c.streams.is_empty());
        assert_eq!(c.default_stream, None);
        assert_eq!(c.main.codec, "h264");
        c.validate().unwrap();
    }

    #[test]
    fn accepts_good_names() {
        with_streams(&[("medium", "ACC_Medium"), ("4k", "ACC_High"), ("a-b-1", "X")], Some("4k"))
            .validate()
            .unwrap();
    }

    #[test]
    fn rejects_bad_names_with_field_paths() {
        for bad in ["Medium", "my stream", "-lead", "", &"a".repeat(33), "main", "media.m3u8"] {
            let f = fields(with_streams(&[(bad, "P")], None).validate());
            assert_eq!(f, vec!["streams[0].name".to_string()], "name {bad:?}");
        }
    }

    #[test]
    fn rejects_duplicate_names() {
        let f = fields(with_streams(&[("a", "P"), ("a", "Q")], None).validate());
        assert_eq!(f, vec!["streams[1].name".to_string()]);
    }

    #[test]
    fn rejects_empty_profile() {
        let f = fields(with_streams(&[("a", "")], None).validate());
        assert_eq!(f, vec!["streams[0].profile".to_string()]);
    }

    #[test]
    fn default_must_name_a_mapping() {
        let f = fields(with_streams(&[("a", "P")], Some("b")).validate());
        assert_eq!(f, vec!["default_stream".to_string()]);
        with_streams(&[("a", "P")], Some("a")).validate().unwrap();
    }

    #[test]
    fn range_checks() {
        let mut c = Config::default();
        c.max_encodes = 0;
        c.idle_timeout_secs = 4;
        c.part_target_ms = 0;
        c.window_segments = 0;
        c.target_duration_secs = 0.0;
        c.main.codec = "vp9".into();
        let mut f = fields(c.validate());
        f.sort();
        assert_eq!(
            f,
            vec![
                "idle_timeout_secs", "main.codec", "max_encodes", "part_target_ms",
                "target_duration_secs", "window_segments",
            ]
        );
        let mut c = Config::default();
        c.max_encodes = 9;
        c.idle_timeout_secs = 601;
        assert_eq!(fields(c.validate()).len(), 2);
    }

    #[test]
    fn migrates_old_flat_shape_and_ignores_port() {
        let old = r#"{"channel":1,"width":1280,"height":720,"framerate":25,"codec":"h265",
            "target_duration_secs":2.0,"part_target_ms":333,"window_segments":6,"port":2999}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert_eq!(
            c.main,
            MainPreset { channel: 1, width: 1280, height: 720, framerate: 25, codec: "h265".into() }
        );
        assert_eq!(c.target_duration_secs, 2.0);
        assert_eq!(c.part_target_ms, 333);
        assert_eq!(c.window_segments, 6);
        assert!(c.streams.is_empty());
        assert_eq!(c.max_encodes, 2);
    }

    #[test]
    fn v2_round_trips_and_has_no_port() {
        let c = with_streams(&[("medium", "ACC_Medium")], Some("medium"));
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("port"));
        assert_eq!(serde_json::from_str::<Config>(&s).unwrap(), c);
    }

    #[test]
    fn llhls_eq_compares_only_tuning() {
        let a = Config::default();
        let mut b = with_streams(&[("x", "P")], None);
        assert!(a.llhls_eq(&b));
        b.part_target_ms = 250;
        assert!(!a.llhls_eq(&b));
    }

    #[test]
    fn app_port_matches_manifest_reverse_proxy() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../manifest.json")).unwrap();
        let targets = manifest["acapPackageConf"]["configuration"]["reverseProxy"]
            .as_array()
            .unwrap();
        assert!(!targets.is_empty());
        for t in targets {
            assert_eq!(
                t["target"].as_str().unwrap(),
                format!("http://localhost:{APP_PORT}"),
                "manifest reverseProxy target must match APP_PORT"
            );
        }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib config::`
Expected: compile errors (`Config`, `StreamMapping`, … not found).

- [ ] **Step 4: Implement `src/config.rs`** (above the tests)

```rust
//! The app's persisted configuration (v2): the built-in `main` capture
//! preset, the name → camera-stream-profile mappings, the default stream,
//! the encode cap, the idle timeout, and global LL-HLS tuning.
//!
//! Stored as one JSON value (see `crate::admin::ConfigStore`). A value in the
//! pre-v2 flat shape (capture fields at top level, plus `port`) still loads:
//! its capture fields become `main`, `port` is ignored.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// The local port the app listens on. Not configurable: AXIS OS's reverse
/// proxy forwards to the target fixed in `manifest.json` at build time, so
/// any other port would cut the app off (a host test checks they match).
pub const APP_PORT: u16 = 2999;

/// Reserved stream name that always means the built-in [`MainPreset`].
pub const MAIN_NAME: &str = "main";

const RESERVED_NAMES: [&str; 2] = [MAIN_NAME, "media.m3u8"];
const MAX_NAME_LEN: usize = 32;

/// Built-in capture preset, served at `/hls/main/…` and used when no default
/// stream is set. Also supplies values a camera profile leaves out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MainPreset {
    /// VDO channel index.
    pub channel: u32,
    /// Capture width, pixels.
    pub width: u32,
    /// Capture height, pixels.
    pub height: u32,
    /// Frame rate, fps (0 = camera default).
    pub framerate: u32,
    /// `"h264"` or `"h265"`.
    pub codec: String,
}

impl Default for MainPreset {
    fn default() -> Self {
        MainPreset { channel: 0, width: 1920, height: 1080, framerate: 30, codec: "h264".into() }
    }
}

/// One URL name mapped to one camera stream profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StreamMapping {
    /// URL segment: `/hls/<name>/media.m3u8`.
    pub name: String,
    /// Camera stream profile name, exactly as the camera lists it.
    pub profile: String,
}

/// The whole app configuration.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Config {
    /// Built-in capture preset.
    pub main: MainPreset,
    /// Name → camera profile mappings.
    pub streams: Vec<StreamMapping>,
    /// Name served by the bare `/hls/media.m3u8`; `null` = `main`.
    pub default_stream: Option<String>,
    /// Maximum concurrent distinct encodes this app may hold (1–8).
    pub max_encodes: u32,
    /// Seconds without requests before a stream's capture stops (5–600).
    pub idle_timeout_secs: u64,
    /// LL-HLS target segment duration, seconds.
    pub target_duration_secs: f64,
    /// LL-HLS target part duration, milliseconds.
    pub part_target_ms: u32,
    /// Segments kept in the media playlist window.
    pub window_segments: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            main: MainPreset::default(),
            streams: Vec::new(),
            default_stream: None,
            max_encodes: 2,
            idle_timeout_secs: 30,
            target_duration_secs: 4.0,
            part_target_ms: 500,
            window_segments: 8,
        }
    }
}

/// One validation failure, addressed by a JSON-path-like field name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FieldError {
    /// e.g. `streams[1].name`, `max_encodes`.
    pub field: String,
    /// Human-readable reason.
    pub message: String,
}

fn err(field: impl Into<String>, message: impl Into<String>) -> FieldError {
    FieldError { field: field.into(), message: message.into() }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= MAX_NAME_LEN
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Config {
    /// Every problem at once, so the UI can mark each field.
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        if self.main.codec != "h264" && self.main.codec != "h265" {
            errors.push(err("main.codec", "must be \"h264\" or \"h265\""));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.streams.iter().enumerate() {
            let field = format!("streams[{i}].name");
            if RESERVED_NAMES.contains(&s.name.as_str()) {
                errors.push(err(field, format!("\"{}\" is reserved", s.name)));
            } else if !valid_name(&s.name) {
                errors.push(err(
                    field,
                    "use 1–32 of a-z, 0-9 and -, starting with a letter or digit",
                ));
            } else if !seen.insert(s.name.as_str()) {
                errors.push(err(field, format!("\"{}\" is used twice", s.name)));
            }
            if s.profile.trim().is_empty() {
                errors.push(err(format!("streams[{i}].profile"), "choose a camera profile"));
            }
        }
        if let Some(d) = &self.default_stream {
            if !self.streams.iter().any(|s| &s.name == d) {
                errors.push(err("default_stream", format!("no stream named \"{d}\"")));
            }
        }
        if !(1..=8).contains(&self.max_encodes) {
            errors.push(err("max_encodes", "must be 1–8"));
        }
        if !(5..=600).contains(&self.idle_timeout_secs) {
            errors.push(err("idle_timeout_secs", "must be 5–600"));
        }
        if self.target_duration_secs <= 0.0 {
            errors.push(err("target_duration_secs", "must be positive"));
        }
        if self.part_target_ms == 0 {
            errors.push(err("part_target_ms", "must be positive"));
        }
        if self.window_segments == 0 {
            errors.push(err("window_segments", "must be positive"));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// Whether the LL-HLS tuning (which every running stream was built with)
    /// is unchanged.
    pub fn llhls_eq(&self, other: &Config) -> bool {
        self.target_duration_secs == other.target_duration_secs
            && self.part_target_ms == other.part_target_ms
            && self.window_segments == other.window_segments
    }
}

/// Wire shape accepted on load: v2 fields, plus the pre-v2 flat capture
/// fields. Unknown fields (e.g. old `port`) are ignored.
#[derive(Deserialize)]
struct ConfigWire {
    main: Option<MainPreset>,
    #[serde(default)]
    streams: Vec<StreamMapping>,
    #[serde(default)]
    default_stream: Option<String>,
    max_encodes: Option<u32>,
    idle_timeout_secs: Option<u64>,
    target_duration_secs: Option<f64>,
    part_target_ms: Option<u32>,
    window_segments: Option<usize>,
    channel: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    framerate: Option<u32>,
    codec: Option<String>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ConfigWire::deserialize(d)?;
        let dflt = Config::default();
        let main = w.main.unwrap_or_else(|| {
            let m = MainPreset::default();
            MainPreset {
                channel: w.channel.unwrap_or(m.channel),
                width: w.width.unwrap_or(m.width),
                height: w.height.unwrap_or(m.height),
                framerate: w.framerate.unwrap_or(m.framerate),
                codec: w.codec.clone().unwrap_or(m.codec),
            }
        });
        Ok(Config {
            main,
            streams: w.streams,
            default_stream: w.default_stream,
            max_encodes: w.max_encodes.unwrap_or(dflt.max_encodes),
            idle_timeout_secs: w.idle_timeout_secs.unwrap_or(dflt.idle_timeout_secs),
            target_duration_secs: w.target_duration_secs.unwrap_or(dflt.target_duration_secs),
            part_target_ms: w.part_target_ms.unwrap_or(dflt.part_target_ms),
            window_segments: w.window_segments.unwrap_or(dflt.window_segments),
        })
    }
}
```

- [ ] **Step 5: Switch `admin.rs` to the new `Config`**

In `src/admin.rs`: delete the `DEFAULT_*` constants, the `Config` struct, `impl Default for Config` and `impl Config { fn validate … }` (lines 24–107 today). Add near the top `pub use crate::config::Config;`. In `post_config`, replace the validation branch with:

```rust
    if let Err(errors) = cfg.validate() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "errors": errors })))
            .into_response();
    }
```

Update the existing admin tests that build `Config { codec: …, ..Config::default() }` to `Config { main: crate::config::MainPreset { codec: "h265".into(), ..Default::default() }, ..Config::default() }`, and the invalid-codec test the same way with `"vp9"`. Delete any assertion on `port`.

In `src/bin/multimux-edge.rs` replace `cfg.port` with `multimux_edge::config::APP_PORT`, and `cfg.channel/width/height/framerate/codec` with `cfg.main.…` (temporary; Task 8 rewrites this file). It does not build on host, so just keep it consistent.

- [ ] **Step 6: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: all pass, including `config::tests::*` and the updated `admin::tests::*`.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs src/lib.rs src/admin.rs src/bin/multimux-edge.rs
git commit -m "Config v2: stream mappings, default, encode cap, idle timeout; drop port" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 2: Profile parameter parser

**Files:**
- Create: `src/profile.rs`
- Modify: `src/lib.rs` (`pub mod profile;`), `src/convert.rs` (`Codec` derive gains `Hash`)

**Interfaces:**
- Consumes: `crate::config::MainPreset`, `crate::convert::Codec`
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct CaptureSettings { pub codec: Codec, pub channel: u32, pub width: u32, pub height: u32, pub framerate: u32, pub gop_length: Option<u32> }`
  - `impl CaptureSettings { pub fn from_main(m: &MainPreset) -> Result<Self, String>; pub fn describe(&self) -> String }` (e.g. `"h264 1280x720@25 ch1"`)
  - `pub struct ParsedProfile { pub settings: CaptureSettings, pub ignored_keys: Vec<String> }`
  - `pub fn parse_profile(parameters: &str, fallback: &MainPreset) -> Result<ParsedProfile, String>`

- [ ] **Step 1: Write the failing tests** (bottom of `src/profile.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn main_preset() -> MainPreset {
        MainPreset { channel: 0, width: 1920, height: 1080, framerate: 30, codec: "h264".into() }
    }

    #[test]
    fn parses_real_p1448_profile() {
        let p = parse_profile(
            "camera=1&videocodec=h264&fps=15&resolution=1280x720&compression=30&audio=0",
            &main_preset(),
        )
        .unwrap();
        assert_eq!(
            p.settings,
            CaptureSettings { codec: Codec::H264, channel: 1, width: 1280, height: 720, framerate: 15, gop_length: None }
        );
        assert_eq!(p.ignored_keys, vec!["audio".to_string(), "compression".to_string()]);
    }

    #[test]
    fn missing_keys_fall_back_to_main_and_fps_to_zero() {
        let p = parse_profile("resolution=640x360", &main_preset()).unwrap();
        assert_eq!(p.settings.codec, Codec::H264);
        assert_eq!(p.settings.channel, 0);
        assert_eq!(p.settings.framerate, 0);
        assert_eq!((p.settings.width, p.settings.height), (640, 360));
        let p = parse_profile("", &main_preset()).unwrap();
        assert_eq!((p.settings.width, p.settings.height), (1920, 1080));
    }

    #[test]
    fn h265_and_keyframe_interval() {
        let p = parse_profile("videocodec=h265&videokeyframeinterval=50", &main_preset()).unwrap();
        assert_eq!(p.settings.codec, Codec::H265);
        assert_eq!(p.settings.gop_length, Some(50));
    }

    #[test]
    fn rejects_unsupported_codec_and_bad_numbers() {
        assert!(parse_profile("videocodec=mjpeg", &main_preset()).unwrap_err().contains("mjpeg"));
        assert!(parse_profile("resolution=wide", &main_preset()).is_err());
        assert!(parse_profile("fps=fast", &main_preset()).is_err());
        assert!(parse_profile("resolution=0x720", &main_preset()).is_err());
    }

    #[test]
    fn percent_decoding_and_case() {
        let p = parse_profile("VideoCodec=H264&resolution=1280%78720", &main_preset()).unwrap();
        assert_eq!(p.settings.codec, Codec::H264);
        assert_eq!(p.settings.width, 1280);
    }

    #[test]
    fn ignored_keys_are_sorted_and_unique() {
        let p = parse_profile("zz=1&audio=0&audio=1&compression=3", &main_preset()).unwrap();
        assert_eq!(p.ignored_keys, vec!["audio", "compression", "zz"]);
    }

    #[test]
    fn describe_is_compact() {
        let s = CaptureSettings { codec: Codec::H265, channel: 1, width: 3840, height: 2160, framerate: 25, gop_length: None };
        assert_eq!(s.describe(), "h265 3840x2160@25 ch1");
        let s = CaptureSettings { framerate: 0, ..s };
        assert_eq!(s.describe(), "h265 3840x2160@auto ch1");
    }

    #[test]
    fn from_main_rejects_bad_codec() {
        let mut m = main_preset();
        m.codec = "vp9".into();
        assert!(CaptureSettings::from_main(&m).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib profile::`
Expected: compile error, `parse_profile` not found.

- [ ] **Step 3: Implement** (above the tests)

```rust
//! Turns an Axis stream profile's parameter string (VAPIX
//! `streamprofile.cgi`, e.g. `resolution=1280x720&fps=25&videocodec=h264`)
//! into the capture settings VDO supports. Keys VDO can't apply through the
//! acap-rs `StreamBuilder` (compression, bitrate, audio, …) are reported as
//! ignored rather than silently dropped.

use crate::config::MainPreset;
use crate::convert::Codec;

/// Everything a VDO capture needs. Two streams with equal settings share one
/// capture (and one hardware encode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureSettings {
    pub codec: Codec,
    pub channel: u32,
    pub width: u32,
    pub height: u32,
    /// 0 = camera default rate.
    pub framerate: u32,
    /// Key-frame interval in frames; `None` = derive from framerate.
    pub gop_length: Option<u32>,
}

impl CaptureSettings {
    pub fn from_main(m: &MainPreset) -> Result<Self, String> {
        Ok(CaptureSettings {
            codec: parse_codec(&m.codec)?,
            channel: m.channel,
            width: m.width,
            height: m.height,
            framerate: m.framerate,
            gop_length: None,
        })
    }

    /// Short human form for status and logs.
    pub fn describe(&self) -> String {
        let codec = match self.codec {
            Codec::H264 => "h264",
            Codec::H265 => "h265",
        };
        let fps = if self.framerate == 0 { "auto".to_string() } else { self.framerate.to_string() };
        format!("{codec} {}x{}@{fps} ch{}", self.width, self.height, self.channel)
    }
}

/// Parsed profile: what will be applied, and which keys won't.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProfile {
    pub settings: CaptureSettings,
    /// Sorted, de-duplicated, lower-case.
    pub ignored_keys: Vec<String>,
}

fn parse_codec(v: &str) -> Result<Codec, String> {
    match v.to_ascii_lowercase().as_str() {
        "h264" => Ok(Codec::H264),
        "h265" => Ok(Codec::H265),
        other => Err(format!("unsupported video codec \"{other}\" (h264 or h265 only)")),
    }
}

fn parse_u32(key: &str, v: &str) -> Result<u32, String> {
    v.parse::<u32>().map_err(|_| format!("{key}: \"{v}\" is not a number"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `parameters`; keys the profile leaves out come from `fallback`
/// (codec, resolution, channel). A missing `fps` means camera default (0).
pub fn parse_profile(parameters: &str, fallback: &MainPreset) -> Result<ParsedProfile, String> {
    let mut settings = CaptureSettings::from_main(fallback)?;
    settings.framerate = 0;
    let mut ignored = std::collections::BTreeSet::new();
    for pair in parameters.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(k).to_ascii_lowercase();
        let value = percent_decode(v);
        match key.as_str() {
            "videocodec" => settings.codec = parse_codec(&value)?,
            "resolution" => {
                let (w, h) = value
                    .to_ascii_lowercase()
                    .split_once('x')
                    .map(|(w, h)| (w.to_string(), h.to_string()))
                    .ok_or_else(|| format!("resolution: \"{value}\" is not WxH"))?;
                let (w, h) = (parse_u32("resolution", &w)?, parse_u32("resolution", &h)?);
                if w == 0 || h == 0 {
                    return Err(format!("resolution: \"{value}\" has a zero dimension"));
                }
                settings.width = w;
                settings.height = h;
            }
            "fps" => settings.framerate = parse_u32("fps", &value)?,
            "camera" => settings.channel = parse_u32("camera", &value)?,
            "videokeyframeinterval" => {
                settings.gop_length = Some(parse_u32("videokeyframeinterval", &value)?)
            }
            _ => {
                ignored.insert(key);
            }
        }
    }
    Ok(ParsedProfile { settings, ignored_keys: ignored.into_iter().collect() })
}
```

In `src/convert.rs` change `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` on `Codec` to `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]`. Add `pub mod profile;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/profile.rs src/lib.rs src/convert.rs
git commit -m "Parse Axis stream profile parameters into capture settings" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 3: Profile source (VAPIX)

**Files:**
- Create: `src/profile_source.rs`
- Modify: `src/lib.rs` (`pub mod profile_source;`), `Cargo.toml` (`acap-vapix`, device feature), `manifest.json`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)] pub struct CameraProfile { pub name: String, pub description: String, pub parameters: String }`
  - `#[async_trait::async_trait]` is NOT used; trait uses boxed futures:
    `pub trait ProfileSource: Send + Sync + 'static { fn list(&self) -> BoxFuture<'_, Result<Vec<CameraProfile>, String>>; }` with `pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;`
  - `pub struct StaticProfileSource(pub std::sync::Mutex<Result<Vec<CameraProfile>, String>>)` with `StaticProfileSource::new(Vec<CameraProfile>)`, `::failing(msg)`, `set(Result<…>)`
  - `pub fn parse_list_response(body: &str) -> Result<Vec<CameraProfile>, String>`
  - device: `pub struct VapixProfileSource` with `VapixProfileSource::new() -> Self` (caches 10 s)

- [ ] **Step 1: Write failing tests** (bottom of `src/profile_source.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const P1448: &str = r#"{"method":"list","apiVersion":"1.0","data":{"streamProfile":[
        {"name":"ACC_High","description":"","parameters":"resolution=3840x2160&fps=25&videocodec=h264"},
        {"name":"View Area 1_ACS_Pro_Low","description":"ACS low","parameters":"camera=1&videocodec=h264&fps=5&resolution=640x360"}
        ],"maxProfiles":26}}"#;

    #[test]
    fn parses_camera_list_response() {
        let list = parse_list_response(P1448).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].name, "View Area 1_ACS_Pro_Low");
        assert_eq!(list[1].description, "ACS low");
        assert!(list[0].parameters.contains("3840x2160"));
    }

    #[test]
    fn reports_vapix_error_object() {
        let e = parse_list_response(r#"{"apiVersion":"1.0","error":{"code":2002,"message":"Bad"}}"#)
            .unwrap_err();
        assert!(e.contains("2002") && e.contains("Bad"), "{e}");
        assert!(parse_list_response("not json").is_err());
    }

    #[tokio::test]
    async fn static_source_returns_and_can_change() {
        let src = StaticProfileSource::new(vec![CameraProfile {
            name: "A".into(),
            description: String::new(),
            parameters: "fps=5".into(),
        }]);
        assert_eq!(src.list().await.unwrap().len(), 1);
        src.set(Err("down".into()));
        assert_eq!(src.list().await.unwrap_err(), "down");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib profile_source::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Where camera stream profiles come from. On the camera:
//! [`VapixProfileSource`] calls VAPIX `streamprofile.cgi` through the
//! acap-vapix local client (service-account credentials over D-Bus, local
//! VAPIX at `http://127.0.0.12`). In tests: [`StaticProfileSource`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One stream profile as the camera lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CameraProfile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// VAPIX parameter string, e.g. `resolution=1280x720&fps=25`.
    pub parameters: String,
}

pub trait ProfileSource: Send + Sync + 'static {
    fn list(&self) -> BoxFuture<'_, Result<Vec<CameraProfile>, String>>;
}

#[derive(Deserialize)]
struct ListResponse {
    data: Option<ListData>,
    error: Option<VapixError>,
}
#[derive(Deserialize)]
struct ListData {
    #[serde(rename = "streamProfile", default)]
    stream_profile: Vec<CameraProfile>,
}
#[derive(Deserialize)]
struct VapixError {
    code: i64,
    message: String,
}

/// Parse a `streamprofile.cgi` `list` JSON response.
pub fn parse_list_response(body: &str) -> Result<Vec<CameraProfile>, String> {
    let r: ListResponse =
        serde_json::from_str(body).map_err(|e| format!("streamprofile.cgi: bad JSON: {e}"))?;
    if let Some(e) = r.error {
        return Err(format!("streamprofile.cgi error {}: {}", e.code, e.message));
    }
    Ok(r.data.map(|d| d.stream_profile).unwrap_or_default())
}

/// Fixed answer, for host tests and host builds.
pub struct StaticProfileSource(pub Mutex<Result<Vec<CameraProfile>, String>>);

impl StaticProfileSource {
    pub fn new(profiles: Vec<CameraProfile>) -> Self {
        StaticProfileSource(Mutex::new(Ok(profiles)))
    }
    pub fn failing(msg: &str) -> Self {
        StaticProfileSource(Mutex::new(Err(msg.to_string())))
    }
    pub fn set(&self, r: Result<Vec<CameraProfile>, String>) {
        *self.0.lock().expect("profile source mutex") = r;
    }
}

impl ProfileSource for StaticProfileSource {
    fn list(&self) -> BoxFuture<'_, Result<Vec<CameraProfile>, String>> {
        let r = self.0.lock().expect("profile source mutex").clone();
        Box::pin(async move { r })
    }
}

#[cfg(feature = "device")]
pub use device::VapixProfileSource;

#[cfg(feature = "device")]
mod device {
    use std::time::{Duration, Instant};

    use super::*;

    const CACHE_FOR: Duration = Duration::from_secs(10);

    /// VAPIX-backed source with a short cache, so a burst of first requests
    /// doesn't hit the camera once each.
    pub struct VapixProfileSource {
        cache: tokio::sync::Mutex<Option<(Instant, Vec<CameraProfile>)>>,
    }

    impl VapixProfileSource {
        pub fn new() -> Self {
            VapixProfileSource { cache: tokio::sync::Mutex::new(None) }
        }

        async fn fetch() -> Result<Vec<CameraProfile>, String> {
            let client = acap_vapix::local_client()
                .map_err(|e| format!("VAPIX service account unavailable: {e}"))?;
            let body = serde_json::json!({
                "apiVersion": "1.0",
                "method": "list",
                "params": { "streamProfileName": [] }
            });
            let resp = client
                .post("axis-cgi/streamprofile.cgi")
                .map_err(|e| format!("streamprofile.cgi url: {e}"))?
                .replace_with(|b| b.json(&body))
                .send()
                .await
                .map_err(|e| format!("streamprofile.cgi request: {e}"))?;
            let text = resp.text().await.map_err(|e| format!("streamprofile.cgi body: {e}"))?;
            parse_list_response(&text)
        }
    }

    impl ProfileSource for VapixProfileSource {
        fn list(&self) -> BoxFuture<'_, Result<Vec<CameraProfile>, String>> {
            Box::pin(async move {
                let mut cache = self.cache.lock().await;
                if let Some((at, list)) = cache.as_ref() {
                    if at.elapsed() < CACHE_FOR {
                        return Ok(list.clone());
                    }
                }
                let list = Self::fetch().await?;
                *cache = Some((Instant::now(), list.clone()));
                Ok(list)
            })
        }
    }
}
```

`Cargo.toml` `[dependencies]`, next to the other acap-rs crates:
```toml
acap-vapix   = { git = "https://github.com/fishloa/acap-rs", rev = "b1f674c7b5fdde8e171911c591cf1ecb0b3296b6", optional = true }
```
and add `"dep:acap-vapix"` to the `device` feature list.

`manifest.json`: add a top-level `resources` block (sibling of `schemaVersion`/`acapPackageConf`):
```json
    "resources": {
        "dbus": {
            "requiredMethods": [
                "com.axis.HTTPConf1.VAPIXServiceAccounts1.GetCredentials"
            ]
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS (device module not compiled on host; CI's `eap` jobs compile it).

- [ ] **Step 5: Commit**

```bash
git add src/profile_source.rs src/lib.rs Cargo.toml Cargo.lock manifest.json
git commit -m "Read camera stream profiles via VAPIX service account" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 4: Registry — resolution and on-demand start

**Files:**
- Create: `src/registry.rs`
- Modify: `src/lib.rs` (`pub mod registry;`)

**Interfaces:**
- Consumes: `Config`, `MAIN_NAME` (Task 1); `CaptureSettings`, `parse_profile` (Task 2); `ProfileSource`, `CameraProfile` (Task 3); `crate::admin::StatusHandle` (existing); `multimux::{RouteHandle, origin::{AppState, router}, output::OutputKind}`.
- Produces:
  - `pub trait CaptureHandle: Send + Sync {}` — dropping the handle stops the capture.
  - `pub trait CaptureFactory: Send + Sync + 'static { fn start(&self, settings: CaptureSettings, route: Arc<RouteHandle>, status: StatusHandle, window_segments: usize, label: String) -> Box<dyn CaptureHandle>; }`
  - `pub const INNER_STREAM: &str = "s";` — every per-capture router serves under `/s/…`.
  - `pub enum ServeError { NotFound, ProfileUnavailable(String), ProfileMissing(String), Unsupported(String), EncoderBusy { in_use: u32, max: u32 } }`
  - `pub struct Registry` with:
    - `pub fn new(config: Config, factory: Arc<dyn CaptureFactory>, profiles: Arc<dyn ProfileSource>) -> Self`
    - `pub async fn ensure(&self, name: &str, now: Instant) -> Result<axum::Router, ServeError>`
    - `pub fn default_name(&self) -> String`
    - `pub fn config(&self) -> Config`
    - `pub fn profiles(&self) -> Arc<dyn ProfileSource>`

- [ ] **Step 1: Write failing tests** (bottom of `src/registry.rs`)

```rust
#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::StreamMapping;
    use crate::profile_source::{CameraProfile, StaticProfileSource};

    #[derive(Default)]
    pub(crate) struct Counts {
        pub started: AtomicUsize,
        pub stopped: AtomicUsize,
    }
    pub(crate) struct FakeFactory(pub Arc<Counts>);
    struct FakeHandle(Arc<Counts>);
    impl CaptureHandle for FakeHandle {}
    impl Drop for FakeHandle {
        fn drop(&mut self) {
            self.0.stopped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl CaptureFactory for FakeFactory {
        fn start(&self, _: CaptureSettings, _: Arc<RouteHandle>, _: StatusHandle, _: usize, _: String) -> Box<dyn CaptureHandle> {
            self.0.started.fetch_add(1, Ordering::SeqCst);
            Box::new(FakeHandle(self.0.clone()))
        }
    }

    pub(crate) fn profile(name: &str, params: &str) -> CameraProfile {
        CameraProfile { name: name.into(), description: String::new(), parameters: params.into() }
    }

    pub(crate) fn setup(streams: &[(&str, &str)]) -> (Arc<Registry>, Arc<Counts>, Arc<StaticProfileSource>) {
        let counts = Arc::new(Counts::default());
        let src = Arc::new(StaticProfileSource::new(vec![
            profile("ACC_High", "resolution=3840x2160&fps=25&videocodec=h264"),
            profile("ACC_Medium", "resolution=1280x720&fps=25&videocodec=h264"),
            profile("ACC_Low", "resolution=640x360&fps=5&videocodec=h264"),
            profile("MJPEG", "videocodec=jpeg"),
        ]));
        let cfg = Config {
            streams: streams
                .iter()
                .map(|(n, p)| StreamMapping { name: n.to_string(), profile: p.to_string() })
                .collect(),
            ..Config::default()
        };
        let reg = Arc::new(Registry::new(cfg, Arc::new(FakeFactory(counts.clone())), src.clone()));
        (reg, counts, src)
    }

    #[tokio::test]
    async fn unknown_name_is_not_found_and_starts_nothing() {
        let (reg, counts, _) = setup(&[("medium", "ACC_Medium")]);
        assert!(matches!(reg.ensure("nope", Instant::now()).await, Err(ServeError::NotFound)));
        assert_eq!(counts.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn first_request_starts_once_then_reuses() {
        let (reg, counts, _) = setup(&[("medium", "ACC_Medium")]);
        let now = Instant::now();
        reg.ensure("medium", now).await.unwrap();
        reg.ensure("medium", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn main_is_always_available() {
        let (reg, counts, _) = setup(&[]);
        reg.ensure("main", Instant::now()).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn names_with_same_settings_share_one_capture() {
        let (reg, counts, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let now = Instant::now();
        reg.ensure("a", now).await.unwrap();
        reg.ensure("b", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_first_requests_start_exactly_one_capture() {
        let (reg, counts, _) = setup(&[("medium", "ACC_Medium")]);
        let now = Instant::now();
        let futs = (0..8).map(|_| {
            let reg = reg.clone();
            tokio::spawn(async move { reg.ensure("medium", now).await.map(|_| ()) })
        });
        for f in futs {
            f.await.unwrap().unwrap();
        }
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn encode_cap_rejects_third_distinct_capture() {
        let (reg, counts, _) = setup(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        reg.ensure("hi", now).await.unwrap();
        reg.ensure("med", now).await.unwrap();
        match reg.ensure("lo", now).await {
            Err(ServeError::EncoderBusy { in_use: 2, max: 2 }) => {}
            other => panic!("expected EncoderBusy, got {:?}", other.map(|_| ())),
        }
        assert_eq!(counts.started.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn profile_errors_are_reported_per_name() {
        let (reg, _, src) = setup(&[("gone", "Deleted"), ("mj", "MJPEG"), ("med", "ACC_Medium")]);
        let now = Instant::now();
        assert!(matches!(reg.ensure("gone", now).await, Err(ServeError::ProfileMissing(p)) if p == "Deleted"));
        assert!(matches!(reg.ensure("mj", now).await, Err(ServeError::Unsupported(_))));
        reg.ensure("med", now).await.unwrap();
        src.set(Err("VAPIX down".into()));
        // a running capture keeps serving without asking VAPIX again
        reg.ensure("med", now).await.unwrap();
        let (reg2, _, src2) = setup(&[("x", "ACC_Low")]);
        src2.set(Err("VAPIX down".into()));
        assert!(matches!(reg2.ensure("x", now).await, Err(ServeError::ProfileUnavailable(m)) if m == "VAPIX down"));
        reg2.ensure("main", now).await.unwrap();
    }

    #[tokio::test]
    async fn default_name_follows_config() {
        let (reg, _, _) = setup(&[("medium", "ACC_Medium")]);
        assert_eq!(reg.default_name(), "main");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib registry::`
Expected: compile error.

- [ ] **Step 3: Implement** (above the tests)

```rust
//! Maps stream names to running captures. Captures start on the first
//! request for a name, are shared by every name that resolves to the same
//! [`CaptureSettings`], count against `max_encodes`, and stop when idle
//! (Task 5) or when a config change unmaps them (Task 5).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use multimux::RouteHandle;
use multimux::origin::{AppState, router};
use multimux::output::OutputKind;

use crate::admin::StatusHandle;
use crate::config::{Config, MAIN_NAME};
use crate::profile::{CaptureSettings, parse_profile};
use crate::profile_source::ProfileSource;

/// Stream name every per-capture router serves under.
pub const INNER_STREAM: &str = "s";

/// A running capture; dropping it stops the capture and frees its encode.
pub trait CaptureHandle: Send + Sync {}

/// Starts captures. The device implementation runs VDO (binary); tests use
/// a fake.
pub trait CaptureFactory: Send + Sync + 'static {
    fn start(
        &self,
        settings: CaptureSettings,
        route: Arc<RouteHandle>,
        status: StatusHandle,
        window_segments: usize,
        label: String,
    ) -> Box<dyn CaptureHandle>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServeError {
    NotFound,
    ProfileUnavailable(String),
    ProfileMissing(String),
    Unsupported(String),
    EncoderBusy { in_use: u32, max: u32 },
}

/// What a name points at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Main,
    Profile(String),
}

pub(crate) struct Capture {
    pub(crate) settings: CaptureSettings,
    pub(crate) names: BTreeSet<String>,
    pub(crate) router: axum::Router,
    pub(crate) status: StatusHandle,
    pub(crate) started_at: Instant,
    pub(crate) last_used: Instant,
    _handle: Box<dyn CaptureHandle>,
}

pub(crate) struct Inner {
    pub(crate) config: Config,
    pub(crate) captures: HashMap<CaptureSettings, Capture>,
}

pub struct Registry {
    pub(crate) inner: Mutex<Inner>,
    factory: Arc<dyn CaptureFactory>,
    profiles: Arc<dyn ProfileSource>,
}

fn target_of(config: &Config, name: &str) -> Option<Target> {
    if name == MAIN_NAME {
        return Some(Target::Main);
    }
    config
        .streams
        .iter()
        .find(|s| s.name == name)
        .map(|s| Target::Profile(s.profile.clone()))
}

impl Registry {
    pub fn new(config: Config, factory: Arc<dyn CaptureFactory>, profiles: Arc<dyn ProfileSource>) -> Self {
        Registry { inner: Mutex::new(Inner { config, captures: HashMap::new() }), factory, profiles }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("registry mutex poisoned")
    }

    pub fn config(&self) -> Config {
        self.lock().config.clone()
    }

    pub fn profiles(&self) -> Arc<dyn ProfileSource> {
        self.profiles.clone()
    }

    /// Name the bare `/hls/media.m3u8` serves.
    pub fn default_name(&self) -> String {
        self.lock().config.default_stream.clone().unwrap_or_else(|| MAIN_NAME.to_string())
    }

    /// The router serving `name`, starting its capture if needed.
    pub async fn ensure(&self, name: &str, now: Instant) -> Result<axum::Router, ServeError> {
        // Fast path: a capture already serves this name.
        let (target, config) = {
            let mut inner = self.lock();
            let target = target_of(&inner.config, name).ok_or(ServeError::NotFound)?;
            if let Some(c) = inner.captures.values_mut().find(|c| c.names.contains(name)) {
                c.last_used = now;
                return Ok(c.router.clone());
            }
            (target, inner.config.clone())
        };

        // Resolve settings without holding the lock (VAPIX is async).
        let settings = match &target {
            Target::Main => CaptureSettings::from_main(&config.main).map_err(ServeError::Unsupported)?,
            Target::Profile(p) => {
                let list = self.profiles.list().await.map_err(ServeError::ProfileUnavailable)?;
                let cam = list
                    .iter()
                    .find(|c| &c.name == p)
                    .ok_or_else(|| ServeError::ProfileMissing(p.clone()))?;
                parse_profile(&cam.parameters, &config.main).map_err(ServeError::Unsupported)?.settings
            }
        };

        let mut inner = self.lock();
        // The mapping may have changed while we were resolving.
        if target_of(&inner.config, name).as_ref() != Some(&target) {
            return Err(ServeError::NotFound);
        }
        if let Some(c) = inner.captures.get_mut(&settings) {
            c.names.insert(name.to_string());
            c.last_used = now;
            return Ok(c.router.clone());
        }
        let max = inner.config.max_encodes;
        let in_use = inner.captures.len() as u32;
        if in_use >= max {
            return Err(ServeError::EncoderBusy { in_use, max });
        }
        let cfg = &inner.config;
        let route = Arc::new(RouteHandle::new(cfg.target_duration_secs, cfg.part_target_ms, cfg.window_segments));
        let mut streams = HashMap::new();
        streams.insert(INNER_STREAM.to_string(), (route.clone(), vec![OutputKind::LlHls.build()]));
        let router = router(Arc::new(AppState::new(streams)));
        let status = StatusHandle::new();
        let handle = self.factory.start(settings, route, status.clone(), cfg.window_segments, name.to_string());
        let mut names = BTreeSet::new();
        names.insert(name.to_string());
        inner.captures.insert(
            settings,
            Capture { settings, names, router: router.clone(), status, started_at: now, last_used: now, _handle: handle },
        );
        Ok(router)
    }
}
```

Note: `Box<dyn CaptureHandle>` must be `Send + Sync` for `Registry: Sync`; the trait bound above guarantees it. If `axum::Router` is not `Sync` in 0.7, wrap stored routers in `Arc<Mutex<…>>` is NOT needed — `Router<()>` is `Send + Sync + Clone`; confirm by compiling.

- [ ] **Step 4: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/registry.rs src/lib.rs
git commit -m "Registry: on-demand shared captures with encode cap" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 5: Registry — idle sweep, live apply, status

**Files:**
- Modify: `src/registry.rs`

**Interfaces:**
- Consumes: Task 4 types.
- Produces (on `Registry`):
  - `pub fn sweep(&self, now: Instant)` — drops captures with `now - last_used >= idle_timeout_secs`.
  - `pub fn apply(&self, new: Config)` — swaps config; stops captures whose names were all unmapped/remapped, or all captures when LL-HLS tuning or `main` changed for main-based captures.
  - `pub fn spawn_sweeper(self: &Arc<Self>)` — tokio task calling `sweep` every 5 s.
  - `pub fn snapshot(&self, now: Instant) -> RegistryStatus`
  - `#[derive(Serialize, ToSchema)] pub struct RegistryStatus { pub encodes: EncodeUsage, pub streams: Vec<StreamStatus> }`
  - `#[derive(Serialize, ToSchema)] pub struct EncodeUsage { pub in_use: u32, pub max: u32 }`
  - `#[derive(Serialize, ToSchema)] pub struct StreamStatus { pub names: Vec<String>, pub settings: String, pub state: String, pub running: bool, pub current_segment: u32, pub current_part: u32, pub frames: u64, pub fps: f64, pub idle_secs: u64, pub last_error: Option<String> }` — `state` is `"starting"` (no frames yet), `"running"`, or `"error"` (`last_error` set and not running).

- [ ] **Step 1: Write failing tests** (append inside `registry::tests`)

```rust
    use std::time::Duration;

    #[tokio::test]
    async fn idle_capture_stops_after_timeout_and_frees_encode() {
        let (reg, counts, _) = setup(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let t0 = Instant::now();
        reg.ensure("hi", t0).await.unwrap();
        reg.ensure("med", t0 + Duration::from_secs(20)).await.unwrap();
        reg.sweep(t0 + Duration::from_secs(29));
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        reg.sweep(t0 + Duration::from_secs(30));
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1); // "hi" idle 30 s
        reg.ensure("lo", t0 + Duration::from_secs(31)).await.unwrap(); // slot freed
    }

    #[tokio::test]
    async fn every_request_keeps_a_capture_alive() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let t0 = Instant::now();
        for s in [0, 20, 40, 60] {
            reg.ensure("med", t0 + Duration::from_secs(s)).await.unwrap();
            reg.sweep(t0 + Duration::from_secs(s + 1));
        }
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn apply_keeps_unchanged_streams_running() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        reg.ensure("med", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams.retain(|s| s.name != "lo");
        cfg.default_stream = Some("med".into());
        cfg.max_encodes = 3;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        reg.ensure("med", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
        assert_eq!(reg.default_name(), "med");
    }

    #[tokio::test]
    async fn apply_stops_remapped_and_removed_streams() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        reg.ensure("med", now).await.unwrap();
        reg.ensure("lo", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams = vec![StreamMapping { name: "med".into(), profile: "ACC_High".into() }];
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 2);
        assert!(matches!(reg.ensure("lo", now).await, Err(ServeError::NotFound)));
        reg.ensure("med", now).await.unwrap(); // restarts with ACC_High
        assert_eq!(counts.started.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn shared_capture_survives_while_one_name_still_maps_to_it() {
        let (reg, counts, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let now = Instant::now();
        reg.ensure("a", now).await.unwrap();
        reg.ensure("b", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams.retain(|s| s.name != "b");
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn apply_llhls_change_restarts_everything() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let now = Instant::now();
        reg.ensure("med", now).await.unwrap();
        reg.ensure("main", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.part_target_ms = 250;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn apply_main_change_restarts_only_main() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let now = Instant::now();
        reg.ensure("med", now).await.unwrap();
        reg.ensure("main", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.main.framerate = 15;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn snapshot_reports_encodes_and_streams() {
        let (reg, _, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let t0 = Instant::now();
        reg.ensure("a", t0).await.unwrap();
        reg.ensure("b", t0).await.unwrap();
        let s = reg.snapshot(t0 + Duration::from_secs(3));
        assert_eq!((s.encodes.in_use, s.encodes.max), (1, 2));
        assert_eq!(s.streams.len(), 1);
        assert_eq!(s.streams[0].names, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(s.streams[0].settings, "h264 1280x720@25 ch0");
        assert_eq!(s.streams[0].state, "starting");
        assert_eq!(s.streams[0].idle_secs, 3);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib registry::`
Expected: compile errors (`sweep`, `apply`, `snapshot` missing).

- [ ] **Step 3: Implement** (add to `impl Registry` and the module)

```rust
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EncodeUsage {
    pub in_use: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StreamStatus {
    /// Names currently served by this capture.
    pub names: Vec<String>,
    /// e.g. `h264 1280x720@25 ch1`.
    pub settings: String,
    /// `starting`, `running` or `error`.
    pub state: String,
    pub running: bool,
    pub current_segment: u32,
    pub current_part: u32,
    pub frames: u64,
    /// Average fps since the capture started.
    pub fps: f64,
    /// Seconds since the last request.
    pub idle_secs: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RegistryStatus {
    pub encodes: EncodeUsage,
    pub streams: Vec<StreamStatus>,
}

impl Registry {
    /// Stop captures nobody has requested for `idle_timeout_secs`.
    pub fn sweep(&self, now: Instant) {
        let mut inner = self.lock();
        let timeout = std::time::Duration::from_secs(inner.config.idle_timeout_secs);
        inner.captures.retain(|_, c| now.saturating_duration_since(c.last_used) < timeout);
    }

    pub fn spawn_sweeper(self: &Arc<Self>) {
        let reg = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                reg.sweep(Instant::now());
            }
        });
    }

    /// Swap in `new` (already validated). Captures keep running unless what
    /// they serve changed.
    pub fn apply(&self, new: Config) {
        let mut inner = self.lock();
        let old = std::mem::replace(&mut inner.config, new);
        let llhls_changed = !old.llhls_eq(&inner.config);
        let main_changed = old.main != inner.config.main;
        let config = inner.config.clone();
        inner.captures.retain(|_, c| {
            if llhls_changed {
                return false;
            }
            c.names.retain(|n| {
                let before = target_of(&old, n);
                let after = target_of(&config, n);
                before == after && !(after == Some(Target::Main) && main_changed)
            });
            !c.names.is_empty()
        });
    }

    pub fn snapshot(&self, now: Instant) -> RegistryStatus {
        let inner = self.lock();
        let mut streams: Vec<StreamStatus> = inner
            .captures
            .values()
            .map(|c| {
                let st = c.status.snapshot();
                let secs = now.saturating_duration_since(c.started_at).as_secs_f64();
                let state = if st.running && st.frames > 0 {
                    "running"
                } else if !st.running && st.last_error.is_some() {
                    "error"
                } else {
                    "starting"
                };
                StreamStatus {
                    names: c.names.iter().cloned().collect(),
                    settings: c.settings.describe(),
                    state: state.to_string(),
                    running: st.running,
                    current_segment: st.current_segment,
                    current_part: st.current_part,
                    frames: st.frames,
                    fps: if secs > 0.0 { ((st.frames as f64 / secs) * 10.0).round() / 10.0 } else { 0.0 },
                    idle_secs: now.saturating_duration_since(c.last_used).as_secs(),
                    last_error: st.last_error,
                }
            })
            .collect();
        streams.sort_by(|a, b| a.names.cmp(&b.names));
        RegistryStatus {
            encodes: EncodeUsage { in_use: inner.captures.len() as u32, max: inner.config.max_encodes },
            streams,
        }
    }
}
```

`Target` needs `Clone, PartialEq, Eq` (already derived in Task 4).

- [ ] **Step 4: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/registry.rs
git commit -m "Registry: idle sweep, live config apply, status snapshot" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 6: HLS front router

**Files:**
- Create: `src/routing.rs`
- Modify: `src/lib.rs` (`pub mod routing;`)

**Interfaces:**
- Consumes: `Registry::{ensure, default_name}`, `ServeError`, `INNER_STREAM`, `registry::tests::setup` (tests).
- Produces: `pub fn hls_router(registry: Arc<Registry>) -> axum::Router` — mount with `.nest("/hls", hls_router(reg))`.

- [ ] **Step 1: Write failing tests** (bottom of `src/routing.rs`)

```rust
#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;
    use crate::registry::tests::setup;

    async fn get(app: &axum::Router, uri: &str) -> axum::response::Response {
        app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap()
    }

    fn app(streams: &[(&str, &str)]) -> (axum::Router, Arc<Registry>) {
        let (reg, _, _) = setup(streams);
        (axum::Router::new().nest("/hls", hls_router(reg.clone())), reg)
    }

    #[tokio::test]
    async fn unknown_name_404() {
        let (app, _) = app(&[]);
        assert_eq!(get(&app, "/hls/nope/media.m3u8").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mapped_name_reaches_multimux_router() {
        // Empty stream: multimux answers 503 (no segments yet), proving the forward.
        let (app, _) = app(&[("medium", "ACC_Medium")]);
        assert_eq!(get(&app, "/hls/medium/media.m3u8").await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn bare_url_redirects_to_default_keeping_query() {
        let (app, reg) = app(&[("medium", "ACC_Medium")]);
        let r = get(&app, "/hls/media.m3u8?_HLS_msn=12&_HLS_part=3").await;
        assert_eq!(r.status(), StatusCode::FOUND);
        assert_eq!(r.headers()[header::LOCATION], "main/media.m3u8?_HLS_msn=12&_HLS_part=3");
        let mut cfg = reg.config();
        cfg.default_stream = Some("medium".into());
        reg.apply(cfg);
        let r = get(&app, "/hls/media.m3u8").await;
        assert_eq!(r.headers()[header::LOCATION], "medium/media.m3u8");
    }

    #[test]
    fn forward_uri_keeps_query_and_rewrites_path() {
        assert_eq!(forward_uri("media.m3u8", Some("_HLS_msn=12&_HLS_part=3")), "/s/media.m3u8?_HLS_msn=12&_HLS_part=3");
        assert_eq!(forward_uri("/seg-1-2.m4s", None), "/s/seg-1-2.m4s");
    }

    #[tokio::test]
    async fn encoder_busy_is_503_with_retry_after_and_reason() {
        let (app, _) = app(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        get(&app, "/hls/hi/media.m3u8").await;
        get(&app, "/hls/med/media.m3u8").await;
        let r = get(&app, "/hls/lo/media.m3u8").await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()[header::RETRY_AFTER], "5");
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"encoder busy (2/2 encodes in use)");
    }

    #[tokio::test]
    async fn missing_profile_is_503_with_reason() {
        let (app, _) = app(&[("gone", "Deleted")]);
        let r = get(&app, "/hls/gone/media.m3u8").await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"camera profile \"Deleted\" not found");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib routing::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! `/hls` front router: resolves a stream name through the [`Registry`]
//! (starting its capture on demand) and forwards the request to that
//! capture's multimux origin router, which serves it under `/s/…`.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use tower::ServiceExt;

use crate::registry::{INNER_STREAM, Registry, ServeError};

pub fn hls_router(registry: Arc<Registry>) -> Router {
    Router::new()
        .route("/media.m3u8", get(bare))
        .route("/{name}/{*rest}", any(forward))
        .with_state(registry)
}

/// Bare URL → relative redirect to the default stream, keeping the query
/// (LL-HLS `_HLS_msn`/`_HLS_part`). Relative, so it works under the
/// `/local/multimuxedge/hls/` proxy prefix.
async fn bare(State(reg): State<Arc<Registry>>, req: Request) -> Response {
    let mut location = format!("{}/media.m3u8", reg.default_name());
    if let Some(q) = req.uri().query() {
        location.push('?');
        location.push_str(q);
    }
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

pub(crate) fn forward_uri(rest: &str, query: Option<&str>) -> String {
    let mut uri = format!("/{INNER_STREAM}/{}", rest.trim_start_matches('/'));
    if let Some(q) = query {
        uri.push('?');
        uri.push_str(q);
    }
    uri
}

async fn forward(
    State(reg): State<Arc<Registry>>,
    Path((name, rest)): Path<(String, String)>,
    mut req: Request,
) -> Response {
    let router = match reg.ensure(&name, Instant::now()).await {
        Ok(r) => r,
        Err(e) => return serve_error(e),
    };
    let uri = forward_uri(&rest, req.uri().query());
    match uri.parse() {
        Ok(u) => *req.uri_mut() = u,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    }
    match router.oneshot(req).await {
        Ok(resp) => resp,
        Err(never) => match never {},
    }
}

fn serve_error(e: ServeError) -> Response {
    let unavailable = |msg: String| {
        (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "5")], msg).into_response()
    };
    match e {
        ServeError::NotFound => StatusCode::NOT_FOUND.into_response(),
        ServeError::EncoderBusy { in_use, max } => {
            unavailable(format!("encoder busy ({in_use}/{max} encodes in use)"))
        }
        ServeError::ProfileMissing(p) => unavailable(format!("camera profile \"{p}\" not found")),
        ServeError::ProfileUnavailable(m) => unavailable(format!("profile source unavailable: {m}")),
        ServeError::Unsupported(m) => unavailable(m),
    }
}
```

Move `tower` from `[dev-dependencies]` to `[dependencies]` in `Cargo.toml` (`tower = { version = "0.5", features = ["util"] }`) — `oneshot` is now used in non-test code.

- [ ] **Step 4: Run tests**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS. If `mapped_name_reaches_multimux_router` returns a status other than 503, read `multimux-0.10.0/src/origin/` to find what an empty LL-HLS stream answers and assert that (the point is "not 404").

- [ ] **Step 5: Commit**

```bash
git add src/routing.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "HLS front router: per-name forwarding, default redirect, clear 503s" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 7: Admin API — live apply, profiles, status, OpenAPI

**Files:**
- Modify: `src/admin.rs`
- Create: `src/openapi.rs`, `tests/openapi_snapshot.rs`, `docs/src/api/openapi.json` (generated)
- Modify: `src/lib.rs` (`pub mod openapi;`)

**Interfaces:**
- Consumes: `Registry::{apply, snapshot, profiles, config}`, `parse_profile`, `CameraProfile`, `RegistryStatus`, `FieldError`.
- Produces:
  - `pub fn admin_router<S: ConfigStore>(store: Arc<S>, app_status: StatusHandle, registry: Arc<Registry>) -> Router`
  - `#[derive(Serialize, ToSchema)] pub struct AdminStatus { pub last_error: Option<String>, #[serde(flatten)] pub registry: RegistryStatus }`
  - `#[derive(Serialize, ToSchema)] pub struct ProfileView { pub name: String, pub description: String, pub parameters: String, pub settings: Option<String>, pub ignored_keys: Vec<String>, pub error: Option<String> }`
  - `#[derive(Serialize, ToSchema)] pub struct ProfilesResponse { pub profiles: Vec<ProfileView>, pub error: Option<String> }`
  - `#[derive(Serialize, ToSchema)] pub struct ValidationErrors { pub errors: Vec<FieldError> }`
  - `#[derive(Serialize, ToSchema)] pub struct Applied { pub status: String }` (`"applied"`)
  - `openapi::ApiDoc` (`#[derive(utoipa::OpenApi)]`) and `pub fn openapi_json() -> String`

- [ ] **Step 1: Write failing tests** (replace the `router()` helper and add tests in `admin::tests`)

```rust
    use crate::registry::tests::setup;

    fn router_with(streams: &[(&str, &str)]) -> (Router, Arc<crate::registry::Registry>) {
        let (reg, _, _) = setup(streams);
        (admin_router(Arc::new(DefaultStore), StatusHandle::new(), reg.clone()), reg)
    }

    fn router() -> Router {
        router_with(&[]).0
    }

    async fn post_json(app: Router, uri: &str, v: &serde_json::Value) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(v).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn post_config_applies_live() {
        let (app, reg) = router_with(&[]);
        let mut cfg = Config::default();
        cfg.streams = vec![crate::config::StreamMapping { name: "med".into(), profile: "ACC_Medium".into() }];
        cfg.default_stream = Some("med".into());
        let r = post_json(app, "/admin/config", &serde_json::to_value(&cfg).unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(body_json(r).await["status"], "applied");
        assert_eq!(reg.default_name(), "med");
    }

    #[tokio::test]
    async fn post_config_invalid_returns_field_errors_and_applies_nothing() {
        let (app, reg) = router_with(&[]);
        let mut cfg = Config::default();
        cfg.streams = vec![crate::config::StreamMapping { name: "Bad Name".into(), profile: "P".into() }];
        let r = post_json(app, "/admin/config", &serde_json::to_value(&cfg).unwrap()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(r).await["errors"][0]["field"], "streams[0].name");
        assert!(reg.config().streams.is_empty());
    }

    #[tokio::test]
    async fn profiles_lists_camera_profiles_with_parsed_settings() {
        let app = router();
        let r = app
            .oneshot(Request::builder().uri("/admin/profiles").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        let med = v["profiles"].as_array().unwrap().iter().find(|p| p["name"] == "ACC_Medium").unwrap();
        assert_eq!(med["settings"], "h264 1280x720@25 ch0");
        let mj = v["profiles"].as_array().unwrap().iter().find(|p| p["name"] == "MJPEG").unwrap();
        assert!(mj["error"].as_str().unwrap().contains("jpeg"));
        assert!(v["error"].is_null());
    }

    #[tokio::test]
    async fn status_has_encodes_and_streams() {
        let (app, reg) = router_with(&[("med", "ACC_Medium")]);
        reg.ensure("med", std::time::Instant::now()).await.unwrap();
        let r = app
            .oneshot(Request::builder().uri("/admin/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = body_json(r).await;
        assert_eq!(v["encodes"]["in_use"], 1);
        assert_eq!(v["streams"][0]["names"][0], "med");
        assert!(v["last_error"].is_null());
    }

    #[tokio::test]
    async fn openapi_json_is_served() {
        let r = router()
            .oneshot(Request::builder().uri("/admin/openapi.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        for p in ["/admin/config", "/admin/status", "/admin/profiles"] {
            assert!(v["paths"][p].is_object(), "missing {p}");
        }
    }
```

Delete the old `get_status_returns_expected_fields` test (its fields no longer exist) and the old `post_config_valid_returns_200` assertion on `"note"`. Update `get_config_returns_defaults` unchanged (still `Config::default()`).

`tests/openapi_snapshot.rs`:
```rust
//! The committed API description must match the code. Regenerate with
//! `UPDATE_OPENAPI=1 cargo test --test openapi_snapshot`.

#[test]
fn openapi_snapshot_matches() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/api/openapi.json");
    let fresh = multimux_edge::openapi::openapi_json();
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &fresh).unwrap();
    }
    let committed = std::fs::read_to_string(path).expect("run with UPDATE_OPENAPI=1 once");
    assert_eq!(committed, fresh, "openapi.json is stale: rerun with UPDATE_OPENAPI=1");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --locked`
Expected: compile errors (`admin_router` arity, `openapi` module).

- [ ] **Step 3: Implement the admin changes**

In `src/admin.rs`:

1. Delete the `Status` struct's use as the response type (keep `Status` and `StatusHandle` — the registry and capture workers use them per capture).
2. `AdminState` gains `registry: Arc<Registry>`; its manual `Clone` clones it too. Rename the `status` field to `app_status` (it now only carries the config-backend error).
3. Router:

```rust
pub fn admin_router<S: ConfigStore>(store: Arc<S>, app_status: StatusHandle, registry: Arc<Registry>) -> Router {
    let state = AdminState { store, app_status, registry };
    Router::new()
        .route("/admin/config", get(get_config::<S>).post(post_config::<S>))
        .route("/admin/status", get(get_status::<S>))
        .route("/admin/profiles", get(get_profiles::<S>))
        .route("/admin/openapi.json", get(get_openapi))
        .with_state(state)
}
```

4. Handlers (annotated for utoipa; generic handlers get the `#[utoipa::path]` attribute exactly the same way):

```rust
/// Current configuration.
#[utoipa::path(get, path = "/admin/config", responses((status = 200, body = Config)))]
async fn get_config<S: ConfigStore>(State(state): State<AdminState<S>>) -> Json<Config> {
    let outcome = state.store.load();
    if let Some(reason) = outcome.error() {
        state.app_status.set_config_error(Some(format!("config load: {reason}")));
    }
    Json(outcome.into_config())
}

/// Validate, store and apply a configuration. Applies immediately.
#[utoipa::path(post, path = "/admin/config", request_body = Config, responses(
    (status = 200, body = Applied),
    (status = 400, body = ValidationErrors),
    (status = 500, description = "config store failed", body = String),
))]
async fn post_config<S: ConfigStore>(State(state): State<AdminState<S>>, Json(cfg): Json<Config>) -> Response {
    if let Err(errors) = cfg.validate() {
        return (StatusCode::BAD_REQUEST, Json(ValidationErrors { errors })).into_response();
    }
    if let Err(e) = state.store.store(&cfg) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    state.registry.apply(cfg);
    Json(Applied { status: "applied".into() }).into_response()
}

/// Encoder usage and every running capture.
#[utoipa::path(get, path = "/admin/status", responses((status = 200, body = AdminStatus)))]
async fn get_status<S: ConfigStore>(State(state): State<AdminState<S>>) -> Json<AdminStatus> {
    Json(AdminStatus {
        last_error: state.app_status.snapshot().last_error,
        registry: state.registry.snapshot(std::time::Instant::now()),
    })
}

/// The camera's stream profiles and what this app would capture for each.
#[utoipa::path(get, path = "/admin/profiles", responses((status = 200, body = ProfilesResponse)))]
async fn get_profiles<S: ConfigStore>(State(state): State<AdminState<S>>) -> Json<ProfilesResponse> {
    let main = state.registry.config().main;
    match state.registry.profiles().list().await {
        Ok(list) => Json(ProfilesResponse {
            profiles: list
                .into_iter()
                .map(|p| {
                    let parsed = crate::profile::parse_profile(&p.parameters, &main);
                    ProfileView {
                        settings: parsed.as_ref().ok().map(|x| x.settings.describe()),
                        ignored_keys: parsed.as_ref().map(|x| x.ignored_keys.clone()).unwrap_or_default(),
                        error: parsed.err(),
                        name: p.name,
                        description: p.description,
                        parameters: p.parameters,
                    }
                })
                .collect(),
            error: None,
        }),
        Err(e) => Json(ProfilesResponse { profiles: Vec::new(), error: Some(e) }),
    }
}

/// This API's OpenAPI 3 description.
#[utoipa::path(get, path = "/admin/openapi.json", responses((status = 200, description = "OpenAPI document")))]
async fn get_openapi() -> Response {
    ([(axum::http::header::CONTENT_TYPE, "application/json")], crate::openapi::openapi_json()).into_response()
}
```

(`use axum::response::Response;` and `use crate::registry::{Registry, RegistryStatus};` at the top.)

5. The first-run truncation fix (`parse_stored`, `ensure_parameter` add-then-set) already shipped in 0.2.0. Keep it; `parse_stored` must keep working with the v2 `Config` (its tests use `Config { main: MainPreset { codec: "h265".into(), ..Default::default() }, ..Config::default() }` after Task 1).

- [ ] **Step 4: Implement `src/openapi.rs`**

```rust
//! OpenAPI description of the admin API, generated from the handler and
//! type annotations. Served at `/admin/openapi.json`; a snapshot lives at
//! `docs/src/api/openapi.json` for the docs site.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "Multimux Edge admin API", description = "Configure stream mappings and inspect captures. All paths are under /local/multimuxedge and require the camera's admin access level."),
    servers((url = "/local/multimuxedge")),
    paths(
        crate::admin::get_config,
        crate::admin::post_config,
        crate::admin::get_status,
        crate::admin::get_profiles,
        crate::admin::get_openapi,
    ),
)]
pub struct ApiDoc;

pub fn openapi_json() -> String {
    let mut doc = ApiDoc::openapi();
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
    doc.to_pretty_json().expect("openapi serializes")
}
```

The handlers must be `pub(crate)` for the `paths(...)` references. If utoipa 6 does not pick up schemas from `body = …` automatically, add `components(schemas(Config, MainPreset, StreamMapping, FieldError, ValidationErrors, Applied, AdminStatus, RegistryStatus, EncodeUsage, StreamStatus, ProfilesResponse, ProfileView))` — check against https://docs.rs/utoipa/6 before deciding. `AdminStatus` uses `#[serde(flatten)]`; annotate the field with `#[schema(inline)]` if utoipa requires it for flatten.

- [ ] **Step 5: Generate the snapshot and run everything**

Run:
```bash
UPDATE_OPENAPI=1 cargo test --locked --test openapi_snapshot
cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check
```
Expected: snapshot written to `docs/src/api/openapi.json`; all tests PASS. Open the JSON and check every path and schema appears with descriptions.

- [ ] **Step 6: Commit**

```bash
git add src/admin.rs src/openapi.rs src/lib.rs tests/openapi_snapshot.rs docs/src/api/openapi.json
git commit -m "Admin API: live apply, profiles, per-stream status, OpenAPI" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 8: Binary wiring and VDO capture factory

**Files:**
- Modify: `src/vdo_source.rs` (`VdoIngestSession::new` signature), `src/bin/multimux-edge.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `VdoIngestSession::new(settings: &CaptureSettings) -> Result<Self>`; `VdoCaptureFactory` (binary-private).

This task only compiles inside the ACAP SDK (CI `eap` jobs). Host checks still must pass.

- [ ] **Step 1: `VdoIngestSession::new(&CaptureSettings)`**

In `src/vdo_source.rs` change the signature to:
```rust
    pub fn new(settings: &crate::profile::CaptureSettings) -> Result<Self> {
        let crate::profile::CaptureSettings { codec, channel, width, height, framerate, gop_length } = *settings;
```
and the GOP line to:
```rust
        let gop_length = gop_length.unwrap_or(if framerate > 0 { framerate } else { 30 });
```
Keep the existing comment explaining why a fixed GOP is forced.

- [ ] **Step 2: Rewrite the binary's wiring**

Replace `STREAM_NAME`, `spawn_capture_pipeline` and `supervise_driver_forever` with a factory, keep `run_vdo_capture` but give it the settings and a stop flag:

```rust
/// One VDO capture on its own OS thread + current-thread runtime (see the
/// module doc's "Threading" section). Dropping the handle stops it: the
/// capture loop checks `stop` after every frame and `supervise_driver`
/// is cancelled via its `CancellationToken`.
struct VdoCapture {
    stop: Arc<AtomicBool>,
    cancel: CancellationToken,
}

impl CaptureHandle for VdoCapture {}

impl Drop for VdoCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.cancel.cancel();
    }
}

struct VdoCaptureFactory;

impl CaptureFactory for VdoCaptureFactory {
    fn start(
        &self,
        settings: CaptureSettings,
        route: Arc<RouteHandle>,
        status: StatusHandle,
        window_segments: usize,
        label: String,
    ) -> Box<dyn CaptureHandle> {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let thread_cancel = cancel.clone();
        let thread_stop = stop.clone();
        info!("multimux-edge: starting capture {label}: {}", settings.describe());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime for a VDO capture");
            rt.block_on(multimux::supervise_driver(
                move |route_handle| {
                    let status = status.clone();
                    let stop = thread_stop.clone();
                    async move {
                        run_vdo_capture(&settings, window_segments, &status, &route_handle, &stop).await
                    }
                },
                route,
                Backoff::production_default(),
                label,
                thread_cancel,
            ));
        });
        Box::new(VdoCapture { stop, cancel })
    }
}
```

`run_vdo_capture(settings: &CaptureSettings, window_segments: usize, status: &StatusHandle, route_handle: &RouteHandle, stop: &AtomicBool)`: build the session with `VdoIngestSession::new(settings)`, and at the top of the `loop` add:
```rust
        if stop.load(Ordering::Relaxed) {
            info!("multimux-edge: capture stopped (idle or unmapped)");
            break;
        }
```
After the loop, when `stop` is set, return `Ok(())` without recording an error (the existing `match driver.into_health()` already returns `Ok(())` for non-failed states).

`main()` becomes:
```rust
    let cfg = outcome.into_config();
    info!("multimux-edge: loaded config: {cfg:?}");

    #[allow(clippy::arc_with_non_send_sync)]
    let registry = Arc::new(Registry::new(
        cfg,
        Arc::new(VdoCaptureFactory),
        Arc::new(VapixProfileSource::new()),
    ));
    registry.spawn_sweeper();

    let inner = axum::Router::new()
        .nest("/hls", routing::hls_router(registry.clone()))
        .merge(admin::admin_router(store, status, registry));
    let app = axum::Router::new().nest(URL_PREFIX, inner);

    let bind_addr = format!("127.0.0.1:{}", multimux_edge::config::APP_PORT);
```
(remove the now-unused `route_handle`, `outputs`, `AppState`, `HashMap` imports; add `use std::sync::atomic::{AtomicBool, Ordering};`, `use multimux_edge::profile::CaptureSettings;`, `use multimux_edge::profile_source::VapixProfileSource;`, `use multimux_edge::registry::{CaptureFactory, CaptureHandle, Registry};`, `use multimux_edge::routing;`). `CancellationToken` is already imported (`tokio_util::sync::CancellationToken`, used by multimux 0.11's `supervise_driver`). Drop the `allow` if clippy doesn't need it. Update the module doc's "Why `supervise_driver`/`advance_route`" section: one supervised capture per distinct settings, started by the registry.

- [ ] **Step 3: Host checks**

Run: `cargo test --locked && cargo clippy --locked -- -D warnings && cargo fmt --all --check`
Expected: PASS (binary not built on host).

- [ ] **Step 4: Commit and push; CI builds the device code**

```bash
git add src/vdo_source.rs src/bin/multimux-edge.rs
git commit -m "Binary: registry-driven on-demand VDO captures" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
git push -u origin stream-profiles
gh run watch "$(gh run list --branch stream-profiles --limit 1 --json databaseId -q '.[0].databaseId')" --exit-status
```
Expected: all four `eap` jobs and `host` pass. Fix compile errors in device code here (the `eap` logs show them) before moving on.

---

### Task 9: Admin UI and player picker

**Files:**
- Rewrite: `html/index.html`
- Create: `html/app.css`, `html/app.js`
- Modify: `html/player.html`

**Interfaces:**
- Consumes: `GET/POST admin/config`, `GET admin/status`, `GET admin/profiles` (relative URLs — the page is served at `/local/multimuxedge/index.html`).

Constraints: no inline `style=""`; no external URLs; works at 360 px wide; light/dark via CSS custom properties on `:root` and `@media (prefers-color-scheme: dark)`.

- [ ] **Step 1: `html/index.html`**

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Multimux Edge</title>
<link rel="stylesheet" href="app.css">
</head>
<body>
<header class="topbar">
  <span class="brand"><span class="brand-mark">M</span>Multimux <b>Edge</b></span>
  <a class="btn btn-ghost" href="player.html">&#9654; Player</a>
</header>
<div class="layout">
  <nav class="sidebar" aria-label="Sections">
    <a href="#streams" class="active">Streams</a>
    <a href="#main">Main preset</a>
    <a href="#encoder">Encoder</a>
    <a href="#llhls">LL-HLS</a>
    <a href="#status">Status</a>
    <a href="#about">About</a>
  </nav>
  <main>
    <section id="streams" class="panel">
      <h2>Streams</h2>
      <p class="hint">Map a URL name to one of the camera's stream profiles. Served at <code>hls/&lt;name&gt;/media.m3u8</code>.</p>
      <div id="profiles-error" class="banner banner-err" hidden></div>
      <table class="grid">
        <thead><tr><th>Name</th><th>Camera profile</th><th>Default</th><th>URL</th><th></th></tr></thead>
        <tbody id="stream-rows"></tbody>
      </table>
      <button id="add-stream" class="btn" type="button">+ Add stream</button>
      <label class="radio"><input type="radio" name="default" value="" id="default-main"> Default to <b>main</b> preset</label>
    </section>

    <section id="main" class="panel">
      <h2>Main preset</h2>
      <p class="hint">Served at <code>hls/main/media.m3u8</code>, and wherever no default stream is set.</p>
      <div class="fields">
        <label>Channel<input id="f-main-channel" type="number" min="0"></label>
        <label>Width<input id="f-main-width" type="number" min="1"></label>
        <label>Height<input id="f-main-height" type="number" min="1"></label>
        <label>Frame rate<input id="f-main-framerate" type="number" min="0"></label>
        <label>Codec<select id="f-main-codec"><option value="h264">H.264</option><option value="h265">H.265</option></select></label>
      </div>
    </section>

    <section id="encoder" class="panel">
      <h2>Encoder</h2>
      <p class="hint">The camera runs about 2 distinct encodes at full frame rate (ARTPEC-6); a third slows every stream. Other clients such as a VMS share the same encoder.</p>
      <div class="fields">
        <label>Max encodes<input id="f-max-encodes" type="number" min="1" max="8"></label>
        <label>Stop idle stream after (s)<input id="f-idle" type="number" min="5" max="600"></label>
      </div>
    </section>

    <section id="llhls" class="panel">
      <h2>LL-HLS</h2>
      <div class="fields">
        <label>Segment duration (s)<input id="f-target" type="number" min="0.1" step="0.1"></label>
        <label>Part duration (ms)<input id="f-part" type="number" min="1"></label>
        <label>Playlist window (segments)<input id="f-window" type="number" min="1"></label>
      </div>
      <p class="hint">Changing these restarts every running stream.</p>
    </section>

    <div class="savebar">
      <span id="save-note" class="note"></span>
      <button id="save" class="btn btn-primary" type="button">Save &amp; apply</button>
    </div>

    <section id="status" class="panel">
      <h2>Status <span id="encodes" class="pill"></span></h2>
      <div id="config-error" class="banner banner-err" hidden></div>
      <table class="grid">
        <thead><tr><th>Names</th><th>Capture</th><th>State</th><th>FPS</th><th>Idle</th><th>Last error</th></tr></thead>
        <tbody id="status-rows"></tbody>
      </table>
      <p id="status-empty" class="hint">No streams running. A stream starts when a player requests it.</p>
    </section>

    <section id="about" class="panel">
      <h2>About</h2>
      <p>Multimux Edge, on-camera LL-HLS built on <a href="https://github.com/fishloa/axis-multimux-edge">multimux</a>.</p>
      <p><a href="https://fishloa.github.io/axis-multimux-edge/">Documentation</a> · <a href="admin/openapi.json">API (OpenAPI)</a></p>
    </section>
  </main>
</div>
<template id="row-tpl">
  <tr>
    <td><input class="name" type="text" placeholder="medium" maxlength="32"><div class="err"></div></td>
    <td><select class="profile"></select><div class="chips"></div><div class="err"></div></td>
    <td><input class="default" type="radio" name="default"></td>
    <td><code class="url"></code> <button class="btn btn-ghost copy" type="button">Copy</button></td>
    <td><button class="btn btn-ghost remove" type="button" aria-label="Remove">&times;</button></td>
  </tr>
</template>
<script src="app.js"></script>
</body>
</html>
```

- [ ] **Step 2: `html/app.css`**

```css
:root {
  color-scheme: light dark;
  --bg: #f4f5f7; --panel: #ffffff; --fg: #17191c; --muted: #6a717c; --border: #dde1e6;
  --accent: #0e9f8a; --accent-fg: #ffffff; --err: #d93c3c; --ok: #1f9d55; --warn: #c27c0e;
  --radius: 8px; --sidebar: 200px;
  font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root { --bg: #0f1113; --panel: #181b1f; --fg: #e6e8eb; --muted: #9aa2ad; --border: #2a2f36;
          --accent: #2cc5ad; --accent-fg: #071412; --err: #f06c6c; --ok: #4ad38a; --warn: #f0b04a; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); }
a { color: var(--accent); }
code { font-size: .85em; background: var(--bg); padding: .1em .35em; border-radius: 4px; }
.topbar { display: flex; align-items: center; justify-content: space-between; padding: .6rem 1rem;
          background: var(--panel); border-bottom: 1px solid var(--border); position: sticky; top: 0; z-index: 2; }
.brand { font-size: 1.05rem; display: flex; align-items: center; gap: .5rem; }
.brand-mark { display: inline-grid; place-items: center; width: 1.6rem; height: 1.6rem; border-radius: 6px;
              background: var(--accent); color: var(--accent-fg); font-weight: 700; }
.layout { display: grid; grid-template-columns: var(--sidebar) 1fr; gap: 1rem; padding: 1rem; max-width: 1200px; margin: 0 auto; }
.sidebar { position: sticky; top: 4rem; align-self: start; display: flex; flex-direction: column; gap: .15rem; }
.sidebar a { padding: .45rem .7rem; border-radius: var(--radius); color: var(--fg); text-decoration: none; }
.sidebar a:hover, .sidebar a.active { background: var(--panel); color: var(--accent); }
main { display: flex; flex-direction: column; gap: 1rem; min-width: 0; }
.panel { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 1rem 1.2rem; }
.panel h2 { margin: 0 0 .5rem; font-size: 1.05rem; display: flex; align-items: center; gap: .6rem; }
.hint { color: var(--muted); font-size: .85rem; margin: .2rem 0 .8rem; }
.fields { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: .8rem; }
label { display: flex; flex-direction: column; gap: .25rem; font-size: .8rem; color: var(--muted); }
label.radio { flex-direction: row; align-items: center; margin-top: .8rem; color: var(--fg); }
input, select { font: inherit; color: var(--fg); background: var(--bg); border: 1px solid var(--border);
                border-radius: 6px; padding: .4rem .5rem; width: 100%; }
input[type=radio] { width: auto; }
input.invalid, select.invalid { border-color: var(--err); }
.err { color: var(--err); font-size: .75rem; min-height: 0; }
.grid { width: 100%; border-collapse: collapse; margin-bottom: .6rem; }
.grid th { text-align: left; font-size: .75rem; color: var(--muted); font-weight: 600; padding: .3rem .4rem; border-bottom: 1px solid var(--border); }
.grid td { padding: .45rem .4rem; border-bottom: 1px solid var(--border); vertical-align: top; }
.btn { font: inherit; border: 1px solid var(--border); background: var(--panel); color: var(--fg);
       border-radius: 6px; padding: .4rem .8rem; cursor: pointer; text-decoration: none; }
.btn-primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg); font-weight: 600; }
.btn-ghost { background: transparent; }
.btn:disabled { opacity: .6; cursor: default; }
.savebar { position: sticky; bottom: 0; display: flex; justify-content: flex-end; align-items: center; gap: 1rem;
           background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: .6rem 1rem; }
.note { font-size: .85rem; color: var(--muted); }
.note.ok { color: var(--ok); } .note.err { color: var(--err); }
.pill { font-size: .75rem; font-weight: 600; padding: .1rem .5rem; border-radius: 999px; background: var(--bg); color: var(--muted); }
.pill.full { color: var(--warn); }
.chips { display: flex; flex-wrap: wrap; gap: .25rem; margin-top: .3rem; }
.chip { font-size: .7rem; padding: .05rem .4rem; border-radius: 999px; border: 1px solid var(--border); color: var(--muted); }
.state-running { color: var(--ok); } .state-starting { color: var(--warn); } .state-error { color: var(--err); }
.banner { padding: .5rem .8rem; border-radius: 6px; margin-bottom: .6rem; font-size: .85rem; }
.banner-err { background: color-mix(in srgb, var(--err) 12%, transparent); color: var(--err); }
@media (max-width: 760px) {
  .layout { grid-template-columns: 1fr; }
  .sidebar { position: static; flex-direction: row; overflow-x: auto; }
  .grid thead { display: none; }
  .grid tr { display: grid; gap: .3rem; padding: .5rem 0; border-bottom: 1px solid var(--border); }
  .grid td { border: 0; padding: 0; }
}
```

- [ ] **Step 3: `html/app.js`**

```js
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const rows = $("stream-rows");
  const tpl = $("row-tpl");
  let profiles = [];
  let base = new URL("hls/", location.href);

  const num = (id) => Number($(id).value);

  function profileOptions(select, current) {
    select.replaceChildren();
    const blank = new Option("Choose a profile…", "");
    select.add(blank);
    const names = profiles.map((p) => p.name);
    if (current && !names.includes(current)) {
      select.add(new Option(current + " (missing on camera)", current));
    }
    for (const p of profiles) {
      const label = p.settings ? `${p.name} — ${p.settings}` : `${p.name} — ${p.error}`;
      const o = new Option(label, p.name);
      o.disabled = !p.settings;
      select.add(o);
    }
    select.value = current || "";
  }

  function chipsFor(row) {
    const p = profiles.find((x) => x.name === row.querySelector(".profile").value);
    const chips = row.querySelector(".chips");
    chips.replaceChildren();
    for (const k of (p && p.ignored_keys) || []) {
      const c = document.createElement("span");
      c.className = "chip";
      c.textContent = k + " ignored";
      chips.append(c);
    }
  }

  function updateUrl(row) {
    const name = row.querySelector(".name").value.trim();
    row.querySelector(".url").textContent = name ? new URL(name + "/media.m3u8", base).pathname : "";
  }

  function addRow(m, isDefault) {
    const row = tpl.content.firstElementChild.cloneNode(true);
    row.querySelector(".name").value = m.name;
    profileOptions(row.querySelector(".profile"), m.profile);
    row.querySelector(".default").checked = isDefault;
    row.querySelector(".name").addEventListener("input", () => updateUrl(row));
    row.querySelector(".profile").addEventListener("change", () => chipsFor(row));
    row.querySelector(".remove").addEventListener("click", () => row.remove());
    row.querySelector(".copy").addEventListener("click", () => {
      const path = row.querySelector(".url").textContent;
      if (path) navigator.clipboard.writeText(new URL(path, location.href).href);
    });
    updateUrl(row);
    chipsFor(row);
    rows.append(row);
  }

  function fill(cfg) {
    $("f-main-channel").value = cfg.main.channel;
    $("f-main-width").value = cfg.main.width;
    $("f-main-height").value = cfg.main.height;
    $("f-main-framerate").value = cfg.main.framerate;
    $("f-main-codec").value = cfg.main.codec;
    $("f-max-encodes").value = cfg.max_encodes;
    $("f-idle").value = cfg.idle_timeout_secs;
    $("f-target").value = cfg.target_duration_secs;
    $("f-part").value = cfg.part_target_ms;
    $("f-window").value = cfg.window_segments;
    rows.replaceChildren();
    for (const m of cfg.streams) addRow(m, cfg.default_stream === m.name);
    $("default-main").checked = !cfg.default_stream;
  }

  function read() {
    const streams = [];
    let def = null;
    for (const row of rows.children) {
      const name = row.querySelector(".name").value.trim();
      streams.push({ name, profile: row.querySelector(".profile").value });
      if (row.querySelector(".default").checked) def = name;
    }
    return {
      main: { channel: num("f-main-channel"), width: num("f-main-width"), height: num("f-main-height"),
              framerate: num("f-main-framerate"), codec: $("f-main-codec").value },
      streams, default_stream: def,
      max_encodes: num("f-max-encodes"), idle_timeout_secs: num("f-idle"),
      target_duration_secs: num("f-target"), part_target_ms: num("f-part"), window_segments: num("f-window"),
    };
  }

  const FIELD_IDS = { "main.channel": "f-main-channel", "main.width": "f-main-width", "main.height": "f-main-height",
    "main.framerate": "f-main-framerate", "main.codec": "f-main-codec", max_encodes: "f-max-encodes",
    idle_timeout_secs: "f-idle", target_duration_secs: "f-target", part_target_ms: "f-part", window_segments: "f-window" };

  function clearErrors() {
    document.querySelectorAll(".invalid").forEach((e) => e.classList.remove("invalid"));
    document.querySelectorAll(".err").forEach((e) => (e.textContent = ""));
  }

  function showErrors(errors) {
    for (const e of errors) {
      const m = /^streams\[(\d+)\]\.(name|profile)$/.exec(e.field);
      if (m) {
        const row = rows.children[Number(m[1])];
        const input = row.querySelector("." + m[2]);
        input.classList.add("invalid");
        input.parentElement.querySelector(".err").textContent = e.message;
      } else if (FIELD_IDS[e.field]) {
        $(FIELD_IDS[e.field]).classList.add("invalid");
      }
    }
  }

  function note(text, cls) {
    $("save-note").textContent = text;
    $("save-note").className = "note " + (cls || "");
  }

  async function save() {
    clearErrors();
    $("save").disabled = true;
    note("Applying…");
    try {
      const r = await fetch("admin/config", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(read()) });
      if (r.status === 400) {
        const body = await r.json();
        showErrors(body.errors);
        note(body.errors.map((e) => `${e.field}: ${e.message}`).join(" · "), "err");
      } else if (!r.ok) {
        note("Save failed: " + (await r.text()), "err");
      } else {
        note("Applied", "ok");
        loadStatus();
      }
    } catch (e) {
      note("Save failed: " + e, "err");
    } finally {
      $("save").disabled = false;
    }
  }

  function cell(text, cls) {
    const td = document.createElement("td");
    td.textContent = text;
    if (cls) td.className = cls;
    return td;
  }

  async function loadStatus() {
    try {
      const s = await (await fetch("admin/status")).json();
      const pill = $("encodes");
      pill.textContent = `${s.encodes.in_use}/${s.encodes.max} encodes`;
      pill.classList.toggle("full", s.encodes.in_use >= s.encodes.max);
      $("config-error").hidden = !s.last_error;
      $("config-error").textContent = s.last_error || "";
      const body = $("status-rows");
      body.replaceChildren();
      for (const st of s.streams) {
        const tr = document.createElement("tr");
        tr.append(cell(st.names.join(", ")), cell(st.settings), cell(st.state, "state-" + st.state),
          cell(String(st.fps)), cell(st.idle_secs + " s"), cell(st.last_error || "—"));
        body.append(tr);
      }
      $("status-empty").hidden = s.streams.length > 0;
    } catch (e) {
      $("encodes").textContent = "status unavailable";
    }
  }

  async function init() {
    try {
      const p = await (await fetch("admin/profiles")).json();
      profiles = p.profiles;
      $("profiles-error").hidden = !p.error;
      $("profiles-error").textContent = p.error ? "Camera profiles unavailable: " + p.error : "";
    } catch (e) {
      $("profiles-error").hidden = false;
      $("profiles-error").textContent = "Camera profiles unavailable: " + e;
    }
    try {
      fill(await (await fetch("admin/config")).json());
    } catch (e) {
      note("Failed to load config: " + e, "err");
    }
    loadStatus();
    setInterval(loadStatus, 5000);
  }

  $("add-stream").addEventListener("click", () => addRow({ name: "", profile: "" }, false));
  $("save").addEventListener("click", save);
  document.querySelectorAll(".sidebar a").forEach((a) =>
    a.addEventListener("click", () => {
      document.querySelectorAll(".sidebar a").forEach((x) => x.classList.remove("active"));
      a.classList.add("active");
    }));
  init();
})();
```

- [ ] **Step 4: Player stream picker**

In `html/player.html`: change `var SRC = "hls/cam/media.m3u8";` to read the stream from the URL hash (`player.html#medium`), defaulting to the bare URL:
```js
  var name = decodeURIComponent(location.hash.slice(1));
  var SRC = name ? "hls/" + name + "/media.m3u8" : "hls/media.m3u8";
```
and add, in the header next to the settings link, a `<select id="stream">` filled from `admin/config` (`default`, `main`, each mapping); on change set `location.hash` and reload. Use a class from the existing `<style>` block for the select (no inline style). In `index.html`, the stream table's URL cell also links to `player.html#<name>` — add `<a class="btn btn-ghost play" href="#">Play</a>` to the template and set `href` in `updateUrl`.

- [ ] **Step 5: Check**

Run: `grep -n 'style="' html/*.html` → no output. `cargo test --locked` still passes (HTML is not compiled). Visual check happens on the device in Task 11.

- [ ] **Step 6: Commit**

```bash
git add html/
git commit -m "Admin UI: Multimux Edge redesign with stream mappings, encoder and live status; player picker" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 10: Docs, API reference, changelog

**Files:**
- Modify: `docs/src/operator/configuration.md`, `docs/src/operator/troubleshooting.md`, `docs/src/SUMMARY.md`, `CHANGELOG.md`
- Create: `docs/src/api/reference.md`
- Modify: `.github/workflows/ci.yml` (docs job copies `docs/src/api/openapi.json` into the site — mdBook copies non-`.md` files from `src/` automatically; verify in the built `docs/book/api/` and only change CI if it is missing)

- [ ] **Step 1: `docs/src/api/reference.md`**

```markdown
# Admin API reference

The admin API lives under `https://<camera>/local/multimuxedge/admin/` and
requires the camera's **admin** access level. The description below is
generated from the code ([`openapi.json`](openapi.json)); the running app
serves the same document at `/local/multimuxedge/admin/openapi.json`.

<redoc spec-url="openapi.json"></redoc>
<script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"></script>
```
(The docs site is public GitHub Pages, so a CDN script is fine here; the camera UI never loads it.)

Add `- [Admin API reference](api/reference.md)` to `docs/src/SUMMARY.md`.

- [ ] **Step 2: `configuration.md`**: rewrite the config section for v2. Cover the config JSON example from the spec (without `port`), the name rules, `main`, the default stream and bare-URL redirect, `max_encodes` with the measured ARTPEC-6 guidance, `idle_timeout_secs`, which profile keys apply versus are ignored (table from the spec), and that every change applies live. Include curl examples:
```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/profiles
curl -u <user>:<pw> -X POST https://<cam>/local/multimuxedge/admin/config \
  -H 'content-type: application/json' \
  -d '{"main":{"channel":0,"width":1920,"height":1080,"framerate":30,"codec":"h264"},
       "streams":[{"name":"medium","profile":"ACC_Medium"}],"default_stream":"medium",
       "max_encodes":2,"idle_timeout_secs":30,
       "target_duration_secs":4.0,"part_target_ms":500,"window_segments":8}'
```

- [ ] **Step 3: `troubleshooting.md`**: add entries for each 503 body (`encoder busy (N/M encodes in use)`, `camera profile "X" not found`, `profile source unavailable: …`, unsupported codec), what each means and what to do, and that stream URLs are now `hls/<name>/media.m3u8` (the old `hls/cam/` path is gone).

- [ ] **Step 4: `CHANGELOG.md`** under `[Unreleased]`:

```markdown
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

### Changed

- **Breaking:** config v2. Capture fields move under `main`; older stored
  configs are migrated on load. `port` is removed (the app always listens
  on 2999, the manifest's reverse-proxy target).
- **Breaking:** the stream URL `hls/cam/media.m3u8` is now `hls/main/…`
  (or the bare `hls/media.m3u8`, which redirects to the default stream).
- **Breaking:** `GET /admin/status` now reports `encodes` and a `streams`
  list; the single-pipeline fields moved into each stream entry.
- Config changes apply immediately; no restart.
```

- [ ] **Step 5: Build docs and run tests**

Run: `mdbook build docs && ls docs/book/api/ && cargo test --locked`
Expected: `openapi.json` and `reference.html` present in `docs/book/api/`; tests PASS.

- [ ] **Step 6: Commit**

```bash
git add docs/ CHANGELOG.md .github/workflows/ci.yml
git commit -m "Docs: stream profiles configuration, API reference, changelog" -m "Claude-Session: https://claude.ai/code/session_01QuUZ7RBZy2xeFajJo9Czza"
```

---

### Task 11: Device verification (P1448-LE)

**Files:** none (fixes found here go into the task that owns the code, with a new commit).

Camera: `$AXIS_DEVICE_IP` (192.168.20.26), creds `$AXIS_DEVICE_USER`/`$AXIS_DEVICE_PASS`, ARTPEC-6, armv7hf, fw 11.11 → artifact `multimux-edge-fw11-armv7hf`. In zsh, write curl flags inline (variables holding several flags do not word-split). Abbreviations below: `U="$AXIS_DEVICE_USER:$AXIS_DEVICE_PASS"`, `SCRATCH=/private/tmp/claude-501/-Volumes-External-Projects-axis-origin/6d5001e0-bba8-440a-88ec-7cd3ee0cee5c/scratchpad`, `H="https://$AXIS_DEVICE_IP/local/multimuxedge"`, `B="https://$AXIS_DEVICE_IP/axis-cgi/applications"`.

- [ ] **Step 1: Get the build from CI and do a fresh install** (uninstall first, so the first-run path runs)

```bash
RUN=$(gh run list --branch stream-profiles --limit 1 --json databaseId -q '.[0].databaseId')
gh run download $RUN -D $SCRATCH/eap -p 'multimux-edge-fw11-armv7hf'
curl -sk --digest -u "$U" "$B/control.cgi?action=remove&package=multimuxedge"
curl -sk --digest -u "$U" -F "packfil=@$(ls $SCRATCH/eap/multimux-edge-fw11-armv7hf/*.eap)" "$B/upload.cgi"
curl -sk --digest -u "$U" "$B/control.cgi?action=start&package=multimuxedge"
sleep 10; curl -sk --digest -u "$U" "$H/admin/status"
```
Expected: `"last_error":null` (truncation fix), `"encodes":{"in_use":0,"max":2}`, `"streams":[]`.

- [ ] **Step 2: Profiles via the VAPIX service account**

`curl -sk --digest -u "$U" "$H/admin/profiles"` → the 6 camera profiles, each with `settings` (e.g. `ACC_Medium` → `h264 1280x720@25 ch0`), `error` null for all. If `error` says the service account is unavailable, check the manifest `resources` block made it into the package and read the app log (`/axis-cgi/admin/systemlog.cgi?appname=multimuxedge`).

- [ ] **Step 3: Map and play**

POST the config from Task 10's curl example (`medium → ACC_Medium`, default `medium`), plus `{"name":"4k","profile":"ACC_High"}` and `{"name":"low","profile":"ACC_Low"}`. Then:
```bash
ffprobe -v error -tls_verify 0 -show_entries stream=codec_name,width,height -of compact "https://$U@$AXIS_DEVICE_IP/local/multimuxedge/hls/medium/media.m3u8"
curl -sk --digest -u "$U" -o /dev/null -w '%{http_code} %{redirect_url}\n' "$H/hls/media.m3u8"
```
Expected: `h264 1280x720`; bare URL `302 …/medium/media.m3u8`. Record the actual channel the profile's `camera=1` mapped to and whether picture is correct (open item in the spec). If `ch1` fails on this single-sensor camera, change the `camera` mapping in `parse_profile` (Task 2) accordingly, with a test.

- [ ] **Step 4: Live change, no restart**

Remap `medium → ACC_Low` via POST; within a few seconds ffprobe `hls/medium/` shows `640x360`; `admin/status` shows the old capture gone. Remove `medium` → `hls/medium/media.m3u8` returns 404. The app's PID is unchanged (`systemlog` shows no restart).

- [ ] **Step 5: Encoder cap and 4K**

With `max_encodes` 2: pull `4k` and `low` concurrently with `ffmpeg -t 15 … -f null -` (pattern from the spike's `mix.sh`) → both ~25 fps (`low` profile is 5 fps by definition: expect 5). Then request a third distinct mapping → `503` body `encoder busy (2/2 encodes in use)`.

- [ ] **Step 6: Idle stop**

Stop all players; after `idle_timeout_secs` + 5 s, `admin/status` shows `"in_use":0`.

- [ ] **Step 7: Admin page and API**

Open `https://<cam>/local/multimuxedge/` in a browser (or headless): page loads, profile dropdown filled, add/remove/default/save work, a bad name shows its field error, status table updates, dark mode works, layout works at phone width. `GET $H/admin/openapi.json` returns the document. Player page plays `#4k` and `#main`.

- [ ] **Step 8: Record results**

Append a "Device verification" section to the PR description with the actual outputs (ffprobe lines, status JSON, fps numbers). Push any fixes, wait for CI green, then report.
