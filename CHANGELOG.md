# Changelog

## Unreleased

- `NMOS_REGISTRY_ADDRESS` and `NMOS_QUERY_ADDRESS` accept a DNS name (for example a
  Kubernetes Service name) as well as an IPv4 literal. `NMOS_HOST_ADDRESS` stays an
  IPv4 literal.
- Scheduled activations call the handler at their time, not up to about 1 ms
  later: the activation is prepared 2 ms early and the last wait blocks on the
  system clock (`Clock::block_until`) instead of tokio's 1 ms timer. A reader of
  `/active` right after the time saw the old state (AMWA IS-05-01 test_28 warned
  "later than expected" in about 15 % of the runs against the mixer).

## 0.1.0 - 2026-10-11

First release: the NMOS node of `LeeO86/strom` 1.0.1 as a standalone crate.

- IS-04 v1.3 Node API, IS-05 v1.2 Connection API, BCP-007-03 MXL transport.
- One resource per call (`insert_device`, `insert_source`, `insert_flow`,
  `insert_sender`, `insert_receiver`, `erase`), as the siblings' nmos-cpp nodes.
- Activation callback per resource with a timeout; accept/reject; `active`,
  `subscription` and `version` change only on accept.
- Scheduled activations (absolute, relative) on `CLOCK_TAI`, with 423 locking and
  cancellation; per-entry bulk results.
- Registration in order, heartbeats, re-registration after a 404 or a stale 200,
  backoff, Query API readiness, deregistration of every resource with the Node last.
- `Settings::apply_env` for the platform names; DNS-SD behind the `dns-sd` feature,
  off by default.
- Persistence hook (`connections`, `on_connections_changed`, `restore`).

Changes against Strom 1.0.1's built-in node:

- `version` and `activation_time` are TAI (`CLOCK_TAI`), not UTC; versions strictly
  increase and change on every activation.
- Scheduled activations work (they answered 400 before).
- A heartbeat 404 re-registers; deregistration uses the plural IS-04 paths
  (`/resource/nodes/{id}`) and deletes every resource.
- The Node `href` is the API base URL; Devices list the Connection API in `controls`;
  audio Sources carry `channels`; bulk errors carry `error` and `debug`.
- DNS-SD browses `_nmos-register._tcp` (v1.3) as well and advertises the node only in
  peer-to-peer mode, with `api_auth`.
