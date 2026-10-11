//! Picks an existing VDO encode to share. The camera's encoder fits about
//! one and a half 4K25 encodes, and its RTSP server (monolith) keeps
//! sharable encodes running for its clients. VDO hands a new stream the
//! *same* encode when the new stream's settings are an exact copy of a
//! sharable one, so joining a matching encode costs the encoder nothing,
//! the way a second RTSP client costs nothing. Host-testable: the device
//! code turns VDO's stream list into [`StreamDesc`]s.

use crate::convert::Codec;
use crate::profile::{CaptureSettings, Tuning};

/// VDO's `format` value for each codec (`VDO_FORMAT_H264` = 0,
/// `VDO_FORMAT_H265` = 1).
pub fn vdo_format(codec: Codec) -> u32 {
    match codec {
        Codec::H264 => 0,
        Codec::H265 => 1,
    }
}

/// The settings of an existing VDO stream that decide whether it can stand
/// in for a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDesc {
    pub id: u32,
    pub sharable: bool,
    pub channel: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    /// Frames per second; 0 when the stream doesn't say.
    pub framerate: u32,
    /// Key frame interval in frames; 0 when the stream doesn't say.
    pub gop_length: u32,
    /// The stream's encoder settings, in the VDO units [`Tuning`] maps to.
    pub compression: u32,
    pub rotation: u32,
    pub horizontal_flip: bool,
    pub rc_mode: u32,
    /// bit/s.
    pub bitrate: u32,
    /// bit/s.
    pub abr_target_bitrate: u32,
    pub abr_retention_time: u32,
    pub zip_gop_mode: u32,
    pub zip_fps_mode: u32,
    pub zip_max_gop_length: u32,
}

/// Whether `s` has every encoder setting `t` sets. Rotation and mirroring
/// change the picture, so leaving them unset means upright and unmirrored;
/// the other keys left unset accept whatever the stream uses.
fn tuning_matches(t: &Tuning, s: &StreamDesc) -> bool {
    let eq = |want: Option<u32>, have: u32| want.is_none_or(|w| w == have);
    t.rotation.unwrap_or(0) == s.rotation
        && t.mirror.unwrap_or(false) == s.horizontal_flip
        && eq(t.compression, s.compression)
        && eq(t.rate_control.map(|r| r.vdo_mode()), s.rc_mode)
        && eq(t.max_bitrate_kbps.map(kbps_to_bps), s.bitrate)
        && eq(t.abr_target_kbps.map(kbps_to_bps), s.abr_target_bitrate)
        && eq(t.abr_retention_secs, s.abr_retention_time)
        && eq(t.zip_dynamic_gop.map(u32::from), s.zip_gop_mode)
        && eq(t.zip_dynamic_fps.map(u32::from), s.zip_fps_mode)
        && eq(t.zip_max_gop_length, s.zip_max_gop_length)
}

/// VAPIX kbit/s to VDO bit/s (× 1024, as the camera's RTSP server does),
/// saturating rather than overflowing.
pub fn kbps_to_bps(kbps: u32) -> u32 {
    kbps.saturating_mul(1024)
}

/// The first sharable stream that delivers `want`: same channel, codec and
/// size, the same frame rate unless `want` leaves it to the camera (0), and
/// the same key frame interval and encoder settings where `want` sets them.
pub fn pick_shareable(streams: &[StreamDesc], want: &CaptureSettings) -> Option<u32> {
    streams
        .iter()
        .find(|s| {
            s.sharable
                && s.channel == want.channel
                && s.format == vdo_format(want.codec)
                && (s.width, s.height) == (want.width, want.height)
                && (want.framerate == 0 || s.framerate == want.framerate)
                && want.gop_length.is_none_or(|g| s.gop_length == g)
                && tuning_matches(&want.tuning, s)
        })
        .map(|s| s.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want() -> CaptureSettings {
        CaptureSettings {
            codec: Codec::H264,
            channel: 1,
            width: 3840,
            height: 2160,
            framerate: 25,
            gop_length: None,
            tuning: Tuning::default(),
        }
    }

    fn monolith_4k() -> StreamDesc {
        StreamDesc {
            id: 0x47,
            sharable: true,
            channel: 1,
            format: 0,
            width: 3840,
            height: 2160,
            framerate: 25,
            gop_length: 32,
            compression: 30,
            rotation: 0,
            horizontal_flip: false,
            rc_mode: 1,
            bitrate: 0,
            abr_target_bitrate: 0,
            abr_retention_time: 604800,
            zip_gop_mode: 0,
            zip_fps_mode: 0,
            zip_max_gop_length: 300,
        }
    }

    #[test]
    fn shares_a_matching_sharable_stream() {
        let other = StreamDesc {
            id: 0x60,
            width: 1280,
            height: 720,
            ..monolith_4k()
        };
        assert_eq!(pick_shareable(&[other, monolith_4k()], &want()), Some(0x47));
    }

    #[test]
    fn never_shares_a_stream_that_is_not_sharable() {
        let s = StreamDesc {
            sharable: false,
            ..monolith_4k()
        };
        assert_eq!(pick_shareable(&[s], &want()), None);
    }

    #[test]
    fn channel_codec_size_and_rate_must_match() {
        let w = want();
        for s in [
            StreamDesc {
                channel: 0,
                ..monolith_4k()
            },
            StreamDesc {
                format: 1,
                ..monolith_4k()
            },
            StreamDesc {
                width: 1920,
                height: 1080,
                ..monolith_4k()
            },
            StreamDesc {
                framerate: 15,
                ..monolith_4k()
            },
        ] {
            assert_eq!(pick_shareable(&[s], &w), None);
        }
    }

    #[test]
    fn camera_default_rate_accepts_any_rate() {
        let w = CaptureSettings {
            framerate: 0,
            ..want()
        };
        let s = StreamDesc {
            framerate: 15,
            ..monolith_4k()
        };
        assert_eq!(pick_shareable(&[s], &w), Some(0x47));
    }

    #[test]
    fn a_requested_key_frame_interval_must_match() {
        let w = CaptureSettings {
            gop_length: Some(25),
            ..want()
        };
        assert_eq!(pick_shareable(&[monolith_4k()], &w), None);
        let w = CaptureSettings {
            gop_length: Some(32),
            ..want()
        };
        assert_eq!(pick_shareable(&[monolith_4k()], &w), Some(0x47));
    }

    #[test]
    fn h265_maps_to_vdo_format_1() {
        let w = CaptureSettings {
            codec: Codec::H265,
            ..want()
        };
        let s = StreamDesc {
            format: 1,
            ..monolith_4k()
        };
        assert_eq!(pick_shareable(&[s], &w), Some(0x47));
    }

    #[test]
    fn encoder_settings_the_profile_sets_must_match() {
        use crate::profile::RateControl;
        let tuned = |t: Tuning| CaptureSettings {
            tuning: t,
            ..want()
        };
        let s = monolith_4k();
        assert_eq!(
            pick_shareable(
                &[s.clone()],
                &tuned(Tuning {
                    compression: Some(30),
                    ..Tuning::default()
                })
            ),
            Some(0x47)
        );
        for t in [
            Tuning {
                compression: Some(50),
                ..Tuning::default()
            },
            Tuning {
                rotation: Some(180),
                ..Tuning::default()
            },
            Tuning {
                mirror: Some(true),
                ..Tuning::default()
            },
            Tuning {
                rate_control: Some(RateControl::Mbr),
                ..Tuning::default()
            },
            Tuning {
                max_bitrate_kbps: Some(500),
                ..Tuning::default()
            },
            Tuning {
                zip_dynamic_gop: Some(true),
                ..Tuning::default()
            },
            Tuning {
                zip_max_gop_length: Some(600),
                ..Tuning::default()
            },
        ] {
            assert_eq!(pick_shareable(&[s.clone()], &tuned(t)), None, "{t:?}");
        }
        let mbr = StreamDesc {
            rc_mode: 2,
            bitrate: 512_000,
            ..monolith_4k()
        };
        let t = Tuning {
            rate_control: Some(RateControl::Mbr),
            max_bitrate_kbps: Some(500),
            ..Tuning::default()
        };
        assert_eq!(pick_shareable(&[mbr], &tuned(t)), Some(0x47));
    }

    #[test]
    fn unset_rotation_and_mirror_only_join_an_upright_unmirrored_encode() {
        let rotated = StreamDesc {
            rotation: 180,
            ..monolith_4k()
        };
        let mirrored = StreamDesc {
            horizontal_flip: true,
            ..monolith_4k()
        };
        assert_eq!(pick_shareable(&[rotated.clone(), mirrored], &want()), None);
        let t = Tuning {
            rotation: Some(180),
            ..Tuning::default()
        };
        assert_eq!(
            pick_shareable(
                &[rotated],
                &CaptureSettings {
                    tuning: t,
                    ..want()
                }
            ),
            Some(0x47)
        );
    }

    #[test]
    fn huge_bitrates_saturate_instead_of_overflowing() {
        let t = Tuning {
            max_bitrate_kbps: Some(u32::MAX),
            ..Tuning::default()
        };
        let s = StreamDesc {
            bitrate: u32::MAX,
            ..monolith_4k()
        };
        assert_eq!(
            pick_shareable(
                &[s],
                &CaptureSettings {
                    tuning: t,
                    ..want()
                }
            ),
            Some(0x47)
        );
    }
}
