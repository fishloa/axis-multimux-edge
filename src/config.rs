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
        MainPreset {
            channel: 0,
            width: 1920,
            height: 1080,
            framerate: 30,
            codec: "h264".into(),
        }
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
    FieldError {
        field: field.into(),
        message: message.into(),
    }
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
                errors.push(err(
                    format!("streams[{i}].profile"),
                    "choose a camera profile",
                ));
            }
        }
        if let Some(d) = &self.default_stream
            && !self.streams.iter().any(|s| &s.name == d)
        {
            errors.push(err("default_stream", format!("no stream named \"{d}\"")));
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
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn with_streams(streams: &[(&str, &str)], default: Option<&str>) -> Config {
        Config {
            streams: streams
                .iter()
                .map(|(n, p)| StreamMapping {
                    name: n.to_string(),
                    profile: p.to_string(),
                })
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
        with_streams(
            &[("medium", "ACC_Medium"), ("4k", "ACC_High"), ("a-b-1", "X")],
            Some("4k"),
        )
        .validate()
        .unwrap();
    }

    #[test]
    fn rejects_bad_names_with_field_paths() {
        for bad in [
            "Medium",
            "my stream",
            "-lead",
            "",
            &"a".repeat(33),
            "main",
            "media.m3u8",
        ] {
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
                "idle_timeout_secs",
                "main.codec",
                "max_encodes",
                "part_target_ms",
                "target_duration_secs",
                "window_segments",
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
            MainPreset {
                channel: 1,
                width: 1280,
                height: 720,
                framerate: 25,
                codec: "h265".into()
            }
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
