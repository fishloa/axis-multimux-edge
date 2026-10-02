# Troubleshooting

## Check `/admin/status` first

```bash
curl -u <user>:<pw> https://<cam>/local/multimuxedge/admin/status
```

Returns:

```json
{
  "running": true,
  "current_segment": 5,
  "current_part": 2,
  "frames": 1234,
  "last_error": null
}
```

- `running: false` or a non-null `last_error` — the capture pipeline itself
  has failed; the error text describes why.
- `last_error` set but `running: true` — this can be a **stale config-store
  error** (a config load failure at boot, e.g. a broken `axparameter`
  backend) rather than a live pipeline fault. It surfaces here because
  nothing else clears it, not because the pipeline is currently unhealthy.
- `current_segment`/`current_part`/`frames` not advancing over repeated
  polls — media is not flowing even though the process is up. Check VDO
  itself (is another app already holding the channel?) before assuming
  `multimux-edge` is at fault.

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

## Config changes not taking effect

Config is **not** hot-reloaded — see [Configuration](configuration.md).
`POST` the new config, then restart the app.

## Full verification checklist

For the complete on-device acceptance checklist (used when verifying a
build against real hardware), see
[Testing → Hardware verification checklist](../contributor/testing.md#hardware-verification-checklist).
