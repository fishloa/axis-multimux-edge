//! OpenAPI description of the admin API, generated from the handler and
//! type annotations. Served at `/admin/openapi.json`; a snapshot lives at
//! `docs/src/api/openapi.json` for the docs site.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Multimux Edge admin API",
        description = "Configure stream mappings and inspect captures. All paths are under /local/multimuxedge and require the camera's admin access level."
    ),
    servers((url = "/local/multimuxedge")),
    tags((name = "admin", description = "Multimux Edge configuration and status")),
    paths(
        crate::admin::get_config,
        crate::admin::post_config,
        crate::admin::get_status,
        crate::admin::get_profiles,
        crate::admin::get_openapi,
    ),
)]
pub struct ApiDoc;

pub fn openapi_json() -> String {
    let mut doc = ApiDoc::openapi();
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
    doc.to_pretty_json().expect("openapi serializes")
}
