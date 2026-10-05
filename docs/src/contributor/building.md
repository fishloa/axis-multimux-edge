# Building

`multimux-edge` is **out of the main cargo workspace** (its own `Cargo.toml` +
`rust-toolchain.toml`, pinned to Rust **1.97**). It depends on published
`multimux 0.11` + `transmux 0.25` + `media-plane 0.5` + `broadcast-common 9.4`
(crates.io — the latter two only under the `device` feature, see
`vdo_source.rs`) and **git-pinned** acap-rs crates (`vdo`/`axparameter`/
`acap-logging` at rev `b1f674c7b5fdde8e171911c591cf1ecb0b3296b6` via a fork,
`fishloa/acap-rs`). It is **not** published to crates.io (`publish = false`)
— it is a deployable `.eap`. The fork exists for firmware-11 SDK 1.15.1
compatibility: it drops a firmware-12-only `VDO_ERROR_NO_VIDEO` diagnostic
arm so the `vdo` crate compiles against the fw11 SDK too.

## Host build (macOS/Linux, no `device` feature)

Compiles + tests the host-testable lib only (`convert`, `admin`, `error` —
see [Module Map](module-map.md)):

```bash
cargo test --locked
cargo clippy --locked -- -D warnings
cargo fmt --all --check
```

## Device build (the `.eap`)

The `device` feature pulls in the acap-rs crates and **only builds inside
the Axis ACAP Native SDK Docker** (`vdo-sys` needs `pkg-config vdo` from the
SDK sysroot — macOS cannot cross-compile ACAP). CI
(`.github/workflows/ci.yml`'s `eap` job) builds all four (firmware,
architecture) combinations on every PR and every push to `main` — see
[Supported Devices](../operator/supported-devices.md) for the matrix. To
build locally in the SDK Docker:

```bash
docker run --rm -v "$PWD:/w" -w /w axisecp/acap-native-sdk:12.11.0-aarch64-ubuntu24.04 bash -lc '
  apt-get update && apt-get install -y --no-install-recommends build-essential curl ca-certificates pkg-config clang libclang-dev
  curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.97.0 --target aarch64-unknown-linux-gnu
  . "$HOME/.cargo/env"
  # set the sysroot / PKG_CONFIG / linker env per .github/workflows/ci.yml's "Discover SDK sysroot" + "Set cross-compile env" steps
  cargo install --locked --git https://github.com/AxisCommunications/acap-rs --rev 5eed2e27fcb2a83e4d42ef71ccd49039949e9cca cargo-acap-build
  ACAP_BUILD_IMPL=equivalent cargo-acap-build --arch aarch64 -- -p multimux-edge --features device
'
# -> target/acap/multimux-edge_0_1_0_aarch64.eap
```

For firmware 11, swap the image tag for `1.15.1-aarch64-ubuntu22.04` (or the
`armv7hf` variant of either). `.github/workflows/ci.yml` is the
authoritative, working recipe — the container env vars it sets for the
sysroot/linker/pkg-config are required; this snippet elides them for
brevity.

See [Releasing](releasing.md) for how these builds become a published
`.eap` set, and [Testing](testing.md) for what "passing" actually means for
each build.

## Building this documentation site

Build the mdBook guide locally with `mdbook build docs` (output in
`docs/book/`), or `mdbook serve docs` for a live-reloading preview while
editing. Pushes to `main` auto-publish the guide + rustdoc API reference to
GitHub Pages via the `docs` job in `.github/workflows/ci.yml`.
