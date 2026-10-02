# Security

## Access levels

`multimux-edge` registers two reverse-proxy paths with the camera's web
server, each gated by a different VAPIX access level (`manifest.json`):

| Path | `apiPath` | Access level |
|---|---|---|
| `https://<cam>/local/multimuxedge/hls/...` | `hls` | `viewer` |
| `https://<cam>/local/multimuxedge/admin/...` | `admin` | `admin` |

This means: any account with **viewer** rights on the camera can pull the
LL-HLS stream; only accounts with **admin** rights can read/write config or
status. Grant camera accounts accordingly — a viewer-only account cannot
read or change `multimux-edge`'s configuration.

## Local-only bind

The app itself binds to `127.0.0.1:2999` (see [Configuration](configuration.md)'s
`port` field) — it is not reachable directly on the network; all access goes
through the camera's own web server and its VAPIX authentication, via the
reverse-proxy paths above.

## No live reconfiguration

Config changes only take effect on restart (see [Configuration](configuration.md)) —
there is no live-reload code path an attacker with `admin` access could use
to affect a running pipeline beyond what a restart itself does.

## Reporting a vulnerability

Use the repository's standard GitHub issue/security-advisory process:
[github.com/fishloa/axis-multimux-edge](https://github.com/fishloa/axis-multimux-edge).
