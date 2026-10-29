# Fomkee Agent

[![CI](https://github.com/fomkee/agent/actions/workflows/verify.yml/badge.svg)](https://github.com/fomkee/agent/actions/workflows/verify.yml)
[![Release](https://img.shields.io/github/v/release/fomkee/agent)](https://github.com/fomkee/agent/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`fomkee-agent` is a self-hosted Linux health collector. It exposes the same
cached, typed snapshot as compact JSON and OpenMetrics for Prometheus. It can
also push that compact snapshot to a Fomkee Heartbeat monitor for JavaScript
body validation. A scrape or publish never runs a collector.

## Install

Download the archive for your Linux architecture from the
[latest release](https://github.com/fomkee/agent/releases/latest), verify it
against `SHA256SUMS`, and install the binary:

```bash
curl -fLO https://github.com/fomkee/agent/releases/latest/download/fomkee-agent-Linux-x86_64.tar.gz
curl -fLO https://github.com/fomkee/agent/releases/latest/download/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf fomkee-agent-Linux-x86_64.tar.gz
sudo install -m 0755 fomkee-agent-Linux-x86_64/fomkee-agent /usr/local/bin/fomkee-agent
```

Use `fomkee-agent-Linux-arm64.tar.gz` on ARM64 Linux. Each archive also
contains the example configuration, systemd unit, README, and Apache-2.0
license.

To build from source, use the independent Rust workspace:

```bash
cargo build --release
FOMKEE_AGENT_TOKEN='replace-me' \
  target/release/fomkee-agent --config examples/config.toml serve
```

The default listener is `127.0.0.1:9469`. `/health` is public but contains no
measurements. `/v1/snapshot` and `/metrics` require
`Authorization: Bearer $FOMKEE_AGENT_TOKEN`. Keep the listener private or put
it behind TLS; the bearer token does not encrypt traffic.

When `[heartbeat]` is configured, the agent reads its secret URL from the
named environment variable. A missing variable leaves publishing disabled and
logs a warning; an installed URL must use HTTPS. Every new snapshot is posted
with a ten-second request timeout and redirects disabled. Delivery failures are
reported and retried with the next snapshot.

`schedule.interval_secs` is the default collection cadence. Every collector
may override it with a positive `interval_secs`; omitted overrides retain the
existing global behavior. The scheduler runs only due collectors, never
overlaps a collector with itself, and keeps the most recent observation from
slower collectors in each published snapshot:

```toml
[schedule]
interval_secs = 30
collector_timeout_secs = 10
max_concurrency = 4

[[collectors]]
type = "filesystem"
id = "disk_root"
path = "/"
mount_requirement = "mountpoint"
interval_secs = 120
```

Repeat the filesystem collector for every disk or mounted volume. On Linux,
`mount_requirement = "mountpoint"` verifies that the configured path is an
exact active mount before reporting capacity. This prevents an absent data
disk from being mistaken for its backing root filesystem. Omit the setting (or
use `"path"`) when monitoring an ordinary directory is intentional.
Linux facts distinguish the underlying filesystem's `read_only` state from
`agent_view_read_only`, which can be true when systemd hardening gives the
agent a restricted mount namespace.

The default local TUI socket is `/run/fomkee-agent/agent.sock`. The packaged
systemd unit creates its parent runtime directory and removes it when the
service stops. A non-systemd development configuration should point the socket
at a private temporary directory owned by the current user.

Inspect one collection cycle locally:

```bash
fomkee-agent --config /etc/fomkee-agent/config.toml snapshot --json
fomkee-agent --config /etc/fomkee-agent/config.toml validate
```

With the daemon running, open the live terminal dashboard over the local Unix
socket:

```bash
fomkee-agent --config /etc/fomkee-agent/config.toml tui
```

The default Dashboard page recognizes built-in collectors and presents their
data according to its meaning: capacity bars for memory and a compact aligned
table for all filesystems; session trends for load and PostgreSQL connections;
state and restart activity for services and containers; database growth and
counter activity; and backup freshness. Press `2` to open the Collectors page, which preserves a
complete technical view of built-in and custom mapped data. Press `1` to
return, Tab or left/right to switch pages, and `q` or Escape to exit. Arrow
keys or `j`/`k` scroll the dashboard or select a collector on the Collectors
page.

Values are formatted from the snapshot's typed unit metadata. The `[tui]`
configuration selects the initial page and the bounded history capacity,
which defaults to 120 snapshots. Each panel shows when its collector last ran,
its configured cadence, and an overdue indication after two missed cadences.
History starts when the TUI opens, is neither
persisted nor loaded from Prometheus, and is used only for session trends and
deltas. Republishing a cached slower observation does not add another trend
point or counter delta. Custom `http_json` and `openmetrics` measurements remain available on
the Collectors page; the agent does not guess dashboard semantics for them.
The TUI never runs collectors. Access is controlled by the socket's `0660`
mode and the `fomkee-agent` service group, so an operator may need group
membership or `sudo -u fomkee-agent` depending on installation policy.

```toml
[tui]
history_snapshots = 120
default_view = "dashboard" # or "collectors"

[tui.capacity]
warning_percent = 80
critical_percent = 90
```

Storage rows include text statuses so color is not the only signal. Green,
yellow, and red reinforce normal, warning/stale, and critical/failure states.
Capacity thresholds also color the memory bar and must satisfy
`0 < warning_percent < critical_percent <= 100`.

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
      "interval_secs": 120,
      "duration_ms": 1,
      "status": "collected",
      "values": { "used_ratio": 0.41 },
      "facts": {}
    }
  }
}
```

OpenMetrics includes only numeric measurements plus collector availability and
duration. It also exposes `fomkee_agent_collector_interval_seconds` and
`fomkee_agent_collector_last_collection_timestamp_seconds`, allowing a
Prometheus rule to compare `time()` with the last collection time. Facts remain
JSON-only rather than being guessed into metric labels.

## Collectors

- `system`: `/proc` uptime, 1-minute load, and memory.
- `filesystem`: bytes and inode usage for one path, with optional exact Linux
  mount-point verification and mount metadata.
- `systemd`: active/sub-state and restart counter for one unit.
- `container`: Docker state, exit code, and restart counter for one container.
- `postgres`: database size, connections, temporary-file counters, deadlocks.
- `backup`: newest matching regular file, its size and age.
- `http_json`: explicitly mapped numeric JSON pointers.
- `openmetrics`: explicitly mapped metric names; matching labeled samples are
  summed, which is useful for Fomkee's per-protocol outbox depth. A mapping can
  select exact labels with `labels = { outcome = "acknowledged" }`. Set
  `required = false` for series that are legitimately absent, such as the age
  of the oldest item in an empty queue.

Filesystems that do not report inode capacity omit inode measurements and set
`facts.inode_metrics_available` to `false`; unavailable capacity is not zero usage.

HTTP source responses are capped at 1 MiB. Only mapped numeric fields are
retained. Source payloads and credentials never appear in the snapshot.
Configuration is capped at 256 KiB, with at most 64 collectors, 128 mappings
per mapped collector, 12 labels per series, and 8 KiB per secret.

## Fomkee check

The preferred private deployment creates one Fomkee Heartbeat monitor per
server and configures its body validator using
[`examples/fomkee-health-check.js`](examples/fomkee-health-check.js) as a
starting point.
Store the returned secret URL only in that server's root-owned agent environment
file as `FOMKEE_AGENT_HEARTBEAT_URL`. This avoids publishing the agent snapshot,
and a stopped agent is detected as a missed heartbeat. Adjust collector IDs and
thresholds to the deployment and dry-run the validator before enabling it.

A pull-based Function monitor remains possible when `/v1/snapshot` is already
available through an operator-controlled private network and TLS. Do not expose
the endpoint publicly merely to support a Function monitor.

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
