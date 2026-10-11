//! Admin config + status HTTP routes for multimux-edge.
//!
//! `GET /admin/config` and `POST /admin/config` read/update the app's
//! [`Config`] through a pluggable [`ConfigStore`]: [`DefaultStore`] (host
//! builds, and the device fallback) always round-trips `Config::default()`;
//! `#[cfg(feature = "device")]` `AxParameterStore` persists it via the ACAP
//! `axparameter` parameter store. `GET /admin/status` reports the running
//! pipeline's [`Status`] through a shared [`StatusHandle`] the pipeline
//! updates as it runs.
//!
//! The routes and `Config` (de)serialization are plain std + serde + axum,
//! so this whole module — including its tests — builds and runs on the host;
//! only `AxParameterStore` is device-gated.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::FieldError;
use crate::registry::{Registry, RegistryStatus};

pub use crate::config::Config;

/// Outcome of [`ConfigStore::load`] — distinguishes "nothing has been
/// stored yet" from "the backend itself is broken". Issue #955: the old
/// `load()` discarded the backend's error and returned
/// [`Config::default`] either way, so an axparameter store that had never
/// worked on any camera looked byte-for-byte identical to one nobody had
/// configured yet. A caller that only wants "the config to run with" should
/// use [`LoadOutcome::into_config`]; a caller that also wants to know
/// whether the store is actually broken (to surface e.g. via
/// `/admin/status`'s `last_error`) should check [`LoadOutcome::error`]
/// first.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LoadOutcome {
    /// A previously stored config was loaded and parsed successfully.
    Stored(Config),
    /// Nothing has been stored yet (a fresh install); the caller should use
    /// [`Config::default`].
    Unset,
    /// The backend failed to produce a config — a `get`/`add` failure or a
    /// stored value that didn't parse — carrying the failure reason. This is
    /// NOT equivalent to [`LoadOutcome::Unset`]: it means the store is
    /// broken, not merely empty.
    Broken(String),
}

impl LoadOutcome {
    /// The config to actually run with: the stored value for
    /// [`LoadOutcome::Stored`], or [`Config::default`] for
    /// [`LoadOutcome::Unset`]/[`LoadOutcome::Broken`] — a broken backend
    /// still needs *some* config to boot with, but callers that discard the
    /// distinction here should also check [`LoadOutcome::error`].
    pub fn into_config(self) -> Config {
        match self {
            LoadOutcome::Stored(c) => c,
            LoadOutcome::Unset | LoadOutcome::Broken(_) => Config::default(),
        }
    }

    /// The failure reason, if and only if the backend is broken (not just
    /// unset).
    pub fn error(&self) -> Option<&str> {
        match self {
            LoadOutcome::Broken(reason) => Some(reason),
            LoadOutcome::Stored(_) | LoadOutcome::Unset => None,
        }
    }
}

/// Interpret the raw stored config string. Empty, or the lone `{` that
/// libaxparameter's truncated first-run `add` used to leave behind, means
/// nothing real was ever stored ([`LoadOutcome::Unset`]), not a broken
/// backend; anything else must parse.
#[cfg_attr(not(feature = "device"), allow(dead_code))] // only the device store calls it
pub(crate) fn parse_stored(s: &str) -> LoadOutcome {
    match s.trim() {
        "" | "{" => LoadOutcome::Unset,
        json => match serde_json::from_str(json) {
            Ok(cfg) => LoadOutcome::Stored(cfg),
            Err(e) => LoadOutcome::Broken(format!("stored config is not valid JSON: {e}")),
        },
    }
}

/// Loads and persists [`Config`]. Host builds use [`DefaultStore`]; device
/// builds use `#[cfg(feature = "device")]` `AxParameterStore`.
pub trait ConfigStore: Send + Sync + 'static {
    /// Load the current config. See [`LoadOutcome`] — "nothing stored yet"
    /// and "the backend is broken" are distinct outcomes, both of which
    /// currently run on [`Config::default`], but only the latter is a real
    /// failure worth surfacing.
    fn load(&self) -> LoadOutcome;
    /// Persist `c` as the new config.
    fn store(&self, c: &Config) -> crate::Result<()>;
}

/// Host + fallback [`ConfigStore`]: `load` always returns
/// [`LoadOutcome::Unset`] (there genuinely is no backend, so "nothing
/// stored" is accurate, not a masked failure), `store` is a no-op. Used on
/// host builds (including these tests) and as a device fallback before
/// axparameter is wired up.
pub struct DefaultStore;

impl ConfigStore for DefaultStore {
    fn load(&self) -> LoadOutcome {
        LoadOutcome::Unset
    }

    fn store(&self, _c: &Config) -> crate::Result<()> {
        Ok(())
    }
}

/// ACAP `axparameter`-backed [`ConfigStore`]: round-trips the whole [`Config`]
/// as one JSON string parameter on the app's `axparameter::Parameter` handle.
/// Device builds only — `axparameter` is an optional, `device`-feature-gated
/// dependency (see `Cargo.toml`).
#[cfg(feature = "device")]
pub struct AxParameterStore {
    /// `None` when the backend could not be opened at all — the store then
    /// reports [`LoadOutcome::Broken`] and refuses writes, instead of the
    /// process refusing to start. See `unavailable`.
    inner: Option<axparameter::parameter::Parameter>,
    /// Why the backend is unavailable, when `inner` is `None`.
    open_error: Option<String>,
}

/// The ACAP `appName` from `manifest.json`, which libaxparameter uses to
/// locate `/etc/dynamic/param/<appName>.conf`.
///
/// This MUST match `manifest.json`'s `appName` exactly. It is **not**
/// `"multimux-edge"`: ACAP rejects a hyphen in `appName` (fixed in #669),
/// and this string was missed at the time. Passing the hyphenated form makes
/// libaxparameter look for a `.conf` that does not exist, so every `add`/`get`
/// fails with "Failed to get real path for symlink
/// /etc/dynamic/param/multimux-edge.conf" — which is exactly how #955
/// presented on a real camera.
#[cfg(feature = "device")]
pub const ACAP_APP_NAME: &str = "multimuxedge";

#[cfg(feature = "device")]
impl AxParameterStore {
    /// The single axparameter parameter name the whole [`Config`] is
    /// serialized under (as JSON).
    const PARAM_NAME: &'static str = "Config";

    /// Open the `multimux-edge` axparameter handle, creating the `Config`
    /// parameter if this is the first run on this camera.
    ///
    /// Issue #955: `store` called `Parameter::set("Config", …)` on a
    /// parameter that was never `add`ed, so persisting a config failed on
    /// every camera (`axparameter set: Failed to set parameter Config`) —
    /// confirmed on-device: `param.cgi?action=list&group=multimux-edge`
    /// returned "Error -1 getting param in group". [`Self::ensure_parameter`]
    /// registers the parameter (with [`Config::default`] as its initial
    /// value) exactly once per camera, then every subsequent `new()` finds
    /// it already there.
    pub fn new() -> crate::Result<Self> {
        let inner = axparameter::parameter::Parameter::new(ACAP_APP_NAME)
            .map_err(|e| crate::OriginError::Config(format!("axparameter open: {e}")))?;
        let store = AxParameterStore {
            inner: Some(inner),
            open_error: None,
        };
        store.ensure_parameter()?;
        Ok(store)
    }

    /// A store whose backend could not be opened.
    ///
    /// Every read reports [`LoadOutcome::Broken`] and every write fails, but
    /// the app still starts and serves media on [`Config::default`]. Exiting
    /// instead produced a respawn loop on a real camera (#955).
    #[must_use]
    pub fn unavailable(reason: String) -> Self {
        AxParameterStore {
            inner: None,
            open_error: Some(reason),
        }
    }

    /// Register [`Self::PARAM_NAME`] via `axparameter::Parameter::add` if it
    /// doesn't already exist.
    ///
    /// Idempotent by design, not just by accident: a second app start (every
    /// restart, forever, since `runMode: "respawn"` in `manifest.json`) must
    /// not fail just because the parameter is now there. `add` reports that
    /// case as `ParameterError::ParamAdded` ("already added") — matching the
    /// vendored `axparameter_example` app's own `add`-then-ignore-ParamAdded
    /// pattern — so that specific error is swallowed; any other error means
    /// the backend itself is broken and is propagated.
    ///
    /// The parameter is added with an empty value and the real default is
    /// then written with `set`: libaxparameter truncates an `add` initial
    /// value at its first `"`, so adding the JSON directly stored just `{`
    /// (observed on AXIS OS 11.11, 2026-10-02). `set` escapes correctly.
    fn ensure_parameter(&self) -> crate::Result<()> {
        let initial = serde_json::to_string(&Config::default())
            .map_err(|e| crate::OriginError::Config(format!("config serialize: {e}")))?;
        let Some(inner) = self.inner.as_ref() else {
            return Ok(());
        };
        match inner.add(Self::PARAM_NAME, None, String::new()) {
            Ok(()) => inner
                .set(Self::PARAM_NAME, initial, true)
                .map_err(|e| crate::OriginError::Config(format!("axparameter set initial: {e}"))),
            Err(e)
                if e.matches::<axparameter::error::ParameterError>(
                    axparameter::error::ParameterError::ParamAdded,
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(crate::OriginError::Config(format!("axparameter add: {e}"))),
        }
    }
}

#[cfg(feature = "device")]
impl ConfigStore for AxParameterStore {
    fn load(&self) -> LoadOutcome {
        // `ensure_parameter` (run once in `new()`) guarantees the parameter
        // exists by the time `load` can be called on a live
        // `AxParameterStore`, so a `get` failure here means the backend is
        // broken (dbus/file-level failure), never "nothing stored yet". An
        // empty or truncated (`{`) stored value is `Unset` (see
        // `parse_stored`): a crash between `add` and `set`, or a camera
        // installed before the first-run truncation fix.
        let Some(inner) = self.inner.as_ref() else {
            return LoadOutcome::Broken(
                self.open_error
                    .clone()
                    .unwrap_or_else(|| "config backend unavailable".to_string()),
            );
        };
        match inner.get::<String>(Self::PARAM_NAME) {
            Ok(s) => parse_stored(&s),
            Err(e) => LoadOutcome::Broken(format!("axparameter get: {e}")),
        }
    }

    fn store(&self, c: &Config) -> crate::Result<()> {
        let s = serde_json::to_string(c)
            .map_err(|e| crate::OriginError::Config(format!("config serialize: {e}")))?;
        let Some(inner) = self.inner.as_ref() else {
            return Err(crate::OriginError::Config(
                self.open_error
                    .clone()
                    .unwrap_or_else(|| "config backend unavailable".to_string()),
            ));
        };
        inner
            .set(Self::PARAM_NAME, s, true)
            .map_err(|e| crate::OriginError::Config(format!("axparameter set: {e}")))
    }
}

/// Live pipeline status, updated by the running pipeline and read by
/// `GET /admin/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    /// Whether the capture/mux pipeline is currently running.
    pub running: bool,
    /// The LL-HLS media sequence number currently being written.
    pub current_segment: u32,
    /// The part index within `current_segment` currently being written.
    pub current_part: u32,
    /// Total frames processed since the pipeline started.
    pub frames: u64,
    /// The most recent pipeline error, if any, as its `Display` text.
    pub last_error: Option<String>,
    /// How the capture's encode relates to the camera's other clients.
    pub encode: EncodeShare,
}

/// How a capture's encode relates to the camera's other clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodeShare {
    /// Not open yet. `joinable`: expected to join an encode the camera
    /// already runs.
    Pending { joinable: bool },
    /// An encode of its own.
    Own,
    /// Joined the camera's encode `id`, which has `peers` clients in all
    /// (0 = not reported).
    Joined { id: u32, peers: u32 },
}

impl Default for EncodeShare {
    fn default() -> Self {
        EncodeShare::Pending { joinable: false }
    }
}

/// Internal state behind [`StatusHandle`]: the served [`Status`] plus a
/// config-backend error tracked in a slot separate from the pipeline's own
/// `last_error`. Kept apart so the capture pipeline's routine
/// "clear the previous attempt's error at the start of a new one"
/// (`run_vdo_capture` in the `multimux-edge` binary) can never silently wipe
/// out evidence that the config store itself is broken — exactly the kind
/// of masked failure issue #955 diagnosed in `ConfigStore::load` itself.
#[derive(Default)]
struct StatusState {
    status: Status,
    config_error: Option<String>,
}

/// Shared, cloneable handle to a [`Status`], read by the admin routes and
/// updated by the running pipeline.
#[derive(Clone)]
pub struct StatusHandle(Arc<Mutex<StatusState>>);

impl StatusHandle {
    /// A fresh handle around [`Status::default`] (not running, no frames).
    pub fn new() -> Self {
        StatusHandle(Arc::new(Mutex::new(StatusState::default())))
    }

    /// The current status, cloned out from behind the lock. `last_error`
    /// prefers the pipeline's own error (something actionable is failing
    /// right now); if the pipeline currently reports none, it falls back to
    /// the persistent config-backend error set via
    /// [`StatusHandle::set_config_error`] — so a broken config store stays
    /// visible even while the capture pipeline itself is running cleanly on
    /// defaults.
    pub fn snapshot(&self) -> Status {
        let state = self.0.lock().expect("status mutex poisoned");
        let mut status = state.status.clone();
        if status.last_error.is_none() {
            status.last_error = state.config_error.clone();
        }
        status
    }

    /// Mark the pipeline as running or stopped.
    pub fn set_running(&self, running: bool) {
        self.0.lock().expect("status mutex poisoned").status.running = running;
    }

    /// Update the current segment/part position.
    pub fn set_position(&self, current_segment: u32, current_part: u32) {
        let mut state = self.0.lock().expect("status mutex poisoned");
        state.status.current_segment = current_segment;
        state.status.current_part = current_part;
    }

    /// Record how the capture's encode relates to the camera's other clients.
    pub fn set_encode(&self, encode: EncodeShare) {
        self.0.lock().expect("status mutex poisoned").status.encode = encode;
    }

    /// Add `n` to the processed-frame counter.
    pub fn add_frames(&self, n: u64) {
        self.0.lock().expect("status mutex poisoned").status.frames += n;
    }

    /// Record (or clear) the pipeline's own most recent error. This is the
    /// pipeline's slot, not the config store's — see
    /// [`StatusHandle::set_config_error`] for why the two are kept apart.
    pub fn set_last_error(&self, err: Option<String>) {
        self.0
            .lock()
            .expect("status mutex poisoned")
            .status
            .last_error = err;
    }

    /// Record (or clear) a config-backend error (issue #955:
    /// [`ConfigStore::load`]'s [`LoadOutcome::error`]) in a slot the capture
    /// pipeline never touches, so it survives every pipeline retry's own
    /// `set_last_error(None)`/`set_last_error(Some(..))` churn. There is no
    /// live path that clears this today — the config store is only loaded
    /// once at boot — matching the honest state of a `respawn`-mode ACAP
    /// app: fixing the backend requires a restart anyway.
    pub fn set_config_error(&self, err: Option<String>) {
        self.0.lock().expect("status mutex poisoned").config_error = err;
    }
}

impl Default for StatusHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Response of `GET /admin/status`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminStatus {
    /// The config-backend or pipeline error, if any.
    pub last_error: Option<String>,
    #[serde(flatten)]
    pub registry: RegistryStatus,
}

/// One camera stream profile and what this app would capture for it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileView {
    pub name: String,
    pub description: String,
    /// Raw `key=value&...` parameter string of the camera profile.
    pub parameters: String,
    /// Human-readable capture settings, when the profile is usable.
    pub settings: Option<String>,
    pub ignored_keys: Vec<String>,
    /// Why the profile cannot be captured, if so.
    pub error: Option<String>,
}

/// Response of `GET /admin/profiles`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfilesResponse {
    pub profiles: Vec<ProfileView>,
    /// Set when the camera's profile list could not be read.
    pub error: Option<String>,
}

/// 400 body of `POST /admin/config`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ValidationErrors {
    pub errors: Vec<FieldError>,
}

/// 200 body of `POST /admin/config`.
#[derive(Debug, Serialize, ToSchema)]
pub struct Applied {
    /// Always `applied`.
    pub status: String,
}

/// Admin router state.
pub(crate) struct AdminState<S: ConfigStore> {
    store: Arc<S>,
    app_status: StatusHandle,
    registry: Arc<Registry>,
    /// Serialises store-then-apply so concurrent POSTs cannot interleave.
    apply_lock: Arc<tokio::sync::Mutex<()>>,
}

// Manual `Clone` so cloning never requires `S: Clone`.
impl<S: ConfigStore> Clone for AdminState<S> {
    fn clone(&self) -> Self {
        AdminState {
            store: Arc::clone(&self.store),
            app_status: self.app_status.clone(),
            registry: Arc::clone(&self.registry),
            apply_lock: Arc::clone(&self.apply_lock),
        }
    }
}

/// Build the admin router. Fully applies its state, so the returned
/// [`Router`] merges directly with `multimux::origin::router`'s.
pub fn admin_router<S: ConfigStore>(
    store: Arc<S>,
    app_status: StatusHandle,
    registry: Arc<Registry>,
) -> Router {
    let state = AdminState {
        store,
        app_status,
        registry,
        apply_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    Router::new()
        .route("/admin/config", get(get_config::<S>).post(post_config::<S>))
        .route("/admin/status", get(get_status::<S>))
        .route("/admin/profiles", get(get_profiles::<S>))
        .route("/admin/openapi.json", get(get_openapi))
        .with_state(state)
}

/// Current configuration.
#[utoipa::path(get, path = "/admin/config", tag = "admin", responses((status = 200, description = "Current configuration", body = Config)))]
pub(crate) async fn get_config<S: ConfigStore>(State(state): State<AdminState<S>>) -> Json<Config> {
    let outcome = state.store.load();
    // A broken backend is real evidence (issue #955): surface it through
    // `/admin/status`'s `last_error` in the config-specific slot.
    if let Some(reason) = outcome.error() {
        state
            .app_status
            .set_config_error(Some(format!("config load: {reason}")));
    }
    Json(outcome.into_config())
}

/// Validate, store and apply a configuration. Applies immediately.
#[utoipa::path(post, path = "/admin/config", tag = "admin", request_body = Config, responses(
    (status = 200, description = "Stored and applied", body = Applied),
    (status = 400, description = "Validation failed; nothing stored or applied", body = ValidationErrors),
    (status = 500, description = "config store failed", body = String),
))]
pub(crate) async fn post_config<S: ConfigStore>(
    State(state): State<AdminState<S>>,
    Json(cfg): Json<Config>,
) -> Response {
    if let Err(errors) = cfg.validate() {
        return (StatusCode::BAD_REQUEST, Json(ValidationErrors { errors })).into_response();
    }
    let _guard = state.apply_lock.lock().await;
    if let Err(e) = state.store.store(&cfg) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    state.registry.apply(cfg);
    Json(Applied {
        status: "applied".into(),
    })
    .into_response()
}

/// Encoder usage and every running capture.
#[utoipa::path(get, path = "/admin/status", tag = "admin", responses((status = 200, description = "Encoder usage and captures", body = AdminStatus)))]
pub(crate) async fn get_status<S: ConfigStore>(
    State(state): State<AdminState<S>>,
) -> Json<AdminStatus> {
    Json(AdminStatus {
        last_error: state.app_status.snapshot().last_error,
        registry: state.registry.snapshot(std::time::Instant::now()),
    })
}

/// The camera's stream profiles and what this app would capture for each.
#[utoipa::path(get, path = "/admin/profiles", tag = "admin", responses((status = 200, description = "Camera stream profiles", body = ProfilesResponse)))]
pub(crate) async fn get_profiles<S: ConfigStore>(
    State(state): State<AdminState<S>>,
) -> Json<ProfilesResponse> {
    let main = state.registry.config().main;
    match state.registry.profiles().list().await {
        Ok(list) => Json(ProfilesResponse {
            profiles: list
                .into_iter()
                .map(|p| {
                    let parsed = crate::profile::parse_profile(&p.parameters, &main);
                    ProfileView {
                        settings: parsed.as_ref().ok().map(|x| x.settings.describe()),
                        ignored_keys: parsed
                            .as_ref()
                            .map(|x| x.ignored_keys.clone())
                            .unwrap_or_default(),
                        error: parsed.err(),
                        name: p.name,
                        description: p.description,
                        parameters: p.parameters,
                    }
                })
                .collect(),
            error: None,
        }),
        Err(e) => Json(ProfilesResponse {
            profiles: Vec::new(),
            error: Some(e),
        }),
    }
}

/// This API's OpenAPI 3 description.
#[utoipa::path(get, path = "/admin/openapi.json", tag = "admin", responses((status = 200, description = "OpenAPI document", content_type = "application/json")))]
pub(crate) async fn get_openapi() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        crate::openapi::openapi_json(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    use super::*;

    use crate::registry::tests::setup;

    fn router_with(streams: &[(&str, &str)]) -> (Router, Arc<crate::registry::Registry>) {
        let (reg, _, _) = setup(streams);
        (
            admin_router(Arc::new(DefaultStore), StatusHandle::new(), reg.clone()),
            reg,
        )
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
        let cfg = Config {
            streams: vec![crate::config::StreamMapping {
                name: "med".into(),
                profile: "ACC_Medium".into(),
            }],
            default_stream: Some("med".into()),
            ..Config::default()
        };
        let r = post_json(app, "/admin/config", &serde_json::to_value(&cfg).unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(body_json(r).await["status"], "applied");
        assert_eq!(reg.default_name(), "med");
    }

    #[tokio::test]
    async fn post_config_invalid_returns_field_errors_and_applies_nothing() {
        let (app, reg) = router_with(&[]);
        let cfg = Config {
            streams: vec![crate::config::StreamMapping {
                name: "Bad Name".into(),
                profile: "P".into(),
            }],
            ..Config::default()
        };
        let r = post_json(app, "/admin/config", &serde_json::to_value(&cfg).unwrap()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(r).await["errors"][0]["field"], "streams[0].name");
        assert!(reg.config().streams.is_empty());
    }

    #[tokio::test]
    async fn profiles_lists_camera_profiles_with_parsed_settings() {
        let app = router();
        let r = app
            .oneshot(
                Request::builder()
                    .uri("/admin/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        let med = v["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "ACC_Medium")
            .unwrap();
        assert_eq!(med["settings"], "h264 1280x720@25 ch1");
        let mj = v["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "MJPEG")
            .unwrap();
        assert!(mj["error"].as_str().unwrap().contains("jpeg"));
        assert!(v["error"].is_null());
    }

    #[tokio::test]
    async fn status_has_encodes_and_streams() {
        let (app, reg) = router_with(&[("med", "ACC_Medium")]);
        let _ = reg.ensure("med", std::time::Instant::now()).await.unwrap();
        let r = app
            .oneshot(
                Request::builder()
                    .uri("/admin/status")
                    .body(Body::empty())
                    .unwrap(),
            )
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
            .oneshot(
                Request::builder()
                    .uri("/admin/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        for p in [
            "/admin/config",
            "/admin/status",
            "/admin/profiles",
            "/admin/openapi.json",
        ] {
            assert!(v["paths"][p].is_object(), "missing {p}");
        }
        assert_eq!(v["servers"][0]["url"], "/local/multimuxedge");
    }

    struct FailingStore;

    impl ConfigStore for FailingStore {
        fn load(&self) -> LoadOutcome {
            LoadOutcome::Unset
        }

        fn store(&self, _c: &Config) -> crate::Result<()> {
            Err(crate::OriginError::Config("disk full".into()))
        }
    }

    #[tokio::test]
    async fn post_config_store_failure_returns_500_and_does_not_apply() {
        let (_, reg) = router_with(&[]);
        let app = admin_router(Arc::new(FailingStore), StatusHandle::new(), reg.clone());
        let cfg = Config {
            streams: vec![crate::config::StreamMapping {
                name: "med".into(),
                profile: "ACC_Medium".into(),
            }],
            ..Config::default()
        };
        let before = reg.config();
        let r = post_json(app, "/admin/config", &serde_json::to_value(&cfg).unwrap()).await;
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(reg.config(), before);
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("parse json body")
    }

    #[tokio::test]
    async fn get_config_returns_defaults() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/admin/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let cfg: Config = serde_json::from_value(body_json(response).await).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[tokio::test]
    async fn post_config_valid_returns_200() {
        let cfg = Config {
            main: crate::config::MainPreset {
                codec: "h265".into(),
                ..Default::default()
            },
            ..Config::default()
        };

        let response = router()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/config")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&cfg).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_config_invalid_codec_returns_400() {
        let cfg = Config {
            main: crate::config::MainPreset {
                codec: "vp9".into(),
                ..Default::default()
            },
            ..Config::default()
        };

        let response = router()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/config")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&cfg).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_config_invalid_window_returns_400() {
        let cfg = Config {
            window_segments: 0,
            ..Config::default()
        };

        let response = router()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/admin/config")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&cfg).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_stored_treats_empty_and_truncated_as_unset() {
        assert_eq!(parse_stored(""), LoadOutcome::Unset);
        assert_eq!(parse_stored("  "), LoadOutcome::Unset);
        assert_eq!(parse_stored("{"), LoadOutcome::Unset);
    }

    #[test]
    fn parse_stored_parses_real_config_and_flags_garbage() {
        let cfg = Config {
            main: crate::config::MainPreset {
                codec: "h265".into(),
                ..Default::default()
            },
            ..Config::default()
        };
        let s = serde_json::to_string(&cfg).unwrap();
        assert_eq!(parse_stored(&s), LoadOutcome::Stored(cfg));
        assert!(matches!(
            parse_stored("{\"channel\":"),
            LoadOutcome::Broken(_)
        ));
    }

    #[test]
    fn load_outcome_unset_and_broken_both_fall_back_to_default_config() {
        assert_eq!(LoadOutcome::Unset.into_config(), Config::default());
        assert_eq!(
            LoadOutcome::Broken("simulated".to_string()).into_config(),
            Config::default()
        );
    }

    #[test]
    fn load_outcome_error_distinguishes_broken_from_unset_and_stored() {
        assert_eq!(LoadOutcome::Unset.error(), None);
        assert_eq!(LoadOutcome::Stored(Config::default()).error(), None);
        assert_eq!(
            LoadOutcome::Broken("axparameter get: boom".to_string()).error(),
            Some("axparameter get: boom")
        );
    }

    /// A [`ConfigStore`] test double standing in for a broken axparameter
    /// backend (issue #955): `load` always reports [`LoadOutcome::Broken`],
    /// the same shape `AxParameterStore::load` now returns instead of
    /// silently falling back to defaults.
    struct BrokenStore;

    impl ConfigStore for BrokenStore {
        fn load(&self) -> LoadOutcome {
            LoadOutcome::Broken("axparameter get: simulated backend failure".to_string())
        }

        fn store(&self, _c: &Config) -> crate::Result<()> {
            Ok(())
        }
    }

    /// A [`ConfigStore`] test double for a backend that has a real stored
    /// value, distinguishing [`LoadOutcome::Stored`] from the
    /// default-shaped [`LoadOutcome::Unset`]/[`LoadOutcome::Broken`] cases
    /// `DefaultStore`/`BrokenStore` cover.
    struct StoredStore;

    impl ConfigStore for StoredStore {
        fn load(&self) -> LoadOutcome {
            LoadOutcome::Stored(Config {
                main: crate::config::MainPreset {
                    codec: "h265".into(),
                    ..Default::default()
                },
                ..Config::default()
            })
        }

        fn store(&self, _c: &Config) -> crate::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn get_config_returns_the_actually_stored_value_when_present() {
        let response = admin_router(
            Arc::new(StoredStore),
            StatusHandle::new(),
            router_with(&[]).1,
        )
        .oneshot(
            Request::builder()
                .uri("/admin/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let cfg: Config = serde_json::from_value(body_json(response).await).unwrap();
        assert_eq!(cfg.main.codec, "h265");
    }

    #[tokio::test]
    async fn get_config_on_broken_backend_still_serves_defaults_but_records_last_error() {
        let status = StatusHandle::new();
        let router = admin_router(Arc::new(BrokenStore), status.clone(), router_with(&[]).1);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/admin/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // The response itself is indistinguishable from "unconfigured" —
        // that's expected, the app still has to boot on *something* — but
        // unlike the bug in #955, the failure is no longer invisible: it
        // must now be visible through `/admin/status`.
        assert_eq!(response.status(), StatusCode::OK);
        let cfg: Config = serde_json::from_value(body_json(response).await).unwrap();
        assert_eq!(cfg, Config::default());

        let last_error = status.snapshot().last_error;
        assert!(
            last_error
                .as_deref()
                .is_some_and(|e| e.contains("simulated backend failure")),
            "expected last_error to surface the broken backend, got {last_error:?}"
        );
    }
}
