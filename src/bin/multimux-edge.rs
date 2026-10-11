//! ACAP entrypoint (`device`-gated; only builds inside the Axis ACAP Native
//! SDK sysroot). Wires the on-demand stream-profile -> LL-HLS pipeline together:
//!
//! - Loads [`multimux_edge::admin::Config`] from the ACAP
//!   `axparameter`-backed [`multimux_edge::admin::AxParameterStore`].
//! - Builds a [`multimux_edge::registry::Registry`] that maps operator-chosen
//!   stream names to camera stream profiles and starts a capture per distinct
//!   [`multimux_edge::profile::CaptureSettings`] on first request, stopping it
//!   when idle or unmapped. The registry starts captures through
//!   [`VdoCaptureFactory`], which drives
//!   [`multimux_edge::vdo_source::VdoIngestSession`] through
//!   [`multimux::supervise_driver`]/[`multimux::source::advance_route`] on a
//!   **dedicated OS thread with its own current-thread tokio runtime** —
//!   see "Threading" below.
//! - Serves the LL-HLS origin ([`multimux_edge::routing::hls_router`]) nested
//!   under `/hls`, merged with the admin config/status routes
//!   ([`multimux_edge::admin::admin_router`]), on `127.0.0.1:<port>` (matching
//!   `manifest.json`'s `reverseProxy` targets).
//!
//! # Threading
//!
//! [`VdoIngestSession::feed`](broadcast_common::Stage::feed) ultimately calls
//! `vdo::RunningStream::next_buffer`, a **blocking** FFI call into
//! `libvdo.so` that only returns once the camera has produced the next frame
//! (see `vdo_source.rs`'s module doc). Running that on an axum worker thread
//! would eventually starve every request being served on the same
//! `rt-multi-thread` runtime once all worker threads happen to be parked in
//! that blocking call. Instead each capture's whole capture/segment/store
//! pipeline runs on a plain `std::thread::spawn`'d OS thread with its own
//! `current_thread` tokio runtime — the blocking call only ever stalls that
//! one dedicated thread, never axum's.
//!
//! # Why `supervise_driver`/`advance_route`, not `run_pipeline`
//!
//! multimux 0.5 (issue #805) deleted `multimux::pipeline::{run_pipeline,
//! SampleSource}` outright once every input — including this app's VDO
//! capture, the last holdout — was ported onto the single
//! `media_plane::ingress` `Dialer`/`Listener` + `IngestSession` architecture.
//! [`run_vdo_capture`] is this app's own `attempt` closure (the same shape
//! `multimux::examples::custom_scheme`'s `run_demo` and every in-tree
//! `run_*` entry point use): [`multimux::supervise_driver`] calls it, retries
//! it with backoff on failure, and [`multimux::source::advance_route`] is
//! the one facade call inside it that both publishes the driver-minted
//! `Trunk` into the route and turns queued samples into servable segments.
//! One supervised capture runs per distinct settings, started by the
//! registry; dropping its [`CaptureHandle`] cancels the supervisor and sets
//! the stop flag so the capture loop returns and frees the VDO stream.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use broadcast_common::Timestamp;
use log::{error, info};
use media_plane::ingress::{HandshakePolicy, IngestDriver};
use media_plane::trunk::TrunkConfig;
use multimux::source::{DriverProgress, advance_route};
use multimux::{Backoff, MultimuxError, RouteHandle};
use multimux_edge::admin::{self, AxParameterStore, ConfigStore, StatusHandle};
use multimux_edge::profile::CaptureSettings;
use multimux_edge::profile_source::VapixProfileSource;
use multimux_edge::registry::{CaptureFactory, CaptureHandle, Registry};
use multimux_edge::routing;
use multimux_edge::vdo_source::VdoIngestSession;
use tokio_util::sync::CancellationToken;

/// The URL prefix AXIS OS's Apache reverse proxy forwards verbatim to this
/// app — `/local/<appName>` with `appName` from `manifest.json`
/// (`multimuxedge`). The proxy does not strip it, so every route is served
/// under this prefix. Keep in lockstep with `manifest.json`'s `setup.appName`.
const URL_PREFIX: &str = "/local/multimuxedge";

/// `Trunk` ring capacities for the VDO capture driver — this app's own
/// choice (mirroring `multimux::source::driver_trunk_config`'s production
/// sizing, which is `pub(crate)` and so not reusable directly by an external
/// crate like this one). `segment_capacity` is `Config::window_segments`
/// (the advertised LL-HLS window depth); the rest are generous fixed sizes
/// for a single-track video capture.
const DRIVER_TIMED_CAPACITY: usize = 64;
const DRIVER_SPARSE_CAPACITY: usize = 16;
const DRIVER_EVENT_CAPACITY: usize = 64;
const DRIVER_PART_CAPACITY: usize = 64;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    acap_logging::init_logger();
    info!("multimux-edge: starting");

    // A failed config backend is LOUD but NOT fatal.
    //
    // #955 correctly stopped this error being swallowed, but the first cut
    // exited on it — which turned a degraded backend into a crash loop. That
    // was observed for real on a camera: `runMode: "respawn"` restarted the
    // app roughly every 250ms, so it served nothing at all. Silently serving
    // on defaults was wrong; refusing to serve is worse. Run on defaults and
    // report the reason through `/admin/status`.
    let (store, store_open_error) = match AxParameterStore::new() {
        Ok(store) => (Arc::new(store), None),
        Err(e) => {
            error!("multimux-edge: axparameter store open failed, serving on defaults: {e}");
            (
                Arc::new(AxParameterStore::unavailable(e.to_string())),
                Some(format!("config backend unavailable: {e}")),
            )
        }
    };

    let status = StatusHandle::new();

    // Issue #955: a broken config backend used to be indistinguishable from
    // an unconfigured one — `load()` swallowed the error and handed back
    // `Config::default()` either way. `LoadOutcome::error` is `Some` only
    // for a genuinely broken backend (not "nothing stored yet"), so record
    // it on `status` immediately — before the config's own effects (codec,
    // port, …) are even applied — so `/admin/status`'s `last_error` shows
    // it from the very first request, not just after a `POST /admin/config`
    // round-trip. Uses `set_config_error`, not `set_last_error`: the capture
    // pipeline (started below, on its own thread) clears/sets
    // `last_error`'s pipeline slot on every retry attempt, which would
    // otherwise erase this within moments of boot.
    let outcome = store.load();
    if let Some(reason) = outcome.error() {
        error!("multimux-edge: config load failed, running on defaults: {reason}");
        status.set_config_error(Some(format!("config load: {reason}")));
    }
    let cfg = outcome.into_config();
    info!("multimux-edge: loaded config: {cfg:?}");

    let registry = Arc::new(Registry::new(
        cfg,
        Arc::new(VdoCaptureFactory),
        Arc::new(VapixProfileSource::new()),
    ));
    registry.spawn_sweeper();

    // AXIS OS's Apache reverse proxy forwards the FULL request path to the
    // target verbatim — it does NOT strip the `/local/<appName>/<apiPath>`
    // prefix (confirmed on hardware, #669, and matches Axis's own C/CivetWeb
    // and axum reverse-proxy examples, which register routes at the full
    // prefixed path). So the app must serve its routes under the real proxied
    // path: `/local/multimuxedge/hls/<stream>/…` and
    // `/local/multimuxedge/admin/…`. The origin's playlists use relative URIs
    // (`media.m3u8`, `seg-*.m4s`), which resolve correctly under the prefix.
    let inner = axum::Router::new()
        .nest("/hls", routing::hls_router(registry.clone()))
        .merge(admin::admin_router(store, status, registry));
    let app = axum::Router::new().nest(URL_PREFIX, inner);

    let bind_addr = format!("127.0.0.1:{}", multimux_edge::config::APP_PORT);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(e) => {
            error!("multimux-edge: failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        }
    };
    info!("multimux-edge: listening on {bind_addr}");

    if let Err(e) = axum::serve(listener, app).await {
        error!("multimux-edge: axum server error: {e}");
        std::process::exit(1);
    }
}

/// One VDO capture on its own OS thread + current-thread runtime (see the
/// module doc's "Threading" section). Dropping the handle stops it: the
/// capture loop checks `stop` after every frame and `supervise_driver`
/// is cancelled via its `CancellationToken`. Drop runs under the registry's
/// lock, so it only signals — it never joins the thread or blocks.
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

/// Starts one [`VdoCapture`] per distinct [`CaptureSettings`] on behalf of the
/// registry.
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
        info!(
            "multimux-edge: starting capture {label}: {}",
            settings.describe()
        );
        let thread_status = status.clone();
        let thread_name = format!("capture-{label}");
        let spawned = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let reason = format!("capture runtime build failed: {e}");
                        error!("multimux-edge: {reason}");
                        thread_status.set_last_error(Some(reason));
                        return;
                    }
                };
                rt.block_on(multimux::supervise_driver(
                    move |route_handle| {
                        let status = thread_status.clone();
                        let stop = thread_stop.clone();
                        async move {
                            run_vdo_capture(
                                &settings,
                                window_segments,
                                &status,
                                &route_handle,
                                &stop,
                            )
                            .await
                        }
                    },
                    route,
                    Backoff::production_default(),
                    label,
                    thread_cancel,
                ));
            });
        if let Err(e) = spawned {
            let reason = format!("capture thread spawn failed: {e}");
            error!("multimux-edge: {reason}");
            status.set_last_error(Some(reason));
        }
        Box::new(VdoCapture { stop, cancel })
    }
}

/// One VDO-capture attempt — the closure [`multimux::supervise_driver`]
/// retries with backoff. Opens the VDO channel
/// ([`VdoIngestSession::new`]), wraps it in an
/// [`media_plane::ingress::IngestDriver`] (no `Dialer`: see `vdo_source`'s
/// own doc for why VDO drives directly), then loops `feed`/`advance_route`
/// — each `feed` blocks on the next VDO buffer (see `vdo_source`'s
/// module doc's "Threading" section) — until `stop` is set (the capture was
/// dropped: idle or unmapped; returns `Ok(())`) or the session's health leaves
/// [`media_plane::ingress::HealthState::is_running`] (a VDO read/convert
/// failure; a live camera channel has no natural clean end).
async fn run_vdo_capture(
    settings: &CaptureSettings,
    window_segments: usize,
    status: &StatusHandle,
    route_handle: &RouteHandle,
    stop: &AtomicBool,
) -> multimux::Result<()> {
    // Checked before opening anything: a retry started after `Drop` set `stop`
    // must not open a new VDO stream / encode.
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let session = VdoIngestSession::new(settings, stop).map_err(|e| MultimuxError::Connect {
        reason: format!("VdoIngestSession init failed: {e}"),
    })?;
    status.set_shared_encode(session.shared_encode().is_some());

    let trunk_config = TrunkConfig::new(
        source_nz(DRIVER_TIMED_CAPACITY),
        source_nz(DRIVER_SPARSE_CAPACITY),
        source_nz(window_segments),
        source_nz(DRIVER_EVENT_CAPACITY),
        source_nz(DRIVER_PART_CAPACITY),
    );
    // VDO capture has no network handshake to bound: `VdoIngestSession::new`
    // already resolved everything synchronously, so `Established` is queued
    // before the very first `feed()` call even runs and is drained (promoting
    // out of `Establishing`) before this driver's handshake deadline is ever
    // checked — see `vdo_source`'s own module doc. `u64::MAX` documents "this
    // deadline is unreachable in practice" rather than picking an arbitrary
    // real timeout for a step that can't actually time out.
    let handshake = HandshakePolicy::establish_by(Timestamp::from_nanos(u64::MAX));
    let mut driver = IngestDriver::new(
        session,
        trunk_config,
        handshake,
        media_plane::DEFAULT_MAX_PROGRAMS,
    );
    let mut progress = DriverProgress::new();
    let start = Instant::now();

    status.set_running(true);
    status.set_last_error(None);

    loop {
        if stop.load(Ordering::Relaxed) {
            info!("multimux-edge: capture stopped (idle or unmapped)");
            break;
        }
        let now = Timestamp::from_instant(start, Instant::now());
        driver.feed((), now);
        advance_route(&driver, route_handle, &mut progress).await;
        // Issue #955: `StatusHandle` was never touched by the pipeline, so
        // `/admin/status` reported `current_segment`/`current_part`/`frames`
        // as permanent zeros while segments were being served correctly —
        // confirmed on-device (`#EXT-X-MEDIA-SEQUENCE` climbing while
        // `/admin/status` stood still). One VDO buffer is processed per
        // `feed()` call (see this function's own module doc), so counting
        // one here is the natural unit for "frames processed". The
        // segment/part position comes straight from this program's `Trunk`
        // — `last_closed_segment` names the newest *closed* segment, so the
        // one "currently being written" (this field's documented meaning)
        // is one past it, or `0` before anything has closed yet; the part
        // count is how many parts that open segment has accumulated so far.
        status.add_frames(1);
        if let Some(program) = driver.programs().next() {
            if let Some(trunk) = driver.trunk(program) {
                let current_segment = trunk.last_closed_segment().map_or(0, |seq| seq + 1);
                let current_part = trunk.parts_in_segment(current_segment).len() as u32;
                status.set_position(current_segment, current_part);
            }
        }
        if !driver.health().is_running() {
            break;
        }
    }

    status.set_running(false);
    match driver.into_health() {
        media_plane::ingress::HealthState::Failed(e) => {
            let reason = e.to_string();
            status.set_last_error(Some(reason.clone()));
            error!("multimux-edge: VDO capture ended: {reason}");
            Err(MultimuxError::Connect { reason })
        }
        // A live camera channel has no natural clean end, but `Stage::finish`
        // is never called by this loop either, so this arm is unreached in
        // practice; treated as a clean stop rather than manufacturing an
        // error for a state this driver never actually produces on its own.
        _ => Ok(()),
    }
}

/// `NonZeroUsize::new(n).unwrap_or(MIN)` — every capacity this module passes
/// to [`TrunkConfig::new`] is a fixed positive constant except
/// `window_segments`, which `Config::validate` (`src/admin.rs`) already
/// rejects as `0` via the admin API; this is a second, structural backstop
/// (degrading to capacity 1 rather than panicking) for that one
/// caller-configurable value, not the primary guard.
fn source_nz(n: usize) -> std::num::NonZeroUsize {
    std::num::NonZeroUsize::new(n).unwrap_or(std::num::NonZeroUsize::MIN)
}
