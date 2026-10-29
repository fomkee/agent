# Fomkee Agent Design

`fomkee-agent` periodically collects bounded host and service observations and
publishes one immutable latest snapshot. HTTP reads never start collection.

The global schedule interval is a backward-compatible default. A collector may
override its cadence; the scheduler waits for the earliest deadline, admits
only due collectors through the shared concurrency bound, and schedules the
next run after completion so one collector cannot overlap itself. Published
snapshots merge newly collected observations with the latest cached
observations from slower collectors. Collection time and typed cadence travel
with each observation, and counter deltas are derived only when that collector
actually runs.

The canonical model distinguishes numeric gauges and counters from typed facts.
Compact JSON is intended for Fomkee Function checks; OpenMetrics is an optional
projection for Prometheus. A failed collector remains explicit and is never
rendered as a healthy zero.

The daemon binds to loopback by default. `/health` is unauthenticated and only
states process readiness. `/v1/snapshot` and `/metrics` require the configured
bearer token. Configuration is closed and rejects unknown fields.

Built-in and mapped HTTP collectors implement the same `Collector` port.
Arbitrary executable and dynamic-library plugins are deliberately excluded;
the future extension boundary is a process-isolated protocol.

The Ratatui client is a separate command and does not own collection. The
daemon streams bounded canonical snapshots over a group-protected Unix socket;
HTTP, OpenMetrics, and TUI therefore observe the same published state.
The client retains a bounded, in-memory snapshot window only while it runs.
Its default semantic dashboard is derived from configured built-in collector
types: capacity is shown relative to the available total, state is explicit,
and history is used only for session trends, changes, and counter activity.
Custom mapped collectors remain in a complete technical browser because unit
metadata alone cannot determine the operational meaning of an arbitrary
measurement. The validated `[tui]` configuration owns the initial page and
history capacity (120 snapshots by default); future semantic panel settings
can extend that boundary without changing collection or snapshot transport.
Cached observations are deduplicated by collector and collection timestamp
before TUI trends and session activity are calculated.
