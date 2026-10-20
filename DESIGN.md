# Fomkee Agent Design

`fomkee-agent` periodically collects bounded host and service observations and
publishes one immutable latest snapshot. HTTP reads never start collection.

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
