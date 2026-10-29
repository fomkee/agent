# Changelog

## 0.2.0

- Added a live terminal dashboard backed by the same bounded snapshots served
  as JSON and OpenMetrics.
- Added independent per-collector schedules, mounted-filesystem verification,
  typed storage status, and bounded in-memory TUI history.
- Added direct Heartbeat publishing so a stopped agent is detected without
  exposing its local snapshot endpoint.
- Added bounded retries for transient Heartbeat delivery failures, including
  brief load-balancer and node-release interruptions.
- Added least-privilege systemd packaging and production-oriented examples.

## 0.1.0

- Initial self-hosted Linux health collector with typed JSON and OpenMetrics
  projections, bounded collectors, bearer authentication, and configuration
  validation.
