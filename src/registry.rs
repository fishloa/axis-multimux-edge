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
    #[allow(dead_code)] // read by the reaper / status endpoints (Task 5)
    pub(crate) settings: CaptureSettings,
    pub(crate) names: BTreeSet<String>,
    pub(crate) router: axum::Router,
    #[allow(dead_code)] // Task 5
    pub(crate) status: StatusHandle,
    #[allow(dead_code)] // Task 5
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
            // snapshot are stale, so resolve again (bounded).
            if inner.config.main != config.main && attempts < 3 {
                continue;
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
                    started_at: now,
                    last_used: now,
                    _handle: handle,
                },
            );
            return Ok(router);
        }
    }
}

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
        fn start(
            &self,
            _: CaptureSettings,
            _: Arc<RouteHandle>,
            _: StatusHandle,
            _: usize,
            _: String,
        ) -> Box<dyn CaptureHandle> {
            self.0.started.fetch_add(1, Ordering::SeqCst);
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
        while src.arrived.load(Ordering::SeqCst) < n {
            tokio::task::yield_now().await;
        }
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
            t.await.unwrap().unwrap();
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
        first.await.unwrap().unwrap();
        let a = spawn_ensure(&reg, "med");
        let b = spawn_ensure(&reg, "lo");
        wait_parked(&src, 3).await;
        src.gate.notify_waiters();
        let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
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
        assert_eq!(t.await.unwrap(), Err(ServeError::NotFound));
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
        t.await.unwrap().unwrap();
        assert_eq!(counts.started.load(Ordering::SeqCst), 1);
        let inner = reg.inner.lock().unwrap();
        assert!(inner.captures.keys().all(|k| k.width == new_width));
    }

    #[tokio::test]
    async fn default_name_follows_config() {
        let (reg, _, _) = setup(&[("medium", "ACC_Medium")]);
        assert_eq!(reg.default_name(), "main");
    }
}
