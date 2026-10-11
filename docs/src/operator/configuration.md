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
| `max_encodes` | integer | `2` | Maximum concurrent encodes of the app's own (range 1–8). When the limit is reached, a stream that would need a new encode receives a 503 response. Streams that join an encode the camera already runs don't count (see [Sharing the camera's encodes](#sharing-the-cameras-encodes)). The camera's other clients (e.g., a VMS) use the same encoder budget. |
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

Stream profiles can set these keys. Each is applied the way the camera's own
RTSP server applies it, so a mapped profile streams the same picture as the
profile over RTSP:

| Key | Type | Meaning |
|---|---|---|
| `videocodec` | string | `"h264"` or `"h265"`. If omitted, the main codec. |
| `resolution` | string | Width × height, e.g., `"1280x720"`. If omitted, the main resolution. |
| `fps` | integer | Frames per second. If omitted or 0, the camera's default fps is used. |
| `camera` | integer | VDO channel index. If omitted, camera 1 is used (the VAPIX default, the same video the camera's own RTSP server sends for that profile), not the main channel. |
| `videokeyframeinterval` | integer | Keyframe interval; 0 means the camera default and is treated as unset. |
| `compression` | integer | Compression, 0–100. |
| `rotation` | integer | 0, 90, 180 or 270. |
| `mirror` | 0/1 | Mirror the image horizontally. |
| `videobitratemode` | string | `vbr`, `mbr` or `abr` (`cbr` is not supported and is ignored). |
| `videomaxbitrate` | integer | Maximum bitrate in kbit/s (for `mbr`). |
| `videoabrtargetbitrate`, `videoabrretentiontime` | integer | Average bitrate target (kbit/s) and retention time (s) for `abr`. |
| `videozgopmode`, `videozfpsmode` | string | Zipstream GOP and frame rate mode: `fixed` or `dynamic`. |
| `videozmaxgoplength` | integer | Zipstream maximum GOP length. |

Other profile keys (audio, text overlays, `videozstrength`, …) are **ignored**;
the admin page lists them next to each profile. A stream that joins an encode
the camera already runs carries that encode's settings for the keys its
profile doesn't set, except `rotation` and `mirror`: a profile without them
only joins an upright, unmirrored encode. If the camera's image is rotated
in its own settings, set `rotation` in the profile to match so the stream
can share the camera's encodes.

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

### Sharing the camera's encodes

The camera's hardware encoder has a fixed budget: the P1448-LE (ARTPEC-6)
fits about one and a half 4K25 encodes. The camera's own RTSP server keeps
its encodes running for its clients (a VMS recording `ACC_High`, say), and
a second client asking for the same stream joins that encode for free.

Multimux Edge does the same. Before it starts an encode for a stream, it
looks for one the camera is already running with the same camera, codec,
resolution and frame rate (and key-frame interval, if the profile sets one)
and joins it. A joined stream:

- costs the encoder nothing, so it runs at the full rate (4K at 25 fps)
  and doesn't slow the camera's other clients;
- carries the existing encode's compression, Zipstream and overlay settings;
- shows `"shared_encode": true` in `/admin/status` and doesn't count
  against `max_encodes`;
- keeps running if the camera's other clients of that encode disconnect;
  it then carries the encode alone and counts against `max_encodes` again
  (within about 5 seconds).

A stream that has nothing to join gets an encode of its own, which counts
against `max_encodes`. A stream that can join is let in even when
`max_encodes` is reached. The app never joins its own encodes; streams that
resolve to the same settings already share one capture. Every distinct encode shares the encoder budget, as
it would for an RTSP client: on the P1448-LE, a 4K stream plus two more
distinct encodes (say 1080p and 720p) bring everything down to about
20 fps.

Profiles without `camera=` use camera 1, as the camera's RTSP server does,
so a mapped profile matches the camera's own stream for that profile. The
main preset uses `main.channel` (default 0); RTSP numbers cameras from 1, so
channel 0 is never one of the camera's RTSP encodes and `main` always runs
an encode of its own. To share, map a stream to a profile instead, or set
`main.channel` to 1 and match a resolution and frame rate the camera
already streams.

Setting `videokeyframeinterval` on a profile only joins an encode with
that exact interval; otherwise the stream starts its own encode.
