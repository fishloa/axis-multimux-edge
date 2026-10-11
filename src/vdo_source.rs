//! VDO capture source (`device`-gated): drives the acap-rs `vdo` crate
//! (Axis VDO — Video Capture API) to pull hardware-encoded H.264/H.265
//! access units off a camera channel, and exposes them as a
//! [`media_plane::ingress::IngestSession`] so this crate's own driving loop
//! (`src/bin/multimux-edge.rs`'s `run_vdo_capture`, over
//! [`multimux::supervise_driver`]/[`multimux::source::advance_route`]) can
//! segment them straight into LL-HLS. Conversion of an Annex B access unit
//! into a [`transmux::pipeline::Sample`]/[`transmux::pipeline::TrackSpec`] is
//! delegated entirely to the pure [`crate::convert`] module — this module's
//! only job is driving VDO and doing the timestamp/frame-type bookkeeping VDO
//! itself doesn't do for us.
//!
//! # Why a bare [`IngestSession`], not a [`media_plane::ingress::Dialer`]
//!
//! Every other multimux-ported input (RTSP/RTMP/SRT/TS-*/HLS-DASH-Smooth
//! pull) dials *out* over a network transport, so it fits
//! `Dialer::dial() -> Session` + the sans-IO handshake pump `media-plane`
//! documents (`dial()` performs no I/O; the handshake completes through the
//! ordinary `feed`/`poll_transmit` loop). VDO capture is not a dial-out at
//! all — it is a **local** hardware channel this process already owns, and
//! opening/starting it (`vdo::StreamBuilder::build`/`Stream::start`) plus the
//! in-band-parameter-set scan (see [`scan_for_param_sets`]) are one-shot local
//! setup work, not a multi-round-trip protocol exchange a `Dialer`/handshake
//! pump would buy anything by re-modelling. So `VdoIngestSession` is
//! constructed directly (`VdoIngestSession::new`, doing that local setup
//! synchronously) and driven directly by an
//! [`media_plane::ingress::IngestDriver`] the caller builds itself — no
//! `Dialer`, no [`media_plane::ingress::DialSupervisor`]. The caller's own
//! `attempt` closure (passed to [`multimux::supervise_driver`]) plays the
//! role a `Dialer`'s retry would have: if VDO setup or a later buffer read
//! fails, the closure returns and `supervise_driver` retries the whole
//! `VdoIngestSession::new` from scratch after backoff, exactly like a failed
//! dial would.
//!
//! # `Stage::In` is `()`: there is nothing to feed
//!
//! Every byte-stream `IngestSession` in `multimux` states `type In<'a> = &'a
//! [u8]` because its driving loop reads bytes off a socket and hands them to
//! `feed`. VDO has no bytes for a caller to read and hand in this shape —
//! the *session itself* performs the (blocking) hardware read inside `feed`.
//! So `VdoIngestSession::In<'a> = ()`: the driving loop's contract becomes
//! "call `feed(())` again to advance", and `feed` is where the blocking VDO
//! read (or, on the very first call, replaying the already-resolved
//! parameter-set scan's pending access unit) actually happens. This is
//! exactly the relaxation `media_plane::ingress`'s round-3 docs describe for
//! non-byte-stream sources (a pull source states its own request/response
//! shape; a pure local-capture source states the simplest honest shape it
//! has, which for VDO is nothing at all).
//!
//! # Blocking I/O — run on a dedicated thread or task
//!
//! [`vdo::RunningStream::next_buffer`] **blocks the calling thread** until the
//! camera produces the next frame (it is a synchronous FFI call into
//! `libvdo.so`, not a `poll`-based async I/O source). `VdoIngestSession::feed`
//! calls it directly and therefore blocks too. **Whoever drives the VDO
//! capture loop (`src/bin/multimux-edge.rs`'s `run_vdo_capture`, spawned by
//! `spawn_capture_pipeline`) must ensure this blocking read does not stall
//! other work on the same thread**: that function runs the whole
//! capture/segment/store pipeline on its own `std::thread::spawn`'d OS thread
//! with a dedicated `current_thread` tokio runtime, so the blocking call only
//! ever stalls that one dedicated thread, never axum's worker threads. Note
//! that `RunningStream`/`Stream` are `unsafe impl Send` in the `vdo` crate
//! (verified against acap-rs rev `8e58acb8f0617253ad21fb71ac319fea19454a38`),
//! so `VdoIngestSession` itself is `Send` (required by
//! [`IngestSession: Send`](media_plane::ingress::IngestSession)) — the risk is
//! purely the blocking call starving a single-threaded executor, not a `Send`
//! bound failure.
//!
//! # Spec / API grounding
//!
//! Against acap-rs rev `8e58acb8f0617253ad21fb71ac319fea19454a38`:
//! - `vdo::StreamBuilder::{channel, format, resolution, framerate}` + `.build()`
//!   (`crates/vdo/src/lib.rs`).
//! - `vdo::Stream::start() -> RunningStream` (consumes `self`).
//! - `vdo::RunningStream::next_buffer(&self) -> Result<StreamBuffer<'_>, vdo::Error>`
//!   (blocking, see above).
//! - `vdo::StreamBuffer::{data_copy, as_slice, header_size, frame_type,
//!   timestamp}` — `data_copy()` returns the coded slice with the buffer's
//!   header (`header_size` bytes) stripped, which is exactly the CMAF sample
//!   `crate::convert` expects. On a *key* frame that header is where VDO
//!   carries the SPS/PPS parameter sets, so [`scan_for_param_sets`] reads the
//!   full frame via `as_slice()` (**not** `data_copy()`) to recover them — see
//!   its doc. **This distinction is load-bearing**: reading `data_copy()`
//!   here would silently drop the parameter sets and produce a stream that
//!   looks structurally fine (init segment builds, segments serve) but that
//!   no real decoder can actually decode.
//! - `vdo::VdoFrameType::{VDO_FRAME_TYPE_H264_IDR, VDO_FRAME_TYPE_H265_IDR}` are
//!   the sync-sample frame types for H.264/H.265 respectively (confirmed
//!   against `vdo`'s own `capture_h264_frames` hardware test, which matches on
//!   `VDO_FRAME_TYPE_H264_IDR | VDO_FRAME_TYPE_H264_I` for "got a key frame");
//!   `VdoIngestSession` treats only the IDR variant as a sync sample
//!   (`is_sync`) — the non-IDR `_I` type is an intra frame that need not reset
//!   the decoder's reference-picture state, so it is not a safe CMAF
//!   random-access point.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use broadcast_common::{Demand, Stage, Timestamp};
use media_plane::ingress::{IngestSession, ProgramId, SessionEvent};
use media_plane::trunk::RetentionClass;
use transmux::pipeline::{Sample, TrackSpec};
use vdo::{Map, RunningStream, Stream, VdoFormat, VdoFrameType};

use crate::Result;
use crate::admin::EncodeShare;
use crate::convert::{self, Codec, ParamSets};
use crate::error::OriginError;

/// Fixed single (video) track id — `VdoIngestSession` carries exactly one
/// video track per stream.
const TRACK_ID: u32 = 1;

/// This app's single program id — VDO captures exactly one camera channel
/// per session, so there is exactly one program, always `ProgramId(0)`.
const PROGRAM: ProgramId = ProgramId(0);

/// Media/track timescale for both H.264 and H.265 (90 kHz video clock).
const CLOCK_RATE: u32 = 90_000;

/// How often a session on a joined encode re-reads the encode's `peers`
/// count, to notice when it is left as the encode's only client.
const PEER_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How often `scan_for_param_sets` logs that it is still waiting for a key frame.
const SCAN_WARN_INTERVAL: Duration = Duration::from_secs(10);

/// How often `scan_for_param_sets` re-asks VDO for a key frame while waiting.
/// For an encode of our own the first request goes out before the first
/// buffer read; on a joined encode it waits one interval. Repeats cover a
/// request that lands on frames already queued ahead of it.
const FORCE_KEY_FRAME_INTERVAL: Duration = Duration::from_secs(2);

/// The first IDR access unit found while collecting parameter sets, held onto
/// so it can be delivered as the first real sample instead of being dropped.
///
/// # Why buffer instead of discard
///
/// [`VdoIngestSession::new`] must return with a complete [`TrackSpec`] already
/// built (before the caller's `IngestDriver` ever calls `feed`), so it reads
/// ahead into the live buffer stream until it can resolve the parameter sets
/// — from the key frame's own header (the primary path) or, as a fallback,
/// from separately-delivered parameter-set buffers (see
/// [`scan_for_param_sets`]). That first resolving key frame is the first
/// decodable sync sample for this stream start — exactly the sample an LL-HLS
/// segmenter needs as its first pushed sample. Discarding it and pulling a
/// fresh buffer for the first `feed()` call would drop that IDR and could
/// hand the segmenter a non-sync first sample. It is delivered as its
/// header-stripped `data_copy()` (SPS/PPS/VPS are carried in the `avcC`/`hvcC`
/// init segment, not the coded samples); any parameter-set/SEI buffer and any
/// picture read before the parameter sets resolve are not decodable relative
/// to the init this source will publish, and are dropped.
struct PendingAu {
    data: Vec<u8>,
    timestamp_us: u64,
    frame_type: VdoFrameType,
}

/// A live VDO stream adapted into a [`media_plane::ingress::IngestSession`].
///
/// Built by [`VdoIngestSession::new`], which starts the VDO stream, scans
/// forward for in-band parameter sets, and pre-builds the single-track
/// [`TrackSpec`] — all before the first [`Stage::feed`] call, so
/// [`SessionEvent::Established`] and [`SessionEvent::NewProgram`] are already
/// queued and ready the moment the caller starts driving. Every subsequent
/// [`feed`](Stage::feed) call blocks (see the module doc) on
/// [`RunningStream::next_buffer`].
pub struct VdoIngestSession {
    /// In a `Mutex` only to make the session `Sync`: multimux 0.11's async
    /// `advance_route` holds `&IngestDriver<Self>` across an `.await`, and
    /// `vdo::RunningStream` (a raw C handle) is `Send` but not `Sync`. Never
    /// actually locked: the only access is `Mutex::get_mut` from `&mut self`.
    running: Mutex<RunningStream>,
    /// Id of the existing encode this session joined, if it joined one.
    shared_encode: Option<u32>,
    /// Told how the encode relates to the camera's other clients: once when
    /// the session opens, then whenever a joined encode's `peers` change.
    on_encode: Box<dyn Fn(EncodeShare) + Send + Sync>,
    /// Last `peers` given to `on_encode` for a joined encode.
    last_peers: u32,
    last_peer_check: Instant,
    track_id: u32,
    codec: Codec,
    clock_rate: u32,
    specs: Vec<TrackSpec>,
    prev_ts_us: Option<u64>,
    /// The parameter-set-bearing access unit found by `new()`, replayed as
    /// the very first sample (see [`PendingAu`]).
    pending_first: Option<PendingAu>,
    /// `true` once the initial `Established`+`NewProgram`(+first sample)
    /// batch has been queued — every `feed()` call after that instead
    /// performs one blocking VDO buffer read (see [`Self::feed`]).
    initial_batch_sent: bool,
    /// Events ready for [`Stage::poll`] to hand back, in order.
    pending: VecDeque<SessionEvent>,
    /// Diagnostic window (4K frame-rate investigation): buffers seen per VDO
    /// frame type (including skipped non-picture ones), total buffers, and
    /// min/max delta between consecutive buffer timestamps (microseconds).
    diag_counts: Vec<(VdoFrameType, u32)>,
    diag_buffers: u32,
    diag_prev_ts_us: Option<u64>,
    diag_min_delta_us: Option<u64>,
    diag_max_delta_us: Option<u64>,
}

/// Buffers per diagnostic log line.
const DIAG_WINDOW: u32 = 250;

impl VdoIngestSession {
    /// Open the VDO channel described by `settings` (channel, size, framerate,
    /// codec, optional GOP length), start it, and scan forward for the in-band parameter sets needed to
    /// build the track's `avcC`/`hvcC`.
    ///
    /// # Errors
    /// Returns [`OriginError::Vdo`] if the stream can't be built/started or a
    /// buffer read fails while scanning for parameter sets, and
    /// [`OriginError::Convert`] if `stop` is set while still waiting for a key
    /// frame (the scan has no buffer limit), or the parameter sets found
    /// don't decode into a valid `TrackSpec` (propagated from
    /// [`convert::track_spec`]).
    ///
    /// `on_encode` is told whether the session runs its own encode or joined
    /// one (with that encode's client count), once the stream is open and
    /// again whenever a joined encode's client count changes.
    pub fn new(
        settings: &crate::profile::CaptureSettings,
        stop: &AtomicBool,
        on_encode: Box<dyn Fn(EncodeShare) + Send + Sync>,
    ) -> Result<Self> {
        let codec = settings.codec;
        let (stream, shared_encode) = match join_existing_encode(settings) {
            Some((stream, id)) => (stream, Some(id)),
            None => {
                let stream = Stream::from_settings(&own_encode_settings(settings))?;
                log::info!(
                    "vdo: new encode {} for {}",
                    stream.id(),
                    settings.describe()
                );
                (stream, None)
            }
        };

        let running = stream.start()?;
        // A joined encode's peers right after start() may not include us
        // yet; report them as unknown (counted as shared) until the first
        // peer check.
        on_encode(match shared_encode {
            Some(id) => EncodeShare::Joined { id, peers: 0 },
            None => EncodeShare::Own,
        });

        // Forcing a key frame on a joined encode adds one for everyone using
        // it, so there the scan first gives the encode's own GOP a chance.
        let (params, pending_first) =
            scan_for_param_sets(&running, codec, stop, shared_encode.is_none())?;
        let spec = convert::track_spec(codec, &params, TRACK_ID, CLOCK_RATE)?;

        Ok(Self {
            running: Mutex::new(running),
            shared_encode,
            on_encode,
            last_peers: 0,
            last_peer_check: Instant::now(),
            track_id: TRACK_ID,
            codec,
            clock_rate: CLOCK_RATE,
            specs: vec![spec],
            prev_ts_us: None,
            pending_first: Some(pending_first),
            initial_batch_sent: false,
            pending: VecDeque::new(),
            diag_counts: Vec::new(),
            diag_buffers: 0,
            diag_prev_ts_us: None,
            diag_min_delta_us: None,
            diag_max_delta_us: None,
        })
    }

    /// On a joined encode, every [`PEER_CHECK_INTERVAL`], re-read the
    /// encode's `peers` and tell `on_encode` when it changed, e.g. when the
    /// camera's other clients of the encode leave.
    fn check_peers(&mut self) {
        let Some(id) = self.shared_encode else { return };
        if self.last_peer_check.elapsed() < PEER_CHECK_INTERVAL {
            return;
        }
        self.last_peer_check = Instant::now();
        let peers = encode_peers(
            self.running
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        if peers != 0 && peers != self.last_peers {
            log::info!("vdo: encode {id} now has {peers} client(s)");
            self.last_peers = peers;
            (self.on_encode)(EncodeShare::Joined { id, peers });
        }
    }

    /// Read exactly one more coded-picture buffer from VDO (blocking; skips
    /// any interleaved parameter-set/SEI buffers exactly like
    /// [`scan_for_param_sets`]'s own skip loop), and turn it into a
    /// [`Sample`].
    fn read_next_sample(&mut self) -> Result<Sample> {
        self.check_peers();
        loop {
            let buf = self
                .running
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .next_buffer()?;
            let ft = buf.frame_type();
            let ts = buf.timestamp();
            // Diagnostic counters (see the `diag_*` fields).
            match self.diag_counts.iter_mut().find(|(t, _)| *t == ft) {
                Some((_, n)) => *n += 1,
                None => self.diag_counts.push((ft, 1)),
            }
            if let Some(prev) = self.diag_prev_ts_us {
                let d = ts.abs_diff(prev);
                self.diag_min_delta_us = Some(self.diag_min_delta_us.map_or(d, |m| m.min(d)));
                self.diag_max_delta_us = Some(self.diag_max_delta_us.map_or(d, |m| m.max(d)));
            }
            self.diag_prev_ts_us = Some(ts);
            self.diag_buffers += 1;
            if self.diag_buffers >= DIAG_WINDOW {
                log::debug!(
                    "vdo diag: {} buffers by type {:?}, ts delta us min={:?} max={:?}",
                    self.diag_buffers,
                    self.diag_counts,
                    self.diag_min_delta_us,
                    self.diag_max_delta_us,
                );
                self.diag_counts.clear();
                self.diag_buffers = 0;
                self.diag_min_delta_us = None;
                self.diag_max_delta_us = None;
            }
            if !is_picture(self.codec, ft) {
                // non-key pictures / SEI while live: dropped, exactly as the
                // scan loop drops them.
                continue;
            }
            let data = buf.data_copy()?;
            let timestamp_us = ts;
            let is_sync = is_idr(self.codec, ft);
            let duration = convert::duration_ticks(
                self.prev_ts_us.unwrap_or(timestamp_us),
                timestamp_us,
                self.clock_rate,
            );
            self.prev_ts_us = Some(timestamp_us);
            let pts_dts = convert::absolute_ticks(timestamp_us, self.clock_rate);
            return Ok(convert::au_to_sample(
                self.codec, &data, pts_dts, duration, is_sync,
            ));
        }
    }
}

/// VDO settings for an encode of our own. Only a GOP length the profile
/// sets is passed on (otherwise the camera's default stands and
/// `scan_for_param_sets` waits for the next key frame, however far off
/// dynamic GOP / Zipstream puts it, #669), plus the profile's encoder keys
/// in the VDO settings the camera's RTSP server uses for them.
fn own_encode_settings(settings: &crate::profile::CaptureSettings) -> Map {
    let mut m = Map::new();
    m.set_u32(c"channel", settings.channel);
    m.set_u32(
        c"format",
        match settings.codec {
            Codec::H264 => VdoFormat::VDO_FORMAT_H264.0 as u32,
            Codec::H265 => VdoFormat::VDO_FORMAT_H265.0 as u32,
        },
    );
    m.set_u32(c"width", settings.width);
    m.set_u32(c"height", settings.height);
    if settings.framerate > 0 {
        m.set_u32(c"framerate", settings.framerate);
    }
    if let Some(gop) = settings.gop_length {
        m.set_u32(c"gop_length", gop);
    }
    m.set_u32(c"buffer.count", 3);
    let t = &settings.tuning;
    let u32s = [
        (c"compression", t.compression),
        (c"rotation", t.rotation),
        (c"rc.mode", t.rate_control.map(|r| r.vdo_mode())),
        (
            c"bitrate",
            t.max_bitrate_kbps.map(crate::vdo_share::kbps_to_bps),
        ),
        (
            c"abr.target_bitrate",
            t.abr_target_kbps.map(crate::vdo_share::kbps_to_bps),
        ),
        (c"abr.retention_time", t.abr_retention_secs),
        (c"zip.gop_mode", t.zip_dynamic_gop.map(u32::from)),
        (c"zip.fps_mode", t.zip_dynamic_fps.map(u32::from)),
        (c"zip.max_gop_length", t.zip_max_gop_length),
    ];
    for (key, value) in u32s {
        if let Some(v) = value {
            m.set_u32(key, v);
        }
    }
    if let Some(mirror) = t.mirror {
        m.set_bool(c"horizontal_flip", mirror);
    }
    m
}

/// How many clients the encode behind `running` has (VDO's `peers`), or 0
/// if VDO doesn't say.
fn encode_peers(running: &RunningStream) -> u32 {
    match running.info() {
        Ok(info) => info.get_u32(c"peers", 0),
        Err(e) => {
            log::debug!("vdo: can't read encode peers: {e}");
            0
        }
    }
}

/// The existing encode a capture with `want` would join: one the camera
/// already runs (for its RTSP clients, say) and VDO lets us share. The
/// encoder fits about one and a half 4K25 encodes, and VDO hands a new
/// stream the *same* encode when its settings are an exact copy of a
/// sharable stream's, so joining costs the encoder nothing (see
/// [`crate::vdo_share`]). Our own encodes are left out: the registry
/// already shares a capture between streams with equal settings, and
/// joining one of ours would hide an encode from the encode count.
fn find_joinable(want: &crate::profile::CaptureSettings) -> Option<vdo::StreamInfo> {
    let streams = match vdo::list_streams() {
        Ok(streams) => streams,
        Err(e) => {
            log::warn!("vdo: can't list streams to share an encode: {e}");
            return None;
        }
    };
    let ours = own_identity();
    let candidates: Vec<vdo::StreamInfo> = streams
        .into_iter()
        .filter(|s| ours.as_deref() != Some(identity(&s.settings).as_str()))
        .collect();
    let descs: Vec<crate::vdo_share::StreamDesc> = candidates.iter().map(describe_stream).collect();
    let id = crate::vdo_share::pick_shareable(&descs, want)?;
    candidates.into_iter().find(|s| s.id == id)
}

/// Whether a capture with `want` would join an existing encode; see
/// [`find_joinable`].
pub fn can_join_existing_encode(want: &crate::profile::CaptureSettings) -> bool {
    find_joinable(want).is_some()
}

/// Join the encode [`find_joinable`] picks for `want`. Returns the stream
/// and the joined encode's id, or `None` (logged) to fall back to an encode
/// of our own.
fn join_existing_encode(want: &crate::profile::CaptureSettings) -> Option<(Stream, u32)> {
    let existing = find_joinable(want)?;
    let id = existing.id;
    let mut copy = existing.settings.clone();
    for key in [c"id", c"identity", c"intent"] {
        copy.remove(key);
    }
    let stream = match Stream::from_settings(&copy) {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!("vdo: joining encode {id} failed, starting our own: {e}");
            return None;
        }
    };
    let owner = identity(&existing.settings);
    if stream.id() != id {
        // VDO started a separate encode with the other client's settings
        // (its GOP, say) instead; drop it and build our own.
        log::warn!(
            "vdo: asked to join encode {id} ({owner}) but got new encode {}; starting our own",
            stream.id()
        );
        return None;
    }
    log::info!("vdo: joined encode {id} ({owner}) for {}", want.describe());
    Some((stream, id))
}

/// The `identity` VDO records for a stream: the name of the process that
/// created it.
fn identity(settings: &Map) -> String {
    settings
        .get_string(c"identity")
        .map(|s| s.as_c_str().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// This process's VDO identity: VDO records the creating process's name,
/// which on the camera is the executable's name (`multimuxedge`).
fn own_identity() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// The settings of an existing VDO stream that [`crate::vdo_share`] matches on.
fn describe_stream(s: &vdo::StreamInfo) -> crate::vdo_share::StreamDesc {
    let m: &Map = &s.settings;
    let mut framerate = m.get_u32(c"framerate", 0);
    if framerate == 0 {
        framerate = m.get_f64(c"framerate", 0.0).round() as u32;
    }
    crate::vdo_share::StreamDesc {
        id: s.id,
        sharable: m.get_bool(c"sharable", false),
        channel: m.get_u32(c"channel", u32::MAX),
        format: m.get_u32(c"format", u32::MAX),
        width: m.get_u32(c"width", 0),
        height: m.get_u32(c"height", 0),
        framerate,
        gop_length: m.get_u32(c"gop_length", 0),
        compression: m.get_u32(c"compression", u32::MAX),
        rotation: m.get_u32(c"rotation", 0),
        horizontal_flip: m.get_bool(c"horizontal_flip", false),
        rc_mode: m.get_u32(c"rc.mode", u32::MAX),
        bitrate: m.get_u32(c"bitrate", 0),
        abr_target_bitrate: m.get_u32(c"abr.target_bitrate", 0),
        abr_retention_time: m.get_u32(c"abr.retention_time", 0),
        zip_gop_mode: m.get_u32(c"zip.gop_mode", 0),
        zip_fps_mode: m.get_u32(c"zip.fps_mode", 0),
        zip_max_gop_length: m.get_u32(c"zip.max_gop_length", 0),
    }
}

/// Read buffers from `running` until the parameter sets (SPS/PPS for H.264;
/// VPS/SPS/PPS for H.265) can be resolved, returning them plus the key-frame
/// access unit as a [`PendingAu`]. There is no buffer limit: it waits as long
/// as the camera takes to produce a key frame, warning every
/// [`SCAN_WARN_INTERVAL`], and fails only on a read error or once `stop` is set.
///
/// Where VDO puts the parameter sets (verified on hardware, ARTPEC-6/H.264,
/// #669): they are carried in the **key-frame buffer's header** — the bytes
/// `data_copy()` strips off the front (`header_size`) to leave just the coded
/// slice. So the full key-frame buffer is `[SPS][PPS][…][IDR slice]` in Annex
/// B, and `extract_param_sets` finds the run in `as_slice()` even though
/// `data_copy()` (the sample bytes) does not contain it. As a fallback for
/// cameras/configs that instead deliver each parameter set as its own buffer
/// (frame types `VDO_FRAME_TYPE_H264_SPS`/`_PPS`, …), those are also collected
/// and tried. The sample handed on for the key frame is the header-stripped
/// `data_copy()` (parameter sets ride in the `avcC`/`hvcC` init, not samples).
fn scan_for_param_sets(
    running: &RunningStream,
    codec: Codec,
    stop: &AtomicBool,
    force_at_start: bool,
) -> Result<(ParamSets, PendingAu)> {
    // Fallback path: latest Annex B bytes for each separately-delivered
    // parameter-set NAL, kept individually so a resent run replaces it.
    let mut vps: Option<Vec<u8>> = None; // H.265 only
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;

    let started = Instant::now();
    let mut last_warn = started;
    // `None` forces before the first read; otherwise the first force waits
    // one interval.
    let mut last_force: Option<Instant> = if force_at_start { None } else { Some(started) };
    let mut seen: usize = 0;
    loop {
        // Ask for a key frame now rather than waiting out the camera's GOP.
        // Best effort: on failure the scan still waits for a natural one.
        if last_force.is_none_or(|t| t.elapsed() >= FORCE_KEY_FRAME_INTERVAL) {
            last_force = Some(Instant::now());
            if let Err(e) = running.force_key_frame() {
                log::warn!(
                    "vdo scan: force_key_frame failed, waiting for a natural key frame: {e}"
                );
            }
        }
        let buf = running.next_buffer()?;
        let i = seen;
        seen += 1;
        if stop.load(Ordering::Relaxed) {
            return Err(OriginError::Convert(
                "capture stopped while waiting for a key frame".into(),
            ));
        }
        if last_warn.elapsed() >= SCAN_WARN_INTERVAL {
            last_warn = Instant::now();
            log::warn!(
                "vdo scan: still waiting for a key frame after {}s ({} buffers seen)",
                started.elapsed().as_secs(),
                seen,
            );
        }
        let ft = buf.frame_type();
        let data = buf.data_copy()?;
        if let Some(kind) = param_set_kind(codec, ft) {
            // Separately-delivered parameter-set buffer (fallback path).
            match kind {
                ParamSetKind::Vps => vps = Some(data),
                ParamSetKind::Sps => sps = Some(data),
                ParamSetKind::Pps => pps = Some(data),
            }
            continue;
        }
        if is_idr(codec, ft) {
            // Primary path: the key frame's own buffer carries the parameter
            // sets in the header that `data_copy()` strips — parse the *full*
            // frame (`as_slice()` up to `size()`).
            let full = buf.as_slice()?;
            let full_au = &full[..buf.size().min(full.len())];
            if let Some(params) = convert::extract_param_sets(codec, full_au) {
                log::info!(
                    "vdo scan: parameter sets from key-frame header at buf[{i}] ({ft:?}, {} ms)",
                    started.elapsed().as_millis()
                );
                return Ok((
                    params,
                    PendingAu {
                        timestamp_us: buf.timestamp(),
                        frame_type: ft,
                        data,
                    },
                ));
            }
            // Fallback path: pair the separately-collected parameter sets with
            // this key frame. Their concatenation is valid Annex B (each is a
            // whole Annex-B NAL buffer).
            let mut blob = Vec::new();
            if let Some(v) = &vps {
                blob.extend_from_slice(v);
            }
            if let (Some(s), Some(p)) = (&sps, &pps) {
                blob.extend_from_slice(s);
                blob.extend_from_slice(p);
            }
            if let Some(params) = convert::extract_param_sets(codec, &blob) {
                log::info!(
                    "vdo scan: parameter sets from separate buffers, IDR at buf[{i}] ({ft:?}, {} ms)",
                    started.elapsed().as_millis()
                );
                return Ok((
                    params,
                    PendingAu {
                        timestamp_us: buf.timestamp(),
                        frame_type: ft,
                        data,
                    },
                ));
            }
            // Key frame before parameter sets resolve (mid-GOP start): drop and
            // keep scanning.
        }
        // non-key pictures / SEI while scanning: dropped
    }
}

/// Which parameter-set NAL a VDO parameter-set frame type carries.
enum ParamSetKind {
    /// H.265 video parameter set (no H.264 equivalent).
    Vps,
    /// Sequence parameter set.
    Sps,
    /// Picture parameter set.
    Pps,
}

/// Classify a VDO `frame_type` as a parameter-set buffer, if it is one. VDO
/// delivers SPS/PPS (and, for H.265, VPS) as dedicated buffers rather than
/// in-band with the coded picture (see [`scan_for_param_sets`]).
fn param_set_kind(codec: Codec, ft: VdoFrameType) -> Option<ParamSetKind> {
    // VDO frame-type values are associated consts, not enum variants usable in
    // patterns, so classify with `==` comparisons (as `is_idr` does).
    match codec {
        Codec::H264 => {
            if ft == VdoFrameType::VDO_FRAME_TYPE_H264_SPS {
                Some(ParamSetKind::Sps)
            } else if ft == VdoFrameType::VDO_FRAME_TYPE_H264_PPS {
                Some(ParamSetKind::Pps)
            } else {
                None
            }
        }
        Codec::H265 => {
            if ft == VdoFrameType::VDO_FRAME_TYPE_H265_VPS {
                Some(ParamSetKind::Vps)
            } else if ft == VdoFrameType::VDO_FRAME_TYPE_H265_SPS {
                Some(ParamSetKind::Sps)
            } else if ft == VdoFrameType::VDO_FRAME_TYPE_H265_PPS {
                Some(ParamSetKind::Pps)
            } else {
                None
            }
        }
    }
}

/// Whether `frame_type` is a coded-picture buffer (IDR/I/P/B) — the buffers
/// that become CMAF samples. Parameter-set (SPS/PPS/VPS) and SEI buffers are
/// **not** samples: parameter sets live in the `avcC`/`hvcC` init segment, and
/// a standalone SEI buffer is not a coded picture. [`VdoIngestSession::read_next_sample`]
/// skips everything that is not a picture.
fn is_picture(codec: Codec, ft: VdoFrameType) -> bool {
    // `==` chains rather than `matches!` — VDO frame types are associated
    // consts, not pattern-usable enum variants.
    match codec {
        Codec::H264 => {
            ft == VdoFrameType::VDO_FRAME_TYPE_H264_IDR
                || ft == VdoFrameType::VDO_FRAME_TYPE_H264_I
                || ft == VdoFrameType::VDO_FRAME_TYPE_H264_P
                || ft == VdoFrameType::VDO_FRAME_TYPE_H264_B
        }
        Codec::H265 => {
            ft == VdoFrameType::VDO_FRAME_TYPE_H265_IDR
                || ft == VdoFrameType::VDO_FRAME_TYPE_H265_I
                || ft == VdoFrameType::VDO_FRAME_TYPE_H265_P
                || ft == VdoFrameType::VDO_FRAME_TYPE_H265_B
        }
    }
}

/// Whether `frame_type` is the codec's IDR (instantaneous decoder refresh)
/// frame type — the only VDO frame type this module treats as a CMAF sync
/// sample (see the module doc for why the non-IDR `_I` type is excluded).
fn is_idr(codec: Codec, frame_type: VdoFrameType) -> bool {
    match codec {
        Codec::H264 => frame_type == VdoFrameType::VDO_FRAME_TYPE_H264_IDR,
        Codec::H265 => frame_type == VdoFrameType::VDO_FRAME_TYPE_H265_IDR,
    }
}

impl Stage for VdoIngestSession {
    /// Nothing to feed — VDO capture has no bytes for a caller to read and
    /// hand in; `feed` itself performs the (blocking) hardware read. See the
    /// module doc's "`Stage::In` is `()`" section.
    type In<'a> = ();
    type Out = SessionEvent;
    type Error = OriginError;

    /// Always "one more" — VDO capture has no meaningful backlog signal to
    /// report; the caller drives this session in a plain loop regardless.
    fn demand(&self) -> Demand {
        Demand::new(1)
    }

    /// Advance the session by exactly one step.
    ///
    /// The **first** call queues [`SessionEvent::Established`],
    /// [`SessionEvent::NewProgram`] (this session's single video track), and
    /// — if [`VdoIngestSession::new`]'s parameter-set scan captured one — the
    /// buffered key-frame [`SessionEvent::Sample`], all without touching VDO
    /// again (everything needed was already resolved synchronously in
    /// `new()`). **Every call after that** blocks on
    /// [`RunningStream::next_buffer`] (via [`Self::read_next_sample`]) and
    /// queues exactly one more [`SessionEvent::Sample`]. A live camera
    /// channel has no natural end-of-stream, so a VDO read/convert failure
    /// here is reported as `Err` (driving [`media_plane::ingress::HealthState::Failed`])
    /// rather than a clean [`Stage::finish`].
    fn feed(&mut self, _input: (), _now: Timestamp) -> Result<()> {
        if !self.initial_batch_sent {
            self.initial_batch_sent = true;
            self.pending.push_back(SessionEvent::Established);
            self.pending.push_back(SessionEvent::NewProgram {
                program: PROGRAM,
                tracks: self.specs.clone(),
            });
            // The pending IDR from `new()`'s scan is always a coded picture
            // (parameter-set/SEI buffers read while scanning were already
            // dropped there) — deliver it as the first sample rather than
            // discarding it (see `PendingAu`'s doc).
            if let Some(pending) = self.pending_first.take() {
                let is_sync = is_idr(self.codec, pending.frame_type);
                // First sample: no previous timestamp to diff against, so
                // duration is 0 — matches every other ported source's first
                // sample (no extra frame of latency; VDO delivers one whole
                // access unit per buffer with its own timestamp).
                let duration = convert::duration_ticks(
                    pending.timestamp_us,
                    pending.timestamp_us,
                    self.clock_rate,
                );
                self.prev_ts_us = Some(pending.timestamp_us);
                let pts_dts = convert::absolute_ticks(pending.timestamp_us, self.clock_rate);
                let sample =
                    convert::au_to_sample(self.codec, &pending.data, pts_dts, duration, is_sync);
                self.pending.push_back(SessionEvent::Sample {
                    program: PROGRAM,
                    track_id: self.track_id,
                    retention: RetentionClass::Timed,
                    sample,
                });
            }
            return Ok(());
        }

        let sample = self.read_next_sample()?;
        self.pending.push_back(SessionEvent::Sample {
            program: PROGRAM,
            track_id: self.track_id,
            retention: RetentionClass::Timed,
            sample,
        });
        Ok(())
    }

    fn poll(&mut self) -> Option<SessionEvent> {
        self.pending.pop_front()
    }

    /// No time-driven work of its own — every event is produced by a `feed`
    /// call (see [`Self::feed`]).
    fn next_deadline(&self) -> Option<Timestamp> {
        None
    }

    fn on_deadline(&mut self, _now: Timestamp) {}

    /// A live camera channel is never told to stop by VDO itself; this only
    /// runs if the caller decides to stop driving (e.g. process shutdown),
    /// and there is nothing buffered that needs flushing beyond what
    /// [`Stage::poll`] has not yet drained.
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

impl IngestSession for VdoIngestSession {
    /// Uninhabited: VDO capture never has anything of its own to send back —
    /// there is no connection to write handshake/keepalive requests onto (see
    /// the module doc's "why a bare `IngestSession`" section). A byte-stream
    /// scheme would set this to `bytes::Bytes` instead (see
    /// `multimux::source::rtsp::RtspIngestSession` for that shape).
    type Request = Infallible;
}
