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
    const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

    /// VAPIX-backed source with a short cache, so a burst of first requests
    /// doesn't hit the camera once each.
    pub struct VapixProfileSource {
        cache: tokio::sync::Mutex<Option<(Instant, Vec<CameraProfile>)>>,
    }

    impl VapixProfileSource {
        pub fn new() -> Self {
            VapixProfileSource {
                cache: tokio::sync::Mutex::new(None),
            }
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
            if !resp.status().is_success() {
                return Err(format!("streamprofile.cgi HTTP {}", resp.status()));
            }
            let text = resp
                .text()
                .await
                .map_err(|e| format!("streamprofile.cgi body: {e}"))?;
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
                let list = tokio::time::timeout(FETCH_TIMEOUT, Self::fetch())
                    .await
                    .map_err(|_| "streamprofile.cgi timed out after 5 s".to_string())?;
                let list = list?;
                *cache = Some((Instant::now(), list.clone()));
                Ok(list)
            })
        }
    }

    impl Default for VapixProfileSource {
        fn default() -> Self {
            Self::new()
        }
    }
}

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
        let e =
            parse_list_response(r#"{"apiVersion":"1.0","error":{"code":2002,"message":"Bad"}}"#)
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
