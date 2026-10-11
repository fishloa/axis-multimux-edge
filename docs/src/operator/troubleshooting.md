# Troubleshooting

## Check `/admin/status` first

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/status
```

Returns:

```json
{
  "last_error": null,
  "encodes": {
    "in_use": 1,
    "max": 2
  },
  "streams": [
    {
      "names": ["medium"],
      "settings": "h264 1280x720@25 ch1",
      "state": "running",
      "running": true,
      "current_segment": 5,
      "current_part": 2,
      "frames": 1234,
      "fps": 25.0,
      "idle_secs": 0,
      "last_error": null,
      "shared_encode": false
    }
  ]
}
```

Interpret the response as follows:

- `last_error` non-null — a config load or capture setup failure at boot
  (e.g., a broken `axparameter` backend). The text describes why.
- `encodes.in_use >= encodes.max` — the app's own encodes are at the cap.
  A stream that needs a new encode will receive a 503; one that can join an
  encode the camera already runs still starts.
- `streams` lists every active capture.
  - `names` — stream names being served by this capture (may be multiple if
    different streams map to the same profile).
  - `settings` — capture codec, resolution, fps, and VDO channel, e.g. `h264 1280x720@25 ch1`.
  - `state` — `starting`, `running`, or `error`.
  - `last_error` — reason the stream failed, if any.
  - `shared_encode` — `true` when the stream joined an encode the camera was
    already running (for its RTSP clients, say); such streams don't count in
    `encodes.in_use`.

## Stream URLs

**Named streams:** `/local/multimuxedge/hls/<name>/media.m3u8` where `<name>` is
the configured stream name. For example:

```
https://<cam>/local/multimuxedge/hls/medium/media.m3u8
```

**Main preset:** always available at `/local/multimuxedge/hls/main/media.m3u8`.

**Bare URL:** `/local/multimuxedge/hls/media.m3u8` always redirects (302) to either
`/<default_stream>/media.m3u8` (if a default stream is set) or `/main/media.m3u8`
(if no default stream is configured).

The old URL path `/local/multimuxedge/hls/cam/media.m3u8` is no longer available.
If you are migrating from an older version, update your clients to use the new
stream names.

## Stream 503 (Service Unavailable)

If a request to a stream URL returns 503, check the `Retry-After` header
(typically 5 seconds) and look at the response body for details:

### `encoder busy (N/M encodes in use)`

All available encoder slots are in use. Check `/admin/status` to see
`encodes.in_use` and `encodes.max`. Either:

- Wait for an idle stream to shut down (controlled by `idle_timeout_secs`
  in [Configuration](configuration.md)).
- Map the stream to a profile the camera already streams, so it joins that
  encode instead of needing its own.
- Increase `max_encodes` if the camera can handle more (the ARTPEC-6 fits
  about one and a half 4K25 encodes).
- Reduce the number of active streams or stop other apps using the encoder
  (e.g., a VMS client).

### `camera profile "X" not found`

The configured stream profile does not exist on this camera. Check available
profiles with:

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/profiles
```

Update the stream configuration to use a valid profile name.

### `profile source unavailable: <underlying message>`

The camera's profile exists but cannot deliver video at the moment. The underlying message
describes what failed: typically the camera's VAPIX profile service or the `streamprofile.cgi`
call (which times out after 5 seconds if the camera is unresponsive). Wait a few seconds
and retry. If the main stream is available, there is likely a temporary camera issue
rather than a permanent codec/hardware incompatibility.

### Unsupported codec

The configured or profile-overridden codec is not supported on this camera or
in this app build. Check [Supported Devices](supported-devices.md) to verify
your camera supports the codec you selected (e.g., H.265 is not available on
all SoCs).

## Playlist 404s

The origin serves LL-HLS nested under `/hls` inside the app
(`https://<cam>/local/multimuxedge/hls/...`). If your camera's reverse-proxy
strips the `apiPath` segment differently than expected, the nest prefix
seen by the app may not match. Confirm the actual proxied path reaching the
app and, if needed, this is a code-level fix — see the nest configuration
in `src/bin/multimux-edge.rs` and [Building](../contributor/building.md) for
how to rebuild.

## No video / garbled video

`convert` (the VDO access-unit → `transmux::Sample` conversion) assumes VDO
delivers Annex-B framing (start codes). If a given camera/VDO version emits
a different framing, this assumption breaks. This is a code-level issue —
see [Architecture](../contributor/architecture.md) for where `VdoIngestSession`
does this conversion.

## Low frame rate

Frame rate drops when the camera runs more distinct encodes than its
encoder fits; the P1448-LE (ARTPEC-6) fits about one and a half 4K25
encodes, shared with the camera's RTSP clients. Check `/admin/status`:

- A 4K stream with `"shared_encode": true` runs on an encode the camera
  already had and costs nothing extra.
- Each stream with `"shared_encode": false` is an encode of its own. Map
  streams to profiles the camera already streams (the same camera,
  resolution and frame rate) so they can join, or stop the extra streams.
- `videokeyframeinterval` on a profile only joins an encode with that exact
  interval, so leave it unset on 4K profiles.

See [Sharing the camera's encodes](configuration.md#sharing-the-cameras-encodes).

## Full verification checklist

For the complete on-device acceptance checklist (used when verifying a
build against real hardware), see
[Testing → Hardware verification checklist](../contributor/testing.md#hardware-verification-checklist).
