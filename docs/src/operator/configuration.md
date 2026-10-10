# Configuration

Configuration is read/written through the admin HTTP API and persisted
on-device via the ACAP `axparameter` store. **Configuration changes apply
immediately** — the capture pipeline reconfigures live without a restart.

## Config Structure

The configuration is a JSON object with the following top-level keys:

```json
{
  "main": {
    "channel": 0,
    "width": 1920,
    "height": 1080,
    "framerate": 30,
    "codec": "h264"
  },
  "streams": [
    {
      "name": "medium",
      "profile": "ACC_Medium"
    }
  ],
  "default_stream": "medium",
  "max_encodes": 2,
  "idle_timeout_secs": 30,
  "target_duration_secs": 4.0,
  "part_target_ms": 500,
  "window_segments": 8
}
```

### Main Capture Settings

| Field | Type | Default | Meaning |
|---|---|---|---|
| `main.channel` | integer | `0` | VDO channel index to capture from (single-sensor cameras use `0`). |
| `main.width` | integer | `1920` | Capture width, pixels. |
| `main.height` | integer | `1080` | Capture height, pixels. |
| `main.framerate` | integer | `30` | Capture frame rate, fps. |
| `main.codec` | string | `"h264"` | `"h264"` or `"h265"` — see [Supported Devices](supported-devices.md) for which SoCs support H.265. |

### Streams and Profiles

| Field | Type | Default | Meaning |
|---|---|---|---|
| `streams` | array | `[]` | List of named streams, each mapping a URL name to a camera profile. |
| `streams[].name` | string | *(required)* | URL name for the stream: lowercase a–z, 0–9 and `-`, 1–32 chars, starting with a letter or digit. The reserved names `main` and `media.m3u8` cannot be used. Stream URLs appear at `/local/multimuxedge/hls/<name>/media.m3u8`. |
| `streams[].profile` | string | *(required)* | Camera stream profile name (e.g., `ACC_Medium`, `ACC_High`). Query available profiles with `GET /admin/profiles`. |
| `default_stream` | string | *(optional)* | Name of the stream to serve at the bare URL `/local/multimuxedge/hls/media.m3u8`. If not set, the bare URL redirects to `main` (the built-in main preset). |

### Encoder Limits

| Field | Type | Default | Meaning |
|---|---|---|---|
| `max_encodes` | integer | `2` | Maximum concurrent encodes (range 1–8). When the limit is reached, new stream requests receive a 503 response. The ARTPEC-6 (P1448-LE) camera handles about 2 distinct encodes; 3 or more will slow every stream to 15–18 fps (see Known limitation below). Other clients (e.g., a VMS) share this limit. |
| `idle_timeout_secs` | integer | `30` | Seconds of inactivity after which an idle stream shuts down its encode (range 5–600). This frees encoder resources for other streams. |

### LL-HLS Parameters

| Field | Type | Default | Meaning |
|---|---|---|---|
| `target_duration_secs` | float | `4.0` | LL-HLS target segment duration, seconds. |
| `part_target_ms` | integer | `500` | LL-HLS target part duration, milliseconds. |
| `window_segments` | integer | `8` | Number of segments kept in the LL-HLS media playlist window. |

## Stream URLs

**Built-in main stream:** every camera always serves its main preset at
`/local/multimuxedge/hls/main/media.m3u8`.

**Named streams:** configured streams are served at `/local/multimuxedge/hls/<name>/media.m3u8`,
where `<name>` is the stream's configured name. For example, a stream named `medium` is at:

```
https://<cam>/local/multimuxedge/hls/medium/media.m3u8
```

**Bare URL:** `/local/multimuxedge/hls/media.m3u8` redirects (302, with query parameters preserved)
to `/<default_stream>/media.m3u8` if a default stream is configured in the config; otherwise
it redirects to `/main/media.m3u8`.

## Profile Keys

Stream profiles can override these keys from the main capture settings:

| Key | Type | Meaning |
|---|---|---|
| `videocodec` | string | `"h264"` or `"h265"` to override the main codec. |
| `resolution` | string | Width × height, e.g., `"1280x720"`, to override width/height. |
| `fps` | integer | Frames per second to override the main framerate. If omitted or 0, the camera's default fps is used. |
| `camera` | integer | VDO channel index to override the main channel. |
| `videokeyframeinterval` | integer | Keyframe interval; 0 means the camera default and is treated as unset. |

All other profile keys (compression, bitrate, audio, etc.) are **ignored** — the stream will use whatever the
profile specifies for those. If a profile omits a key listed above, the corresponding main setting is used.

## API

### Fetch profiles available on the camera

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/profiles
```

Returns a profiles object, e.g.:

```json
{
  "profiles": [
    {
      "name": "ACC_High",
      "description": "High quality H.264 stream",
      "parameters": "videocodec=h264&resolution=1920x1080&fps=30&camera=0",
      "settings": "h264 1920x1080@30 ch0",
      "ignored_keys": ["compression", "bitrate", "audio"],
      "error": null
    },
    {
      "name": "ACC_Medium",
      "description": "Medium quality H.264 stream",
      "parameters": "videocodec=h264&resolution=1280x720&fps=25&camera=0",
      "settings": "h264 1280x720@25 ch0",
      "ignored_keys": ["compression", "bitrate", "audio"],
      "error": null
    }
  ],
  "error": null
}
```

Each profile object contains:
- `name`: profile identifier (use in `streams[].profile`)
- `description`: human-readable description
- `parameters`: raw camera profile parameters
- `settings`: the capture settings this app would use (e.g., codec/resolution/fps), or null if unusable
- `ignored_keys`: profile keys this app cannot override (e.g., compression, bitrate, audio)
- `error`: reason the profile cannot be captured, if any (e.g., unsupported codec)

The top-level `error` is set if the camera's profile list itself could not be read.

### Read the current configuration

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/config
```

### Update the configuration (applies immediately)

```bash
curl -u <user>:<pw> -X POST https://<cam>/local/multimuxedge/admin/config \
  -H 'content-type: application/json' \
  -d '{
    "main": {
      "channel": 0,
      "width": 1920,
      "height": 1080,
      "framerate": 30,
      "codec": "h264"
    },
    "streams": [
      {
        "name": "medium",
        "profile": "ACC_Medium"
      }
    ],
    "default_stream": "medium",
    "max_encodes": 2,
    "idle_timeout_secs": 30,
    "target_duration_secs": 4.0,
    "part_target_ms": 500,
    "window_segments": 8
  }'
```

A successful `POST` returns `200 OK` with `{"status":"applied"}`.
A validation error returns `400 Bad Request` with `{"errors":[{"field":"<name>","message":"<reason>"},...]}`.

### Validation

A `POST /admin/config` is rejected with `400 Bad Request` if:

- `main.codec` is not `"h264"` or `"h265"`.
- `streams[].name` is not lowercase a–z, 0–9, and `-`, is too short or long, or starts with a digit.
- `streams[].name` is a reserved name (`main` or `media.m3u8`).
- `streams[].name` is used more than once (duplicates).
- `streams[].profile` is empty.
- `default_stream` names a stream that does not exist in the `streams` array.
- `target_duration_secs` is `<= 0`.
- `part_target_ms` is `<= 0`.
- `window_segments` is `<= 0`.
- `max_encodes` is outside the range 1–8.
- `idle_timeout_secs` is outside the range 5–600.

**Note:** The app does NOT validate that a profile exists on the camera at config time.
A missing or unavailable profile shows up later as a 503 `camera profile "X" not found`
or `profile source unavailable: …` error when a stream URL is accessed.

> **Known limitation:** on the P1448-LE (ARTPEC-6, AXIS OS 11.11), setting any
explicit key-frame interval caps 4K capture at ~18 fps (the camera's default
gives 25 fps, also over its own RTSP). The app no longer forces a key-frame
interval; it only applies one when the stream profile sets
`videokeyframeinterval`, so avoid setting it on 4K profiles.
