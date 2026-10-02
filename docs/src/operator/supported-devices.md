# Supported Devices

`multimux-edge` targets Axis cameras built on the **ARTPEC-6, 7, 8, and 9**
SoCs. Every release ships four `.eap` packages — one per (firmware,
architecture) pair:

| Firmware | SDK version | Architecture | `.eap` filename suffix |
|---|---|---|---|
| 11 | 1.15.1 | aarch64 | `fw11_aarch64` |
| 11 | 1.15.1 | armv7hf | `fw11_armv7hf` |
| 12 | 12.1.0 | aarch64 | `fw12_aarch64` |
| 12 | 12.1.0 | armv7hf | `fw12_armv7hf` |

Match your camera's CPU architecture and installed firmware major version
to pick the right file from a [release](https://github.com/fishloa/axis-multimux-edge/releases).

## Codec support

| SoC | H.264 | H.265 |
|---|---|---|
| ARTPEC-6 | yes | no |
| ARTPEC-7 | yes | yes |
| ARTPEC-8 | yes | yes |
| ARTPEC-9 | yes | yes |

Set the codec via the [`codec` config field](configuration.md) — H.265 is
only valid on ARTPEC-7/8/9; requesting it on ARTPEC-6 will not work since the
hardware encoder doesn't support it.

## Architecture, not model number

Axis camera model numbers don't map 1:1 to a SoC generation across product
lines — check your specific camera model's datasheet or `Vendor` /
`ProdNbr` VAPIX parameters for its ARTPEC generation if you're unsure which
`.eap` to install.
