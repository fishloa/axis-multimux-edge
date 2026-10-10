//! Maps stream names to running captures. Captures start on the first
//! request for a name, are shared by every name that resolves to the same
//! [`CaptureSettings`], count against `max_encodes`, and stop when idle
//! (see `sweep`) or when a config change unmaps them (see `apply`).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use utoipa::ToSchema;

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
    /// Runs under the registry lock: must not block (spawn only).
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
    pub fn new(
        config: Config,
        factory: Arc<dyn CaptureFactory>,
        profiles: Arc<dyn ProfileSource>,
    ) -> Self {
        Registry {
            inner: Mutex::new(Inner {
                config,
                captures: HashMap::new(),
            }),
            factory,
            profiles,
        }
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
        self.lock()
            .config
            .default_stream
            .clone()
            .unwrap_or_else(|| MAIN_NAME.to_string())
    }

    /// The router serving `name`, starting its capture if needed.
    pub async fn ensure(&self, name: &str, now: Instant) -> Result<axum::Router, ServeError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
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
                Target::Main => {
                    CaptureSettings::from_main(&config.main).map_err(ServeError::Unsupported)?
                }
                Target::Profile(p) => {
                    let list = self
                        .profiles
                        .list()
                        .await
                        .map_err(ServeError::ProfileUnavailable)?;
                    let cam = list
                        .iter()
                        .find(|c| &c.name == p)
                        .ok_or_else(|| ServeError::ProfileMissing(p.clone()))?;
                    parse_profile(&cam.parameters, &config.main)
                        .map_err(ServeError::Unsupported)?
                        .settings
                }
            };

            let mut inner = self.lock();
            // The mapping may have changed while we were resolving.
            if target_of(&inner.config, name).as_ref() != Some(&target) {
                return Err(ServeError::NotFound);
            }
            // `main` may have changed too; settings resolved from the old
            // snapshot are stale, so resolve again (bounded). If `main` changes
            // again during the 3rd attempt we proceed with that attempt's
            // settings; the next `apply` reconciles.
            if inner.config.main != config.main && attempts < 3 {
                continue;
            }
            if let Some(c) = inner.captures.get_mut(&settings) {
                c.names.insert(name.to_string());
                c.last_used = now;
                return Ok(c.router.clone());
            }
            // The VAPIX await above can take up to 5 s; stamp the new capture
            // with the insert time (never earlier than the caller's `now`) so
            // a short idle timeout cannot sweep it on the very next tick.
            let stamp = now.max(Instant::now());
            let max = inner.config.max_encodes;
            let in_use = own_encodes(&inner.captures);
            if in_use >= max {
                return Err(ServeError::EncoderBusy { in_use, max });
            }
            let cfg = &inner.config;
            let route = Arc::new(RouteHandle::new(
                cfg.target_duration_secs,
                cfg.part_target_ms,
                cfg.window_segments,
            ));
            let mut streams = HashMap::new();
            streams.insert(
                INNER_STREAM.to_string(),
                (route.clone(), vec![OutputKind::LlHls.build()]),
            );
            let router = router(Arc::new(AppState::new(streams)));
            let status = StatusHandle::new();
            let handle = self.factory.start(
                settings,
                route,
                status.clone(),
                cfg.window_segments,
                name.to_string(),
            );
            let mut names = BTreeSet::new();
            names.insert(name.to_string());
            inner.captures.insert(
                settings,
                Capture {
                    settings,
                    names,
                    router: router.clone(),
                    status,
                    started_at: stamp,
                    last_used: stamp,
                    _handle: handle,
                },
            );
            return Ok(router);
        }
    }

    /// Stop captures nobody has requested for `idle_timeout_secs`.
    pub fn sweep(&self, now: Instant) {
        let mut inner = self.lock();
        let timeout = std::time::Duration::from_secs(inner.config.idle_timeout_secs);
        inner
            .captures
            .retain(|_, c| now.saturating_duration_since(c.last_used) < timeout);
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
                    fps: if secs > 0.0 {
                        ((st.frames as f64 / secs) * 10.0).round() / 10.0
                    } else {
                        0.0
                    },
                    idle_secs: now.saturating_duration_since(c.last_used).as_secs(),
                    last_error: st.last_error,
                    shared_encode: st.shared_encode,
                }
            })
            .collect();
        streams.sort_by(|a, b| a.names.cmp(&b.names));
        RegistryStatus {
            encodes: EncodeUsage {
                in_use: own_encodes(&inner.captures),
                max: inner.config.max_encodes,
            },
            streams,
        }
    }
}

/// Captures running an encode of their own. One that joined an existing
/// encode costs the encoder nothing, so it doesn't count against
/// `max_encodes`.
fn own_encodes(captures: &HashMap<CaptureSettings, Capture>) -> u32 {
    captures
        .values()
        .filter(|c| !c.status.snapshot().shared_encode)
        .count() as u32
}

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
    /// Joined an encode the camera was already running; not counted in
    /// `encodes.in_use`.
    pub shared_encode: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RegistryStatus {
    pub encodes: EncodeUsage,
    pub streams: Vec<StreamStatus>,
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::config::StreamMapping;
    use crate::profile_source::{CameraProfile, StaticProfileSource};

    #[derive(Default)]
    pub(crate) struct Counts {
        pub started: AtomicUsize,
        pub stopped: AtomicUsize,
        /// New fake captures report that they joined an existing encode.
        pub share: std::sync::atomic::AtomicBool,
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
        fn start(
            &self,
            _: CaptureSettings,
            _: Arc<RouteHandle>,
            status: StatusHandle,
            _: usize,
            _: String,
        ) -> Box<dyn CaptureHandle> {
            self.0.started.fetch_add(1, Ordering::SeqCst);
            if self.0.share.load(Ordering::SeqCst) {
                status.set_shared_encode(true);
            }
            Box::new(FakeHandle(self.0.clone()))
        }
    }

    pub(crate) fn profile(name: &str, params: &str) -> CameraProfile {
        CameraProfile {
            name: name.into(),
            description: String::new(),
            parameters: params.into(),
        }
    }

    pub(crate) fn setup(
        streams: &[(&str, &str)],
    ) -> (Arc<Registry>, Arc<Counts>, Arc<StaticProfileSource>) {
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
                .map(|(n, p)| StreamMapping {
                    name: n.to_string(),
                    profile: p.to_string(),
                })
                .collect(),
            ..Config::default()
        };
        let reg = Arc::new(Registry::new(
            cfg,
            Arc::new(FakeFactory(counts.clone())),
            src.clone(),
        ));
        (reg, counts, src)
    }

    #[tokio::test]
    async fn unknown_name_is_not_found_and_starts_nothing() {
        let (reg, counts, _) = setup(&[("medium", "ACC_Medium")]);
        assert!(matches!(
            reg.ensure("nope", Instant::now()).await,
            Err(ServeError::NotFound)
        ));
        assert_eq!(counts.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn first_request_starts_once_then_reuses() {
        let (reg, counts, _) = setup(&[("medium", "ACC_Medium")]);
        let now = Instant::now();
        let _ = reg.ensure("medium", now).await.unwrap();
        let _ = reg.ensure("medium", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn main_is_always_available() {
        let (reg, counts, _) = setup(&[]);
        let _ = reg.ensure("main", Instant::now()).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn names_with_same_settings_share_one_capture() {
        let (reg, counts, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let now = Instant::now();
        let _ = reg.ensure("a", now).await.unwrap();
        let _ = reg.ensure("b", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn encode_cap_rejects_third_distinct_capture() {
        let (reg, counts, _) =
            setup(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        let _ = reg.ensure("hi", now).await.unwrap();
        let _ = reg.ensure("med", now).await.unwrap();
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
        assert!(
            matches!(reg.ensure("gone", now).await, Err(ServeError::ProfileMissing(p)) if p == "Deleted")
        );
        assert!(matches!(
            reg.ensure("mj", now).await,
            Err(ServeError::Unsupported(_))
        ));
        let _ = reg.ensure("med", now).await.unwrap();
        src.set(Err("VAPIX down".into()));
        // a running capture keeps serving without asking VAPIX again
        let _ = reg.ensure("med", now).await.unwrap();
        let (reg2, _, src2) = setup(&[("x", "ACC_Low")]);
        src2.set(Err("VAPIX down".into()));
        assert!(
            matches!(reg2.ensure("x", now).await, Err(ServeError::ProfileUnavailable(m)) if m == "VAPIX down")
        );
        let _ = reg2.ensure("main", now).await.unwrap();
    }

    /// Parks every `list()` call until released, so callers are provably
    /// between the fast-path check and the relock.
    struct GatedProfileSource {
        inner: StaticProfileSource,
        arrived: AtomicUsize,
        gate: tokio::sync::Notify,
    }
    impl crate::profile_source::ProfileSource for GatedProfileSource {
        fn list(&self) -> crate::profile_source::BoxFuture<'_, Result<Vec<CameraProfile>, String>> {
            Box::pin(async move {
                let notified = self.gate.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                self.arrived.fetch_add(1, Ordering::SeqCst);
                notified.await;
                self.inner.list().await
            })
        }
    }

    fn gated(
        streams: &[(&str, &str)],
        max: u32,
    ) -> (Arc<Registry>, Arc<Counts>, Arc<GatedProfileSource>) {
        let counts = Arc::new(Counts::default());
        let src = Arc::new(GatedProfileSource {
            inner: StaticProfileSource::new(vec![
                profile("ACC_High", "resolution=3840x2160&fps=25&videocodec=h264"),
                profile("ACC_Medium", "resolution=1280x720&fps=25&videocodec=h264"),
                profile("ACC_Low", "resolution=640x360&fps=5&videocodec=h264"),
                profile("NoRes", "videocodec=h264"),
            ]),
            arrived: AtomicUsize::new(0),
            gate: tokio::sync::Notify::new(),
        });
        let cfg = Config {
            max_encodes: max,
            streams: streams
                .iter()
                .map(|(n, p)| StreamMapping {
                    name: n.to_string(),
                    profile: p.to_string(),
                })
                .collect(),
            ..Config::default()
        };
        let reg = Arc::new(Registry::new(
            cfg,
            Arc::new(FakeFactory(counts.clone())),
            src.clone(),
        ));
        (reg, counts, src)
    }

    async fn wait_parked(src: &GatedProfileSource, n: usize) {
        let wait = async {
            while src.arrived.load(Ordering::SeqCst) < n {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .is_err()
        {
            panic!(
                "expected {n} callers parked in list(), got {}",
                src.arrived.load(Ordering::SeqCst)
            );
        }
    }

    async fn join(t: Joined) -> Result<(), ServeError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), t)
            .await
            .expect("ensure task did not finish in 5s")
            .unwrap()
    }

    type Joined = tokio::task::JoinHandle<Result<(), ServeError>>;
    fn spawn_ensure(reg: &Arc<Registry>, name: &'static str) -> Joined {
        let reg = reg.clone();
        tokio::spawn(async move { reg.ensure(name, Instant::now()).await.map(|_| ()) })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn gated_concurrent_first_requests_start_exactly_one_capture() {
        let (reg, counts, src) = gated(&[("medium", "ACC_Medium")], 2);
        let tasks: Vec<_> = (0..8).map(|_| spawn_ensure(&reg, "medium")).collect();
        wait_parked(&src, 8).await;
        src.gate.notify_waiters();
        for t in tasks {
            join(t).await.unwrap();
        }
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cap_race_for_last_slot_has_one_winner() {
        let (reg, counts, src) = gated(
            &[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")],
            2,
        );
        // occupy one slot
        let first = spawn_ensure(&reg, "hi");
        wait_parked(&src, 1).await;
        src.gate.notify_waiters();
        join(first).await.unwrap();
        let a = spawn_ensure(&reg, "med");
        let b = spawn_ensure(&reg, "lo");
        wait_parked(&src, 3).await;
        src.gate.notify_waiters();
        let (ra, rb) = (join(a).await, join(b).await);
        let busy = |r: &Result<(), ServeError>| {
            matches!(r, Err(ServeError::EncoderBusy { in_use: 2, max: 2 }))
        };
        assert!(ra.is_ok() != rb.is_ok(), "{ra:?} {rb:?}");
        assert!(busy(&ra) || busy(&rb));
        assert_eq!(counts.started.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remap_during_resolve_is_not_found_and_starts_nothing() {
        let (reg, counts, src) = gated(&[("med", "ACC_Medium")], 2);
        let t = spawn_ensure(&reg, "med");
        wait_parked(&src, 1).await;
        {
            let mut inner = reg.inner.lock().unwrap();
            let mut cfg = inner.config.clone();
            cfg.streams.clear();
            inner.config = cfg;
        }
        src.gate.notify_waiters();
        assert_eq!(join(t).await, Err(ServeError::NotFound));
        assert_eq!(counts.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn main_change_during_resolve_uses_new_main() {
        // "NoRes" leaves width/height to the main preset.
        let (reg, counts, src) = gated(&[("n", "NoRes")], 2);
        let t = spawn_ensure(&reg, "n");
        wait_parked(&src, 1).await;
        let new_width = {
            let mut inner = reg.inner.lock().unwrap();
            inner.config.main.width += 2;
            inner.config.main.width
        };
        src.gate.notify_waiters();
        // the retry parks in list() again
        wait_parked(&src, 2).await;
        src.gate.notify_waiters();
        join(t).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
        let inner = reg.inner.lock().unwrap();
        assert!(inner.captures.keys().all(|k| k.width == new_width));
    }

    #[tokio::test]
    async fn default_name_follows_config() {
        let (reg, _, _) = setup(&[("medium", "ACC_Medium")]);
        assert_eq!(reg.default_name(), "main");
    }

    #[tokio::test]
    async fn idle_capture_stops_after_timeout_and_frees_encode() {
        let (reg, counts, _) =
            setup(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let t0 = Instant::now();
        let _ = reg.ensure("hi", t0).await.unwrap();
        let _ = reg
            .ensure("med", t0 + Duration::from_secs(20))
            .await
            .unwrap();
        reg.sweep(t0 + Duration::from_secs(29));
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        // "hi" is stamped at insert time (>= t0), so use 31 s for a safe margin.
        reg.sweep(t0 + Duration::from_secs(31));
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1); // "hi" idle >= 30 s
        let _ = reg
            .ensure("lo", t0 + Duration::from_secs(32))
            .await
            .unwrap(); // slot freed
    }

    #[tokio::test]
    async fn every_request_keeps_a_capture_alive() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let t0 = Instant::now();
        for s in [0, 20, 40, 60] {
            let _ = reg
                .ensure("med", t0 + Duration::from_secs(s))
                .await
                .unwrap();
            reg.sweep(t0 + Duration::from_secs(s + 1));
        }
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn apply_keeps_unchanged_streams_running() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        let _ = reg.ensure("med", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams.retain(|s| s.name != "lo");
        cfg.default_stream = Some("med".into());
        cfg.max_encodes = 3;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
        let _ = reg.ensure("med", now).await.unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
        assert_eq!(reg.default_name(), "med");
    }

    #[tokio::test]
    async fn apply_stops_remapped_and_removed_streams() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        let now = Instant::now();
        let _ = reg.ensure("med", now).await.unwrap();
        let _ = reg.ensure("lo", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams = vec![StreamMapping {
            name: "med".into(),
            profile: "ACC_High".into(),
        }];
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 2);
        assert!(matches!(
            reg.ensure("lo", now).await,
            Err(ServeError::NotFound)
        ));
        let _ = reg.ensure("med", now).await.unwrap(); // restarts with ACC_High
        assert_eq!(counts.started.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn shared_capture_survives_while_one_name_still_maps_to_it() {
        let (reg, counts, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let now = Instant::now();
        let _ = reg.ensure("a", now).await.unwrap();
        let _ = reg.ensure("b", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.streams.retain(|s| s.name != "b");
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn apply_llhls_change_restarts_everything() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let now = Instant::now();
        let _ = reg.ensure("med", now).await.unwrap();
        let _ = reg.ensure("main", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.part_target_ms = 250;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn apply_main_change_restarts_only_main() {
        let (reg, counts, _) = setup(&[("med", "ACC_Medium")]);
        let now = Instant::now();
        let _ = reg.ensure("med", now).await.unwrap();
        let _ = reg.ensure("main", now).await.unwrap();
        let mut cfg = reg.config();
        cfg.main.framerate = 15;
        reg.apply(cfg);
        assert_eq!(counts.stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn snapshot_reports_encodes_and_streams() {
        let (reg, _, _) = setup(&[("a", "ACC_Medium"), ("b", "ACC_Medium")]);
        let t0 = Instant::now();
        let _ = reg.ensure("a", t0).await.unwrap();
        let _ = reg.ensure("b", t0).await.unwrap();
        let s = reg.snapshot(t0 + Duration::from_secs(3));
        assert_eq!((s.encodes.in_use, s.encodes.max), (1, 2));
        assert_eq!(s.streams.len(), 1);
        assert_eq!(s.streams[0].names, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(s.streams[0].settings, "h264 1280x720@25 ch1");
        assert_eq!(s.streams[0].state, "starting");
        assert_eq!(s.streams[0].idle_secs, 3);
    }

    #[tokio::test]
    async fn captures_sharing_an_existing_encode_do_not_count_against_the_cap() {
        let (reg, counts, _) = setup(&[("hi", "ACC_High"), ("med", "ACC_Medium")]);
        let mut cfg = reg.config();
        cfg.max_encodes = 1;
        reg.apply(cfg);
        counts.share.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        reg.ensure("hi", t0).await.unwrap();
        reg.ensure("med", t0).await.unwrap();
        let s = reg.snapshot(t0);
        assert_eq!((s.encodes.in_use, s.encodes.max), (0, 1));
        assert!(s.streams.iter().all(|st| st.shared_encode));
        // An own encode still counts, and the cap still applies to the next.
        counts.share.store(false, Ordering::SeqCst);
        reg.ensure("main", t0).await.unwrap();
        assert_eq!(reg.snapshot(t0).encodes.in_use, 1);
    }
}
