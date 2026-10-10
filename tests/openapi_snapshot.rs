//! The committed API description must match the code. Regenerate with
//! `UPDATE_OPENAPI=1 cargo test --test openapi_snapshot`.

#[test]
fn openapi_snapshot_matches() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/api/openapi.json");
    let fresh = multimux_edge::openapi::openapi_json();
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &fresh).unwrap();
    }
    let committed = std::fs::read_to_string(path).expect("run with UPDATE_OPENAPI=1 once");
    assert_eq!(
        committed, fresh,
        "openapi.json is stale: rerun with UPDATE_OPENAPI=1"
    );
}
