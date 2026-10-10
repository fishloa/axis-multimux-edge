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

/// Extract the raw (percent-encoded) remainder of the path after the stream name.
/// Path from axum nest is `/{name}/{rest}` still encoded; returns the `rest` portion
/// or None if there is no remainder.
pub(crate) fn raw_rest(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    let after_name = path.split_once('/')?;
    Some(after_name.1)
}

/// Check if a raw (percent-encoded) path contains dot segments that could
/// enable traversal: `.`, `..`, `%2e`, `%2e%2e`, `.%2e`, `%2e.` (case-insensitive).
pub(crate) fn has_dot_segment(raw: &str) -> bool {
    for segment in raw.split('/') {
        let seg_lower = segment.to_lowercase();
        if seg_lower == "."
            || seg_lower == ".."
            || seg_lower == "%2e"
            || seg_lower == "%2e%2e"
            || seg_lower == ".%2e"
            || seg_lower == "%2e."
        {
            return true;
        }
    }
    false
}

async fn forward(
    State(reg): State<Arc<Registry>>,
    Path((name, _)): Path<(String, String)>,
    mut req: Request,
) -> Response {
    let router = match reg.ensure(&name, Instant::now()).await {
        Ok(r) => r,
        Err(e) => return serve_error(e),
    };

    // Extract raw remainder from the request path (still percent-encoded).
    let raw = raw_rest(req.uri().path()).unwrap_or_default();

    // Reject any remainder with dot segments (path traversal protection).
    if has_dot_segment(raw) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Build forwarded URI with raw-encoded remainder + original query.
    let mut uri_str = format!("/{INNER_STREAM}/{}", raw);
    if let Some(q) = req.uri().query() {
        uri_str.push('?');
        uri_str.push_str(q);
    }

    match uri_str.parse() {
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
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            msg,
        )
            .into_response()
    };
    match e {
        ServeError::NotFound => StatusCode::NOT_FOUND.into_response(),
        ServeError::EncoderBusy { in_use, max } => {
            unavailable(format!("encoder busy ({in_use}/{max} encodes in use)"))
        }
        ServeError::ProfileMissing(p) => unavailable(format!("camera profile \"{p}\" not found")),
        ServeError::ProfileUnavailable(m) => {
            unavailable(format!("profile source unavailable: {m}"))
        }
        ServeError::Unsupported(m) => unavailable(m),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;
    use crate::registry::tests::setup;

    async fn get(app: &axum::Router, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn app(streams: &[(&str, &str)]) -> (axum::Router, Arc<Registry>) {
        let (reg, _, _) = setup(streams);
        (
            axum::Router::new().nest("/hls", hls_router(reg.clone())),
            reg,
        )
    }

    #[tokio::test]
    async fn unknown_name_404() {
        let (app, _) = app(&[]);
        assert_eq!(
            get(&app, "/hls/nope/media.m3u8").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn mapped_name_reaches_multimux_router() {
        // Empty stream: multimux answers 503 (no segments yet), proving the forward.
        let (app, _) = app(&[("medium", "ACC_Medium")]);
        assert_eq!(
            get(&app, "/hls/medium/media.m3u8").await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn bare_url_redirects_to_default_keeping_query() {
        let (app, reg) = app(&[("medium", "ACC_Medium")]);
        let r = get(&app, "/hls/media.m3u8?_HLS_msn=12&_HLS_part=3").await;
        assert_eq!(r.status(), StatusCode::FOUND);
        assert_eq!(
            r.headers()[header::LOCATION],
            "main/media.m3u8?_HLS_msn=12&_HLS_part=3"
        );
        let mut cfg = reg.config();
        cfg.default_stream = Some("medium".into());
        reg.apply(cfg);
        let r = get(&app, "/hls/media.m3u8").await;
        assert_eq!(r.headers()[header::LOCATION], "medium/media.m3u8");
    }

    #[test]
    fn raw_rest_extracts_remainder() {
        assert_eq!(raw_rest("/medium/media.m3u8"), Some("media.m3u8"));
        assert_eq!(raw_rest("/main/foo%3Fx=1"), Some("foo%3Fx=1"));
        assert_eq!(raw_rest("/main/seg-1-2.m4s"), Some("seg-1-2.m4s"));
        assert_eq!(raw_rest("/medium/"), Some(""));
        assert_eq!(raw_rest("/medium"), None);
    }

    #[test]
    fn has_dot_segment_detects_traversal() {
        assert!(!has_dot_segment("media.m3u8"));
        assert!(!has_dot_segment("foo%3Fx=1"));
        assert!(!has_dot_segment("seg-1-2.m4s"));
        assert!(has_dot_segment(".."));
        assert!(has_dot_segment("."));
        assert!(has_dot_segment("%2e%2e"));
        assert!(has_dot_segment("%2e"));
        assert!(has_dot_segment(".%2e"));
        assert!(has_dot_segment("%2e."));
        // Case-insensitive
        assert!(has_dot_segment("%2E%2E"));
        assert!(has_dot_segment("%2E"));
        // In segments
        assert!(has_dot_segment("foo/../bar"));
        assert!(has_dot_segment("foo/./bar"));
    }

    #[test]
    fn forward_path_with_query_preserved() {
        // Normal path with query
        let req = Request::builder()
            .uri("/medium/media.m3u8?_HLS_msn=12&_HLS_part=3")
            .body(Body::empty())
            .unwrap();
        let raw = raw_rest(req.uri().path()).unwrap();
        assert_eq!(raw, "media.m3u8");
        assert_eq!(req.uri().query(), Some("_HLS_msn=12&_HLS_part=3"));
        let mut uri_str = format!("/{INNER_STREAM}/{}", raw);
        if let Some(q) = req.uri().query() {
            uri_str.push('?');
            uri_str.push_str(q);
        }
        assert_eq!(uri_str, "/s/media.m3u8?_HLS_msn=12&_HLS_part=3");
    }

    #[test]
    fn forward_path_encoding_preserved() {
        // Percent-encoded ? is preserved, not decoded to actual query
        let req = Request::builder()
            .uri("/main/foo%3Fx=1")
            .body(Body::empty())
            .unwrap();
        let raw = raw_rest(req.uri().path()).unwrap();
        assert_eq!(raw, "foo%3Fx=1");
        let mut uri_str = format!("/{INNER_STREAM}/{}", raw);
        if let Some(q) = req.uri().query() {
            uri_str.push('?');
            uri_str.push_str(q);
        }
        assert_eq!(uri_str, "/s/foo%3Fx=1");
    }

    #[test]
    fn forward_segment_names() {
        let req = Request::builder()
            .uri("/main/seg-1-2.m4s")
            .body(Body::empty())
            .unwrap();
        let raw = raw_rest(req.uri().path()).unwrap();
        assert_eq!(raw, "seg-1-2.m4s");
        let uri_str = format!("/{INNER_STREAM}/{}", raw);
        assert_eq!(uri_str, "/s/seg-1-2.m4s");
    }

    #[tokio::test]
    async fn encoder_busy_is_503_with_retry_after_and_reason() {
        let (app, _) = app(&[("hi", "ACC_High"), ("med", "ACC_Medium"), ("lo", "ACC_Low")]);
        get(&app, "/hls/hi/media.m3u8").await;
        get(&app, "/hls/med/media.m3u8").await;
        let r = get(&app, "/hls/lo/media.m3u8").await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()[header::RETRY_AFTER], "5");
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"encoder busy (2/2 encodes in use)");
    }

    #[tokio::test]
    async fn missing_profile_is_503_with_reason() {
        let (app, _) = app(&[("gone", "Deleted")]);
        let r = get(&app, "/hls/gone/media.m3u8").await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"camera profile \"Deleted\" not found");
    }

    #[tokio::test]
    async fn dot_segment_in_path_rejected() {
        let (app, _) = app(&[("main", "ACC_Medium")]);
        assert_eq!(
            get(&app, "/hls/main/%2e%2e/x").await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            get(&app, "/hls/main/../x").await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}
