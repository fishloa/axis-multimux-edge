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
        let fps = if self.framerate == 0 {
            "auto".to_string()
        } else {
            self.framerate.to_string()
        };
        format!(
            "{codec} {}x{}@{fps} ch{}",
            self.width, self.height, self.channel
        )
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
        other => Err(format!(
            "unsupported video codec \"{other}\" (h264 or h265 only)"
        )),
    }
}

fn parse_u32(key: &str, v: &str) -> Result<u32, String> {
    v.parse::<u32>()
        .map_err(|_| format!("{key}: \"{v}\" is not a number"))
}

/// Convert a hex digit character (0-9, a-f, A-F) to its value, or None if invalid.
fn hex_val(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Try to decode two hex digits as a single byte.
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The camera a profile without `camera=` streams from: VAPIX's default,
/// which is what the camera's own RTSP server uses for the same profile.
pub const DEFAULT_CAMERA: u32 = 1;

/// Parse `parameters`; codec and resolution the profile leaves out come from
/// `fallback`. A missing `camera` means [`DEFAULT_CAMERA`] and a missing `fps`
/// camera default (0).
pub fn parse_profile(parameters: &str, fallback: &MainPreset) -> Result<ParsedProfile, String> {
    let mut settings = CaptureSettings::from_main(fallback)?;
    settings.channel = DEFAULT_CAMERA;
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
                let gop = parse_u32("videokeyframeinterval", &value)?;
                settings.gop_length = if gop == 0 { None } else { Some(gop) }
            }
            _ => {
                ignored.insert(key);
            }
        }
    }
    Ok(ParsedProfile {
        settings,
        ignored_keys: ignored.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_preset() -> MainPreset {
        MainPreset {
            channel: 0,
            width: 1920,
            height: 1080,
            framerate: 30,
            codec: "h264".into(),
        }
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
            CaptureSettings {
                codec: Codec::H264,
                channel: 1,
                width: 1280,
                height: 720,
                framerate: 15,
                gop_length: None
            }
        );
        assert_eq!(
            p.ignored_keys,
            vec!["audio".to_string(), "compression".to_string()]
        );
    }

    #[test]
    fn missing_keys_fall_back_to_main_and_fps_to_zero() {
        let p = parse_profile("resolution=640x360", &main_preset()).unwrap();
        assert_eq!(p.settings.codec, Codec::H264);
        // A missing `camera` is VAPIX's default camera 1, not the main preset's channel.
        assert_eq!(p.settings.channel, 1);
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
        assert!(
            parse_profile("videocodec=mjpeg", &main_preset())
                .unwrap_err()
                .contains("mjpeg")
        );
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
        let s = CaptureSettings {
            codec: Codec::H265,
            channel: 1,
            width: 3840,
            height: 2160,
            framerate: 25,
            gop_length: None,
        };
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

    #[test]
    fn percent_decode_utf8_safe() {
        // Test that invalid escape sequences and multi-byte UTF-8 don't panic.
        // "%4é" has a literal '%' at position 0, '4' at position 1, but 'é' is a multi-byte UTF-8 char.
        // The sequence "%4" is incomplete (needs 2 hex digits), so both '%' and '4' stay literal.
        let result = percent_decode("%4é");
        assert_eq!(result, "%4é");

        // Trailing "%" stays literal.
        let result = percent_decode("test%");
        assert_eq!(result, "test%");

        // Incomplete "%4" stays literal.
        let result = percent_decode("%4");
        assert_eq!(result, "%4");

        // Invalid hex "%zz" stays literal.
        let result = percent_decode("%zz");
        assert_eq!(result, "%zz");

        // Valid hex "%41" (0x41 = 'A') decodes correctly.
        let result = percent_decode("%41");
        assert_eq!(result, "A");
    }

    #[test]
    fn gop_length_zero_maps_to_none() {
        let p = parse_profile("videokeyframeinterval=0", &main_preset()).unwrap();
        assert_eq!(p.settings.gop_length, None);

        // Non-zero gop_length is still Some.
        let p = parse_profile("videokeyframeinterval=50", &main_preset()).unwrap();
        assert_eq!(p.settings.gop_length, Some(50));
    }
}
