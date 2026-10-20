# Fomkee Agent

`fomkee-agent` is a self-hosted Linux health collector. It exposes the same
cached, typed snapshot as compact JSON for a Fomkee Function monitor and as
OpenMetrics for Prometheus. A scrape never runs a collector.

This directory is an independent Rust workspace:

```bash
cd agent
cargo build --release
FOMKEE_AGENT_TOKEN='replace-me' \
  target/release/fomkee-agent --config examples/config.toml serve
```

The default listener is `127.0.0.1:9469`. `/health` is public but contains no
measurements. `/v1/snapshot` and `/metrics` require
`Authorization: Bearer $FOMKEE_AGENT_TOKEN`. Keep the listener private or put
it behind TLS; the bearer token does not encrypt traffic.

Inspect one collection cycle locally:

```bash
fomkee-agent --config /etc/fomkee-agent/config.toml snapshot --json
fomkee-agent --config /etc/fomkee-agent/config.toml validate
```

`validate` checks the closed TOML shape and domain values without opening
databases or requiring secrets. `snapshot` builds collectors and therefore
also validates their environment-backed credentials.

## Data contract

Every collector is either `collected` with numeric values and typed facts, or
`failed` with a bounded safe code and summary. Failure is never converted to
zero. Counters retain their raw cumulative value; after the first daemon cycle
the JSON view also includes a reset-aware `<name>_delta` gauge for the most
recent interval. This is why PostgreSQL's historical `temp_bytes_total` can be
large while `temp_bytes_total_delta` remains zero during a quiet interval.

The JSON representation intentionally omits Prometheus type/unit metadata:

```json
{
  "schema_version": 1,
  "sequence": 2,
  "generated_at": "2026-10-20T10:00:00Z",
  "collectors": {
    "disk_root": {
      "collected_at": "2026-10-20T10:00:00Z",
      "duration_ms": 1,
      "status": "collected",
      "values": { "used_ratio": 0.41 },
      "facts": {}
    }
  }
}
```

OpenMetrics includes only numeric measurements plus collector availability and
duration. Facts remain JSON-only rather than being guessed into metric labels.

## Collectors

- `system`: `/proc` uptime, 1-minute load, and memory.
- `filesystem`: bytes and inode usage for one path.
- `systemd`: active/sub-state and restart counter for one unit.
- `container`: Docker state, exit code, and restart counter for one container.
- `postgres`: database size, connections, temporary-file counters, deadlocks.
- `backup`: newest matching regular file, its size and age.
- `http_json`: explicitly mapped numeric JSON pointers.
- `openmetrics`: explicitly mapped metric names; matching labeled samples are
  summed, which is useful for Fomkee's per-protocol outbox depth.

HTTP source responses are capped at 1 MiB. Only mapped numeric fields are
retained. Source payloads and credentials never appear in the snapshot.
Configuration is capped at 256 KiB, with at most 64 collectors, 128 mappings
per mapped collector, 12 labels per series, and 8 KiB per secret.

## Fomkee check

Create a Fomkee Function monitor for the protected `/v1/snapshot` URL, add the
bearer header, and use [examples/fomkee-health-check.js](examples/fomkee-health-check.js).
Adjust collector IDs and thresholds to the deployment. Run Test check once with
a healthy response and once for each threshold before scheduling it.

Prometheus can scrape `/metrics` with the same bearer token when frequent
retention and graphs are desired. Fomkee remains responsible for the periodic
attention decision; the agent does not become a time-series database.

## Service installation

Install the release binary at `/usr/local/bin/fomkee-agent`, the configuration
at `/etc/fomkee-agent/config.toml`, and a root-readable environment file at
`/etc/fomkee-agent/agent.env`. Copy
[packaging/fomkee-agent.service](packaging/fomkee-agent.service) to systemd,
then enable it. The sample service does not grant Docker socket access; add the
dedicated runtime group only when the container collector is required, after
accepting that Docker socket access is effectively privileged.

The Caddy example terminates TLS without putting credentials in a URL. Limit
network access to the Fomkee checker and/or Prometheus in addition to bearer
authentication.
