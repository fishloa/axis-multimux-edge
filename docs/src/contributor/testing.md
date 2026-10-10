# Testing

## Host tests

The host-testable lib (`convert`, `admin`, `error` — see
[Module Map](module-map.md)) is tested on every PR and every push to `main`
with:

```bash
cargo test --locked
cargo clippy --locked -- -D warnings
cargo fmt --all --check
```

This covers `tests/convert_synthetic.rs` (synthetic Annex-B conversion
cases) and `src/admin.rs`'s route tests (config get/post validation,
status snapshot fields, `ConfigStore` broken/stored/unset behavior via test
doubles).

## Device build verification

The `device`-gated code (`vdo_source.rs`, the `multimux-edge` binary) can only
be **compiled** on host CI (inside the ACAP SDK Docker — see
[Building](building.md)); compiling is not the same as verifying it works.
Compilation success is a prerequisite, not an acceptance criterion.

## Hardware verification checklist

The real "done" for any change touching the capture/serving path is
verified on a real ARTPEC-6/7/8/9 camera:

1. The `.eap` installs and the app shows **running** in the camera's Apps
   list.
2. `curl -u <user>:<pw> https://<cam>/local/multimuxedge/hls/media.m3u8`
   (a 302 to the default stream, or `main`) or
   `.../hls/main/media.m3u8` returns an LL-HLS media playlist (`#EXT-X-PART`, `#EXT-X-PART-INF`,
   `#EXT-X-SERVER-CONTROL`).
3. A real LL-HLS player (Safari / hls.js / `ffplay`) plays **live** video via
   that URL at low latency.
4. The admin settings page loads at `https://<cam>/local/multimuxedge/index.html` (the bare
   directory returns 401 on AXIS OS 11.11) and a config change (e.g.
   `part_target_ms`) takes effect immediately.
5. H.264 verified on all SoCs; H.265 verified additionally on
   ARTPEC-7/8/9 (see [Supported Devices](../operator/supported-devices.md)).

> **Known verify-on-device detail:** the origin is served nested under
> `/hls`; if the camera's reverse-proxy strips the `apiPath` segment before
> forwarding, the nest prefix may need adjusting in
> `src/bin/multimux-edge.rs`'s `Router::nest("/hls", …)`. Confirm the actual
> proxied path on the device if the playlist 404s. Likewise confirm VDO
> delivers Annex-B (start codes) — `convert` assumes it; if a given
> camera/VDO version carries a different framing, `VdoIngestSession`'s
> `data_copy()` handling needs adjusting.

See [Troubleshooting](../operator/troubleshooting.md) for diagnosing a
failure at any of these steps via `/admin/status`.
