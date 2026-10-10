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

The app always listens on `127.0.0.1:2999`. This is fixed (it is the
manifest's `reverseProxy` target) and not configurable. It is not reachable
from the network; all access goes through the camera's own web server and its
VAPIX authentication, via the reverse-proxy paths above.

## Live reconfiguration

Every setting applies immediately via `POST /admin/config` (see
[Configuration](configuration.md)); no restart is needed. That endpoint is
reachable only at the `admin` access level, behind the camera's reverse proxy.

## Reporting a vulnerability

Use the repository's standard GitHub issue/security-advisory process:
[github.com/fishloa/axis-multimux-edge](https://github.com/fishloa/axis-multimux-edge).
