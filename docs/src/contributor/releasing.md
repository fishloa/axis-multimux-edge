# Releasing

Releases are entirely CI-driven — never manually `gh release create` or
upload artifacts from a local machine. Pushing a `v*` tag is the only
trigger.

## What happens on `git push --tags`

1. The `eap` matrix job (four combinations: firmware 11/12 ×
   aarch64/armv7hf — see [Supported Devices](../operator/supported-devices.md))
   builds a `.eap` for each and uploads it as a workflow artifact.
2. The `release` job (gated on `startsWith(github.ref, 'refs/tags/v')`)
   downloads every `multimux-edge-*` artifact, renames each to
   `<name>_<version>_<fw>_<arch>.eap`, generates a `SHA256SUMS` file, then
   creates (or reuses) a GitHub Release named after the tag and uploads the
   four `.eap`s + `SHA256SUMS` to it.

## Cutting a release

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

Then watch the Actions run (`gh run watch` or the Actions tab) — the
release appears at `github.com/fishloa/axis-multimux-edge/releases/tag/vX.Y.Z`
once the `release` job completes.

## Moving a tag (fixing a bad release)

If a tag was pushed against a broken commit, move it and re-push:

```bash
git tag -f vX.Y.Z <good-commit>
git push -f origin vX.Y.Z
```

This is safe for a tag that's only minutes old with no external consumers;
for anything already depended on externally, cut a new patch tag instead of
force-moving.
