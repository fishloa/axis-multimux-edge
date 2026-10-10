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
      "name": "medium",
      "error": null
    }
  ]
}
```

- `last_error` non-null — a config load or capture setup failure at boot
  (e.g., a broken `axparameter` backend). The text describes why.
- `encodes.in_use >= encodes.max` — the encoder is at or over capacity.
  New stream requests will receive a 503.
- `streams[].error` non-null — that stream has failed; the error text
  describes why (profile not found, camera unavailable, etc.).

## Stream URLs

Streams are accessed at `/local/multimuxedge/hls/<name>/media.m3u8`, where
`<name>` is the configured stream name. For example:

```
https://<cam>/local/multimuxedge/hls/medium/media.m3u8
```

The old URL path `/local/multimuxedge/hls/cam/media.m3u8` is no longer
available. If you are migrating from an older version, update your clients
to use the new stream names.

The bare URL `/local/multimuxedge/hls/media.m3u8` redirects to the
`default_stream` if configured, otherwise returns a 404.

## Stream 503 (Service Unavailable)

If a request to a stream URL returns 503, check the `Retry-After` header
(typically 5 seconds) and look at the response body for details:

### `encoder busy (N/M encodes in use)`

All available encoder slots are in use. Check `/admin/status` to see
`encodes.in_use` and `encodes.max`. Either:

- Wait for an idle stream to shut down (controlled by `idle_timeout_secs`
  in [Configuration](configuration.md)).
- Increase `max_encodes` if the camera can handle more (the ARTPEC-6 handles
  about 2 distinct encodes at full frame rate).
- Reduce the number of active streams or stop other apps using the encoder
  (e.g., a VMS client).

### `camera profile "X" not found`

The configured stream profile does not exist on this camera. Check available
profiles with:

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/profiles
```

Update the stream configuration to use a valid profile name.

### `profile source unavailable: <reason>`

The camera's profile exists but cannot deliver video at the moment. Common
reasons:

- **`VDO channel not ready` or similar** — the VDO subsystem is initializing
  or has failed. Wait a few seconds and retry.
- **`Timeout waiting for keyframe`** — the camera is not producing frames
  fast enough. Check the camera's health and network connectivity.

If the issue persists, restart the Multimux Edge app.

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

## Full verification checklist

For the complete on-device acceptance checklist (used when verifying a
build against real hardware), see
[Testing → Hardware verification checklist](../contributor/testing.md#hardware-verification-checklist).
