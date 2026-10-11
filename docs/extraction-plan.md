# Extraction plan: Strom's NMOS node → `mxl-nmos`

Status: agreed. The maintainer answered the open questions on 2026-10-11 (§10); this
document reflects those answers.

Source revision: `LeeO86/strom` `main` at `c2e27a6` (release 1.0.1).

## 1. What was read

- `backend/src/nmos/` (6 files, 3,064 lines): `mod.rs` (wiring + the module's tests),
  `node.rs` (resource model, IS-04/IS-05 handlers), `http.rs` (axum routes, CORS),
  `register.rs` (registration client, mDNS), `settings.rs`, `domain.rs` (MXL domain
  files).
- The glue: `backend/src/state/mod.rs` (`install_nmos`, `nmos_ready`, `apply_nmos_mxl`),
  `backend/src/config.rs` (platform env names, `STROM_*` aliases, registry and Query
  URLs), `backend/src/api/platform.rs` (`/livez`, `/readyz`, `/metrics`, export/import),
  `backend/src/main.rs` (start, SIGTERM → 143), `backend/src/network.rs` (interfaces).
- `docs/NMOS_MXL.md`, `docs/CHANGELOG.md`, `IMPLEMENTATION_PLAN.md` (G1–G14 audit),
  PRs #9 (node), #11 (BCP-007-03 suite fixes), #13 (platform contract v1.0.0), and the
  git history of the NMOS files (5 commits).
- AMWA test setups in the sibling repos. Strom has **no AMWA job in CI**: PR #11 records
  one manual run of BCP-007-03-01 (nmos-testing `9001851`: 55 passed, `test_15` manual,
  14 IS-10 checks disabled, 3 not applicable). The only automated AMWA run in the family
  is `LeeO86/mxl-st2110-gateway` `tests/integration/nmos-testing.sh`, which installs
  nmos-testing from source at a pinned commit (no Docker Hub).
- The optional `nmos` crate (`st2110/crate-nmos` 1.2.0).

## 2. Licence

- Strom (and its upstream, Eyevinn's Strom) is **dual-licensed `MIT OR Apache-2.0`**:
  workspace `Cargo.toml` `license = "MIT OR Apache-2.0"`, README "License: MIT OR
  Apache-2.0", `LICENSE-MIT` ("Copyright (c) 2025 Eyevinn Technology") and
  `LICENSE-APACHE` (appendix "Copyright 2025 Eyevinn Technology").
- The NMOS module was written in the fork (first commit `5bb13f6`, Sept 2026) and was
  contributed under the repository's licence. There is no CLA or separate notice.
- Therefore the extraction is allowed as planned:
  - `mxl-nmos` is licensed **`MIT OR Apache-2.0`**, the same as the code's origin;
  - `LICENSE-MIT` and `LICENSE-APACHE` are copied from Strom **unchanged**, including
    their copyright lines;
  - the README states the provenance (extracted from the NMOS node of `LeeO86/strom`,
    a fork of Eyevinn's Strom, at `c2e27a6`) and this reasoning.
- The mixer is MIT; an `MIT OR Apache-2.0` dependency can be used under MIT.
- Dependencies (all compatible): `axum` MIT, `tower-http` MIT, `tokio` MIT, `tracing`
  MIT, `reqwest` / `serde` / `serde_json` / `uuid` / `futures` / `libc` /
  `network-interface` / `mdns-sd` (feature `dns-sd`) MIT OR Apache-2.0, `hostname` MIT.
  Test-only: `jsonschema` MIT, `tempfile` MIT OR Apache-2.0, `tower` MIT. The AMWA JSON
  schemas vendored for tests are Apache-2.0; they keep their own `LICENSE` file next to
  them. CI runs `cargo deny check licenses` with an allow-list of MIT, Apache-2.0,
  BSD-2/3-Clause, ISC, Unicode-3.0, Zlib.
- **`nmos` crate: not used.** It is Apache-2.0 only (compatible), but it targets ST 2110:
  no MXL transport, IS-05 schemas only for v1.1, no `/bulk`. Using it would add code
  (MXL types on top) rather than remove it. Strom's `serde_json` documents stay, and the
  test suite validates them against the official AMWA schemas instead.

## 3. What moves and what stays

| Strom file | Goes to | Notes |
| --- | --- | --- |
| `nmos/node.rs` | `src/model.rs`, `src/is04.rs`, `src/is05.rs` | Resource model and handlers, without `strom_types` (`Flow`, `BlockInstance`, block ids). |
| `nmos/http.rs` | `src/http.rs` | Router, CORS layer, own listener. |
| `nmos/register.rs` | `src/registration.rs`, `src/dns_sd.rs` | mDNS part behind feature `dns-sd`. |
| `nmos/settings.rs` | `src/settings.rs` | Without Strom defaults (`label = "Strom"`, port 8080). Plus the env helper (from `config.rs`). |
| `nmos/domain.rs` | stays in Strom | G2 domain files are each application's job (Q6). The crate only receives the list of visible domains (id, label, path) from the application. |
| `nmos/mod.rs` tests | `tests/` of the crate | Re-written against the neutral API. The Strom copies stay in Strom (§8). |
| `config.rs`: `parse_tags`, `registry_url`, `query_base_url`, `parse_bool_env`, `require_announce_ipv4`, `first_routable_ipv4` | `src/settings.rs`, `src/address.rs` | The platform env names (G1/G3/G4/G5). Strom keeps its `STROM_*` aliases and `.strom.toml` layering and fills the struct itself. |
| `network.rs` (interface discovery) | `src/address.rs` (small subset) | Only what `interfaces[]` and the announce address need. |

What **stays in Strom** (the glue, ~250 lines expected):

- mapping flows to Devices (`device key = flow id`) and MXL blocks to senders/receivers
  (`resource key = "{flow_id}:{block_id}"`, which keeps every NMOS id unchanged, §5.3);
- the fixed formats Strom announces today (1080p50 v210, 2-ch float32) and the receiver
  caps, now passed in as data;
- the periodic sync (every 1 s, as today) from the flow list to the crate's
  insert/erase calls, and the "seed staged/active from block properties until the
  first IS-05 patch" rule;
- `domain.rs` (domain scan, output domain create/remove, seed-derived output domain id)
  and the `auto` rules that need the filesystem (preferred domain path, the domain that
  holds `{flow}.mxl-flow`), passed to the crate as its domain list and `on_resolve_auto`;
- the activation handler `apply_nmos_mxl` (writes domain path and flow id onto the block
  and **restarts the flow**, unchanged);
- `/readyz`, `/metrics`, export/import, SIGTERM handling, `STROM_*` aliases.

## 4. Strom today vs. the crate spec

The crate must meet its spec, so a few behaviours change for Strom. The rule "no
behaviour change for Strom" and the "Platform lessons" conflict on these points; the
proposal is in the last column and needs your OK (Q1).

| Topic | Strom 1.0.1 today | Crate spec | Proposal |
| --- | --- | --- | --- |
| Scheduled activations | Rejected with 400 "not implemented" | Relative and absolute, TAI | Implement. Strom gains them; requests that failed before now succeed. |
| Timestamps | `tai_now()` is UTC (`SystemTime`), 37 s behind TAI | CLOCK_TAI | Use CLOCK_TAI on Linux. Strom's `version` / `activation_time` values move +37 s (a correction). On non-Linux builds (Strom ships Windows/macOS binaries): UTC + 37 s. |
| `version` | Can repeat within the same ns | Strictly increasing per resource | `max(now, last + 1 ns)`. |
| Activation callback | Awaited without timeout; error → 500 | Async with timeout, accept/reject | Timeout configurable (default 2 s). Strom sets 30 s (Q2) because its handler restarts a flow. Reject or timeout → 500 with the reason in `debug`, `active` unchanged. |
| `master_enable: false` | Callback only when the flow is running | Normal activation | Always call the callback; Strom's handler already does nothing for a stopped flow, so Strom's result is the same. |
| Re-registration | After a heartbeat 404 the stale "already registered" map is kept, so nothing is re-posted (bug) | Re-register everything in order after a 404 | Clear the map on 404 and re-register Node → Device → Source → Flow → Sender/Receiver. |
| Backoff | Fixed: retry at the next 5 s heartbeat | Backoff | Exponential, 1 s doubling to a 10 s cap, reset on success. |
| Deregistration | DELETE of the Node only | DELETE of every resource, reverse order, Node last, within a timeout | Implement; `shutdown(timeout)`. |
| Readiness | Registration POST + heartbeat OK. `query_base` is computed in `config.rs` but never used | Query API check | `is_registered()` = heartbeat OK **and** the Query API lists the node, when a Query base is set. Strom passes its `query_base`, so its readiness becomes stricter (Q3). |
| Node `hostname` | Container hostname (= pod name) | G5: never announce a hostname or pod name | **Kept** (Q4): the nmos-cpp siblings announce the system hostname there too (nmos-cpp's `host_name` default); addresses (`href`, `api.endpoints`) stay IP literals. |
| Audio Source | No `channels` (required by IS-04 v1.3 `source_audio.json`) | Schema-valid resources | Add `channels` from the flow's channel labels. |
| mDNS advertisement | No `api_auth` TXT record (required for v1.3) | — (DNS-SD job SHOULD pass) | Add `api_auth=false` in the `dns-sd` feature. |
| Bulk | Per-entry codes, no `error`/`debug` | Per-entry results, documented | Keep per-entry (one reject fails only that entry); add `error` and `debug` per entry, as the IS-05 bulk response schema allows. |

Unchanged for Strom: ids, labels, descriptions, tags, group hints, routes under
`/x-nmos`, CORS headers, `transportfile` 404, empty `interface_bindings`,
`auto` rules, MAC `port_id`, receiver caps with `version: "0:0"`, settings and their
defaults, output domain files, export/import, exit codes.

## 5. Public API

Shaped like the nmos-cpp nodes of the sibling repos (Q5): one resource per call, as
`nmos::make_device` / `make_*_source` / `make_*_flow` / `make_sender` /
`make_receiver` + `insert_resource`, and the callbacks set on a builder as in
`nmos::experimental::node_implementation()`. Rustdoc on every public item.

### 5.1 Settings

```rust
pub struct Settings {
    pub label: String,                    // NMOS_LABEL; node label and device label prefix
    pub description: String,              // node description (Strom: "Strom NMOS node for MXL")
    pub seed: Option<String>,             // NMOS_SEED
    pub node_id: Option<Uuid>,            // explicit id when no seed
    pub id_path: Option<PathBuf>,         // persisted random id when neither is set
    pub tags: BTreeMap<String, Vec<String>>, // NMOS_TAGS, on Node and Devices
    pub host: Option<Ipv4Addr>,           // NMOS_HOST_ADDRESS; else first non-loopback IPv4
    pub port: u16,                        // NMOS_PORT; the port announced (and bound by `serve`)
    pub https: bool,
    pub registration: bool,               // register at all (Strom: NMOS_ENABLED)
    pub registry: Option<String>,         // base URL, from NMOS_REGISTRY_ADDRESS / _PORT
    pub query: Option<String>,            // base URL, NMOS_QUERY_ADDRESS / _PORT (default registry, port + 1)
    pub dns_sd: bool,                     // NMOS_DNS_SD, default false; needs feature `dns-sd`
    pub service_name_prefix: String,      // mDNS instance name prefix (Strom: "strom")
    pub heartbeat: Duration,              // 5 s
    pub activation_timeout: Duration,     // 2 s (Strom: 30 s)
}

impl Settings {
    /// Apply the platform env names on top of `self`; returns the names that were set
    /// (so an app can show each setting's origin). Invalid values -> Err (app exits 78).
    pub fn apply_env(&mut self) -> Result<Vec<&'static str>, SettingsError>;
}
```

Strom keeps its own config chain and fills `Settings` itself. The registry is a base
URL so Strom's legacy `STROM_NMOS_REGISTRY` (full URL) keeps working.

### 5.2 Implementation callbacks

```rust
let implementation = NodeImplementation::new(on_activation)  // required, §5.4
    .on_resolve_auto(resolver)        // optional; default: §5.5
    .domains(provider)                // Fn() -> Vec<MxlDomain { id, label, path }>
    .on_connections_changed(saver);   // optional persistence hook, §5.6
let node = Node::new(settings, implementation)?;  // Err if the announce address is invalid
```

The domain provider is called on each request and each registration tick (Strom
rescans on each request today). The crate reads no files; scanning `domain_def.json`
stays in the application (Q6).

### 5.3 Resources

```rust
node.insert_device(Device::new(key, name))?;                       // label = "{NMOS_LABEL} {name}", NMOS_TAGS added
node.insert_source(Source::video(key, device_key, grain_rate))?;   // ::audio(.., channels), ::data(..)
node.insert_flow(Flow::coded_video(key, source_key, device_key, VideoFormat { .. }))?;
node.insert_flow(Flow::audio(key, source_key, device_key, media_type, sample_rate, bit_depth))?;
node.insert_flow(Flow::data(key, source_key, device_key, media_type, grain_rate))?;
node.insert_sender(Sender::mxl(key, flow_key, device_key))?;       // .mxl_flow_id(id).mxl_domain_id(id)
node.insert_receiver(Receiver::mxl(key, device_key, Format::Audio, media_types))?; // .caps(constraint_sets, version)
node.erase(ResourceType::Sender, key)?;                            // dependants are erased too
node.set_staged(Role::Receiver, key, Connection { .. })?;          // app-side state, no callback
node.set_active(Role::Receiver, key, Connection { .. })?;
```

- Every resource has public `label`, `description` and `tags` (with a
  `group_hint(group, role)` helper, as `nmos::make_group_hint`).
- `insert_*` with an existing key replaces the resource and bumps its `version`.
  References (device, source, flow) must exist, so the registry never sees an orphan.
- `mxl_flow_id` / `mxl_domain_id` on a sender are what `auto` resolves to; the MXL flow
  id may differ from the IS-04 Flow id (BCP-007-03).
- Changes are pushed to the registry by the registration task.

### 5.4 Identity

- `node_id = UUIDv5(NAMESPACE, seed)`, with Strom's namespace constant
  `8c76aff5-a8f9-53ce-97d0-766f1074b699` (= `UUIDv5(NAMESPACE_OID, "leeo86.strom.nmos.v1")`).
  Keeping it keeps every Strom id; the mixer uses the same derivation with its own seed.
- `device = UUIDv5(node_id, "device:{key}")`, `source|flow|sender|receiver =
  UUIDv5(node_id, "{role}:{key}")`, exactly Strom's formulas.
- `NAMESPACE` and `derive_id(seed, key)` are public, so an application can derive other
  stable ids (MXL flow ids, its output domain id) the same way.

### 5.5 Activation callback

```rust
pub type ActivationHandler =
    Arc<dyn Fn(Activation) -> BoxFuture<'static, Result<(), Rejected>> + Send + Sync>;

pub struct Activation {
    pub key: String,            // the app's resource key
    pub role: Role,             // Sender | Receiver
    pub nmos_id: Uuid,
    pub master_enable: bool,
    pub mxl_domain_id: Option<Uuid>,
    pub domain_path: Option<PathBuf>,  // from the application's domain list
    pub mxl_flow_id: Option<Uuid>,
    pub peer_id: Option<Uuid>,  // sender_id / receiver_id from the PATCH
    pub cause: Cause,           // Immediate | Scheduled | Bulk | Restore
}
pub struct Rejected(pub String);
```

- Called once per resource, for single, bulk and scheduled activations and for restore.
- `Ok` = the application took the parameters (not "the flow is open"). The crate then
  updates `active`, the IS-04 `subscription` and `version`. Never blocks on media; the
  receiver's `waiting` state is the app's business.
- `Err(Rejected)` or the timeout -> 500 with the reason, `active` unchanged. On timeout
  the future is dropped.
- Scheduled: the PATCH returns 202 at once; a TAI timer task calls the callback at the
  activation time; `staged` is locked (423) until then except for a PATCH that cancels
  it with `activation.mode: null`.
- `auto` default resolution: a sender's `mxl_flow_id` / `mxl_domain_id` (a sender
  without a configured flow id gets a random one once, as Strom does); otherwise the
  only visible domain; receivers reject `auto` for `mxl_flow_id`. `on_resolve_auto`
  replaces this (Strom: preferred domain path, then the domain holding the flow).

### 5.6 Serving, registration, shutdown, persistence

```rust
node.router() -> axum::Router            // routes under /x-nmos, CORS included; merge into your server
node.serve(addr).await -> Result<()>     // own listener on NMOS_PORT; bind error -> Err (app exits 75)
node.start_registration()                // spawns the task; no-op when registration is off
node.is_registered() -> bool             // for /readyz only; nothing in the crate touches liveness
node.shutdown(timeout).await             // DELETE all resources in reverse order, Node last

node.connections() -> Vec<SavedConnection>          // serde; active state of every sender/receiver
node.restore(Vec<SavedConnection>).await            // calls the callback for each (Cause::Restore)
```

`on_connections_changed` is called after every change of an `active` state, so the
application can save it under `/config` (G9). Strom does not use it: its active
connection is the saved flow, as today (`IMPLEMENTATION_PLAN.md` G9).

## 6. Git history

`git filter-repo --path backend/src/nmos/ --path-rename backend/src/nmos/:src/` on a
clone of Strom keeps the 5 commits that touched these files (authors and dates kept).
That history is merged into `mxl-nmos` with `--allow-unrelated-histories`, and the next
commits split and rename the files. Two points (Q7):

- Strom's commit messages say `(#11)`, `(#13)`, `(#19)`; in `mxl-nmos` GitHub would link
  them to the wrong issues. Proposal: rewrite them to `LeeO86/strom#11` etc. with
  `--message-callback` (only that change).
- The history survives only if that PR is merged with a **merge commit**, not squashed.

## 7. Tests and CI in `mxl-nmos`

- Unit tests: every item in the spec's test list, with an injected TAI clock
  (`trait Clock`) for scheduled activations and versions, a mock registry (axum) for
  order, 404 re-registration, backoff and deregistration, and env parsing.
- Schema tests: every served resource and IS-05 document validated against the
  official schemas, vendored from `AMWA-TV/is-04` `v1.3.3`, `AMWA-TV/is-05` `v1.2.0`,
  `AMWA-TV/bcp-007-03` `v1.0.0` and `AMWA-TV/bcp-004-01` `v1.0.0` (all Apache-2.0)
  under `tests/schemas/` with their licence.
- Integration: the example node against the nmos-cpp registry container — register,
  heartbeat, re-register after a registry restart, deregister on shutdown, readiness via
  its Query API.
- AMWA nmos-testing, installed from source at a pinned commit (the sibling's
  `9001851…`, so the results are comparable with Strom's PR #11 run) — no Docker Hub:
  - job 1 (DNS-SD off, the default): IS-04-01, IS-05-01, IS-05-02, BCP-007-03-01; the
    node registers with the tool's mock registry by static address;
  - an allow-list file (`ci/amwa-expected.json`) names each test that may end
    "Could Not Test" / "Not Applicable" / "Manual" and why; any other non-pass result
    fails the job, and so does an allow-listed test that changes state;
  - job 2 (feature `dns-sd`, `NMOS_DNS_SD=true`): the same suites, so the mDNS tests run.
- Registry image: `mirror.gcr.io/rhastie/nmos-cpp` pinned by digest (Google's Docker
  Hub mirror, no login; checked reachable). Q8.
- Also: `cargo fmt --check`, `clippy -D warnings`, `cargo doc` with `missing_docs`
  denied, `cargo deny`, builds on Linux, macOS and Windows (Strom ships all three).

## 8. Switching Strom

- PR in `LeeO86/strom`: `mxl-nmos = { git = "https://github.com/LeeO86/mxl-nmos", tag = "v0.1.0", features = ["dns-sd"] }`.
- `backend/src/nmos/` shrinks to the glue (§3). Strom's existing NMOS tests stay and
  run against the glue (the HTTP ones unchanged; the internals ones point at the crate).
- Strom's NMOS settings, readiness, shutdown and export/import keep their behaviour,
  except the rows of §4 you approve.
- BCP-007-03-01 against Strom with the same nmos-testing commit, as a new job in
  Strom's CI (Q9).
- `docs/CHANGELOG.md` entry; Strom's `IMPLEMENTATION_PLAN.md` notes that the
  per-receiver callback makes "no flow restart on routing change" possible (follow-up).

## 9. Order of work

1. PR 2: history import + licence files (no behaviour).
2. PR 3: crate API and model, unit and schema tests, CI.
3. PR 4: example node, integration and AMWA CI, docs.
4. Tag `v0.1.0` with release notes — only with your OK.
5. Strom PR.

## 10. Decisions (maintainer, 2026-10-11)

| # | Question | Answer |
| --- | --- | --- |
| 1 | Behaviour changes for Strom (§4) | Yes, all of them. |
| 2 | Activation timeout for Strom | 30 s; Strom's handler keeps restarting the flow before it answers. |
| 3 | Strom readiness via the Query API | Yes. |
| 4 | Node `hostname` | Keep it (unsure). The nmos-cpp siblings announce the system hostname in that field too, so the crate keeps it; every address stays an IP literal. |
| 5 | Sender as one bundled call | No: one resource per call, as in the siblings' nmos-cpp nodes (§5.3). |
| 6 | Domain helpers in the crate | No: they stay in each application (§3). |
| 7 | History import with `(#N)` rewritten | Yes. |
| 8 | nmos-testing from source, registry from `mirror.gcr.io` | Yes. |
| 9 | AMWA BCP-007-03-01 job in Strom's CI | Yes, in the switch PR. |
| 10 | Crate metadata | Not on crates.io; `authors = ["Adrian Hilber <LeeO86@users.noreply.github.com>"]`. |
