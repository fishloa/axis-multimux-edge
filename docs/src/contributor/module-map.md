# Module Map

| File | Lines | Host-testable? | Responsibility |
|---|---|---|---|
| `src/lib.rs` | 14 | yes | Crate root — re-exports `convert`, `admin`, `error`; gates `vdo_source`/the bin behind `device`. |
| `src/error.rs` | 28 | yes | `OriginError` + `Result` alias used across the crate. |
| `src/convert.rs` | 393 | yes | Pure VDO Annex-B access unit → `transmux::Sample`/`TrackSpec` conversion, in-band SPS/PPS/VPS extraction. No `vdo` dependency. |
| `src/admin.rs` | 731 | yes (routes + `DefaultStore`); `AxParameterStore` is device-gated | `Config`, `ConfigStore` trait + `DefaultStore`/`AxParameterStore` impls, `Status`/`StatusHandle`, the `/admin/config` + `/admin/status` axum routes. |
| `src/vdo_source.rs` | 530 | no — `device`-gated | Drives `vdo::StreamBuilder`/`RunningStream::next_buffer`; implements `media_plane::ingress::IngestSession` for VDO capture. |
| `src/bin/multimux-edge.rs` | 365 | no — `device`-gated, `required-features = ["device"]` | The ACAP entrypoint binary: loads config, spawns the capture pipeline thread, serves origin + admin routes. |

## Host vs device

Only `convert.rs`, `error.rs`, and `admin.rs`'s non-`AxParameterStore` code
paths compile and run on a host machine (macOS/Linux) without the `device`
feature. `vdo_source.rs` and the `multimux-edge` binary require the `device`
feature, which pulls in `vdo`/`axparameter`/`acap-logging` (git-pinned,
ACAP-only crates) and only builds inside the Axis ACAP Native SDK Docker
image. See [Building](building.md).

## Tests

- `tests/convert_synthetic.rs` — integration tests against `convert.rs`
  using synthetic Annex-B input, host-only.
- `src/admin.rs`'s `#[cfg(test)] mod tests` — axum route tests using
  `tower::ServiceExt::oneshot` against `DefaultStore`/test-double
  `ConfigStore` implementations (`BrokenStore`, `StoredStore`), host-only.

See [Testing](testing.md) for how to run these and how device-only code is
verified instead.
