# Configuration

Configuration is read/written through the admin HTTP API and persisted
on-device via the ACAP `axparameter` store. **Changes take effect on the
next app restart** — the running capture pipeline is not reconfigured live.

## Fields

| Field | Type | Default | Meaning |
|---|---|---|---|
| `channel` | integer | `0` | VDO channel index to capture from (single-sensor cameras use `0`). |
| `width` | integer | `1920` | Capture width, pixels. |
| `height` | integer | `1080` | Capture height, pixels. |
| `framerate` | integer | `30` | Capture frame rate, fps. |
| `codec` | string | `"h264"` | `"h264"` or `"h265"` — see [Supported Devices](supported-devices.md) for which SoCs support H.265. |
| `target_duration_secs` | float | `4.0` | LL-HLS target segment duration, seconds. |
| `part_target_ms` | integer | `500` | LL-HLS target part duration, milliseconds. |
| `window_segments` | integer | `8` | Number of segments kept in the LL-HLS media playlist window. |
| `port` | integer | `2999` | HTTP bind port (must match `manifest.json`'s `reverseProxy` target). |

## Validation

A `POST /admin/config` is rejected with `400 Bad Request` if:

- `codec` is anything other than `"h264"` or `"h265"`.
- `target_duration_secs` is `<= 0`.
- `part_target_ms` is `0`.
- `window_segments` is `0`.
- `port` is `0`.

## API

```bash
# Read the current config
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/config

# Update it (takes effect on next restart)
curl -u <user>:<pw> -X POST https://<cam>/local/multimuxedge/admin/config \
  -H 'content-type: application/json' \
  -d '{"channel":0,"width":1920,"height":1080,"framerate":30,"codec":"h265","target_duration_secs":4.0,"part_target_ms":500,"window_segments":8,"port":2999}'
```

A successful `POST` returns `{"status":"ok","note":"takes effect on restart"}`.
After changing the config, restart the app (Apps list, or
`cargo-acap-sdk start`) for it to take effect.
