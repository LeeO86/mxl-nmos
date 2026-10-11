# mxl-nmos — Specification: extract the NMOS node from the Strom fork into a crate

Status: Draft v0.2. This document is both the crate's specification and the extraction
task. It is carried out in the `LeeO86/strom` repository, with write access to this
repository `LeeO86/mxl-nmos`.

Changelog: v0.2 adds NMOS behaviour learned while running the sibling media functions on a
Kubernetes MXL platform: TAI scheduling,
asynchronous activation, `master_enable`, version and registration order, CORS, AMWA
notes, readiness vs liveness, and bulk semantics ("Platform lessons" below). The
platform contract G1–G14 that the crate's users meet is written out in
`LeeO86/mxl-audio-mixer` `SPECIFICATION.md`, Appendix A.

---

## Goal

Strom's NMOS node (IS-04 v1.3, IS-05 v1.2, AMWA BCP-007-03, platform contract v1.0.0)
becomes a standalone, reusable Rust crate **`mxl-nmos`** in its own repository. Strom
then uses the crate instead of its built-in code, with no behaviour change. The second
consumer will be `LeeO86/mxl-audio-mixer` (Rust, see its `SPECIFICATION.md` §3.1),
so the crate's API must be neutral: it must not know about Strom flows, GStreamer or
any other application model.

## Working rules

- First, read Strom's NMOS code, its tests, the AMWA test setup, `CHANGELOG`, and the
  v1.0.0 platform contract changes (PRs #9, #11, #13). Then write
  `docs/extraction-plan.md` in the new repo: which modules move, what stays in Strom
  (the glue between NMOS and Strom flows), the proposed public API, the licence
  situation, and open questions. Stop and ask me if anything is unclear or if the
  licence does not allow the extraction as planned.
- **Licence:** the code originates from a fork of Eyevinn's Strom. Keep its licence
  for the extracted code (copy the licence file and copyright notices unchanged). If it
  is MIT or Apache-2.0, license the crate the same way (or dual MIT/Apache-2.0 if the
  original allows it). Record the reasoning in the README.
- No behaviour change for Strom in this task. In particular, do **not** fix Strom's
  "a connection change restarts the whole flow" behaviour here; that is a separate
  task. But the crate API must make a fix possible (per-receiver activation, below).
- Small, reviewable commits/PRs in both repos. Keep the git history of the moved files
  if practical (e.g. `git filter-repo` on the NMOS paths); otherwise reference the
  source commit in the first commit message.

## Crate scope

### Must have (v0.x, needed by Strom and the mixer)

- **Node model:** one Node; one or more Devices; Sources, Flows, Senders, Receivers
  with the BCP-007-03 MXL transport (`urn:x-nmos:transport:mxl`), formats video,
  audio, data, and the media types used on the platform (`video/v210`, `video/v210a`,
  `audio/float32`, `video/smpte291`).
- **Dynamic resources:** add, update and remove Devices, Sources, Flows, Senders and
  Receivers at runtime; the crate bumps `version` timestamps and updates the registry.
- **IS-04 Node API v1.3** and **IS-05 Connection API v1.2** served by the crate:
  `staged`/`active`/`constraints`/`transporttype`, `transportfile` 404 for MXL senders,
  empty `interface_bindings`, `/bulk` endpoints for senders and receivers, scheduled
  activations (relative/absolute) as well as immediate, correct NMOS error format.
- **Activation callback, per resource:** on activation of a receiver or sender the
  crate calls the application with the resolved new transport parameters
  (`mxl_domain_id`, `mxl_flow_id`, `master_enable`) for **that one resource**; the
  application returns accept/reject (with reason). The crate updates `active` and the
  IS-04 `subscription` only on accept. No assumption about restarting anything.
- **Sender auto values:** support `auto` for sender `mxl_domain_id`/`mxl_flow_id` as
  Strom does today; receivers reject `auto` for `mxl_flow_id`.
- **BCP-004-01** receiver capabilities (constraint sets) and **BCP-002-01** group hints.
- **Identity:** UUIDv5 from `NMOS_SEED` plus a stable resource key supplied by the
  application; `NMOS_LABEL`, `NMOS_TAGS` on Node and Devices.
- **Registration:** static registry (`NMOS_REGISTRY_ADDRESS`/`_PORT`), heartbeats,
  re-registration after registry restart or 404, backoff; Query API check for
  readiness (`NMOS_QUERY_ADDRESS`/`_PORT`, default registration port + 1);
  `is_registered()` for `/readyz`.
- **Addresses:** hrefs and `api.endpoints` announce `NMOS_HOST_ADDRESS` (IPv4 literal)
  or the first non-loopback IPv4, never `0.0.0.0` or a hostname.
- **DNS-SD:** off by default (`NMOS_DNS_SD=false`); mDNS/DNS-SD browse and advertise
  behind a cargo feature `dns-sd`, disabled by default.
- **Shutdown:** `shutdown()` deletes all resources from the registry (Node last) within
  a timeout, so applications can meet G8.
- **Persistence hook:** the application can save and restore the active IS-05 state of
  every resource (G9: connections survive a restart).
- **Configuration:** a settings struct, plus an optional helper that reads the
  platform-standard environment variables above (G1 names); the application can also
  fill the struct itself.

### Platform lessons (must also be met)

- **Scheduled activations use TAI.** IS-05 `activate_scheduled_absolute` times are TAI
  `seconds:nanoseconds`; relative ones count from the TAI now. Compare with CLOCK_TAI,
  not UTC, or absolute activations land 37 s off. Unit-test this with an injected
  clock.
- **The activation callback is asynchronous, with a timeout** (configurable, e.g. 2 s).
  "Accept" means the application took the new parameters. It does *not* mean the flow
  is open: a receiver whose flow does not exist yet is accepted and the application
  reports `waiting` until the flow appears. On the platform, the fabrics agent mirrors
  a flow to a node *because* a receiver there was activated, so the flow often comes
  after the activation. The IS-05 response must never block on media.
- **`master_enable: false`** is a normal activation (the application stops reading),
  not a reject; `active` shows it.
- **Persistence restore calls the callback** for every restored resource, so the
  application re-applies its connections at start (G9).
- **`mxl_flow_id` may differ from the IS-04 Flow id** (BCP-007-03). The crate never
  assumes they are equal; the application supplies both.
- **Resource `version`** is strictly increasing per resource, even for two changes in
  the same nanosecond (TAI `seconds:nanoseconds`).
- **Registration order:** Node → Device → Source → Flow → Sender/Receiver; deletion in
  reverse, the Node last. After a 404 on a heartbeat, register everything again in
  order.
- **CORS** headers on every Node API and Connection API response, including errors and
  OPTIONS. The AMWA IS-04-01 suite checks them.
- **Readiness vs liveness:** `is_registered()` feeds `/readyz` only. Nothing in the
  crate may make an application's liveness depend on the registry, because a
  restarting registry must not restart every pod.
- **Bulk activations** call the callback once per resource. Document whether one reject
  fails the whole bulk request or only that entry (IS-05 allows per-entry results;
  prefer per-entry), and test it.
- **BCP-004-01 receiver caps carry a `version`** (a sibling project needed it to pass the
  AMWA run).

### Integration shape

- Async on **tokio**; HTTP with **axum**. The crate offers both:
  - its own listener on `NMOS_PORT` (default for most apps), and
  - an `axum::Router` that an application can merge into its own server.
- WebSocket for IS-04 events only if Strom needs it today; otherwise not in scope.
- Public API documented with rustdoc and a small example node (`examples/mxl_node.rs`)
  with one MXL video sender, one audio receiver, a fake activation handler, and
  registration against a registry.
- No GStreamer, no Strom types, no MXL SDK dependency in the crate (MXL IDs are UUIDs
  and strings here; the applications talk to MXL themselves).
- Optional: use the `nmos` crate (st2110/crate-nmos) for schema-tested IS-04/IS-05
  types if its licence fits and it reduces code; otherwise keep Strom's types. Record
  the decision.

### Not in scope

Registry or Query API server, controller functions, IS-07/08/11/12, SDP for RTP,
authorisation (IS-10).

## Tests and CI (in `mxl-nmos`)

- Unit tests: UUIDv5 derivation, resource add/update/remove and version bumps,
  staged→active state machine (immediate, scheduled, bulk), activation accept/reject,
  subscription updates, `auto` handling, error responses, settings from env.
  Also the platform lessons above: TAI scheduling with an injected clock, a callback
  timeout, `master_enable: false`, restore → callback, strictly increasing versions,
  registration order and re-registration after a 404, CORS on errors and OPTIONS, and
  per-entry bulk results.
- JSON validation of every served resource and IS-05 document against the official
  AMWA schemas (IS-04 v1.3, IS-05 v1.2) in the test suite.
- Integration: the example node against an nmos-cpp registry container — registers,
  heartbeats, re-registers after registry restart, deregisters on shutdown, readiness
  via Query API.
- **AMWA NMOS Testing in CI** against the example node: IS-04-01, IS-05-01, IS-05-02,
  BCP-007-03-01 — all must pass.
  - With DNS-SD off (the default), the tests that need DNS-SD cannot run (in IS-04-01,
    e.g. the mock-registry tests). List the expected "Could Not Test"/N/A results in the
    CI configuration and fail on any other result, so a new failure is never hidden
    among them. A second job with the `dns-sd` feature on SHOULD run them.
  - Pull the testing-tool image from GHCR or a mirror, or with a Docker Hub login: the
    sibling repos' CI failed repeatedly on Docker Hub rate limits.
- Release: semver tags `vX.Y.Z` (start at `v0.1.0`; `v1.0.0` once both Strom and the
  mixer use it), `CHANGELOG.md`, README with usage and the settings table.

## Switching Strom to the crate

- Strom depends on `mxl-nmos` by **git tag** (not branch).
- Strom keeps only the glue: mapping Strom flows/blocks to NMOS Devices and resources,
  and its activation handler (which, for now, keeps today's behaviour of restarting the
  flow).
- All existing Strom NMOS tests and the AMWA BCP-007-03-01 run must stay green; the
  platform contract v1.0.0 behaviour (settings, readiness, shutdown, export/import) must
  be unchanged. Add a short note to Strom's CHANGELOG.
- Note in Strom's `IMPLEMENTATION_PLAN.md` (or equivalent) that the per-receiver
  activation callback now makes the "no flow restart on routing change" fix possible,
  as a follow-up task.

## Deliverables

1. New repo `LeeO86/mxl-nmos`: crate, example, tests, CI with AMWA suites, README,
   licence files, `docs/extraction-plan.md`, release `v0.1.0`.
2. PR in `LeeO86/strom` switching to `mxl-nmos v0.1.0`, all tests green.
3. A short summary for me: public API overview, what changed in Strom, open points.
