# mxl-nmos

An AMWA NMOS Node for MXL media functions, in Rust: the IS-04 v1.3 Node API, the
IS-05 v1.2 Connection API and the BCP-007-03 MXL transport
(`urn:x-nmos:transport:mxl`), with registration to a static IS-04 registry.

It was extracted from the NMOS node of [`LeeO86/strom`](https://github.com/LeeO86/strom)
(see [`docs/extraction-plan.md`](docs/extraction-plan.md)) and is used by Strom and by
[`LeeO86/mxl-audio-mixer`](https://github.com/LeeO86/mxl-audio-mixer).

The crate knows nothing about media. MXL domain and flow ids are UUIDs here; the
application reads and writes MXL itself and keeps its own MXL domain files. There is no
GStreamer, MXL SDK or application model in the crate.

## Use

Depend on a release tag, never on a branch:

```toml
[dependencies]
mxl-nmos = { git = "https://github.com/LeeO86/mxl-nmos", tag = "v0.1.0" }
# DNS-SD (mDNS) registry browsing and node advertisement, off by default:
# mxl-nmos = { git = "…", tag = "v0.1.0", features = ["dns-sd"] }
```

The API follows the nmos-cpp nodes of the sibling media functions: one call per
resource, callbacks on a builder.

```rust,ignore
use mxl_nmos::*;

let mut settings = Settings::new("Audio Mixer", 3312);
settings.apply_env()?;                         // the platform names below; Err → exit 78
let node = Node::new(settings, NodeImplementation::new(|activation| Box::pin(async move {
        // Apply activation.mxl_domain_id / mxl_flow_id / master_enable to ONE
        // receiver or sender. Accept even if the flow does not exist yet.
        Ok(())
    }))
    .domains(|| my_visible_mxl_domains())      // Vec<MxlDomain { id, label, path }>
    .on_connections_changed(|all| save_under_config(all)))?;

node.insert_device(Device::new("mixer", "Mixer"))?;
node.insert_receiver(Receiver::mxl("in-1", "mixer", Format::Audio,
                                   vec![media_types::AUDIO_FLOAT32.into()]))?;
node.insert_source(Source::audio("pgm", "mixer", vec![Channel::new("L"), Channel::new("R")]))?;
node.insert_flow(Flow::audio("pgm", "pgm", "mixer", media_types::AUDIO_FLOAT32,
                             Rational::new(48_000, 1), Some(32)))?;
node.insert_sender(Sender::mxl("pgm", "pgm", "mixer"))?;
node.restore(load_from_config()).await;        // G9: re-applies connections via the callback

let server = node.bind(([0, 0, 0, 0], node.settings().port).into()).await?; // Err → exit 75
node.start_registration();
tokio::spawn(server.run());                    // or merge node.router() into your own axum app
// /readyz: node.is_registered(). /livez must not depend on it.
// SIGTERM: node.shutdown(timeout).await deletes every resource, the Node last.
```

A complete node is in [`examples/mxl_node.rs`](examples/mxl_node.rs): one MXL video
sender, one audio receiver, a fake activation handler, registration.

```sh
NMOS_HOST_ADDRESS=192.0.2.10 NMOS_REGISTRY_ADDRESS=192.0.2.20 NMOS_REGISTRY_PORT=8010 \
    cargo run --example mxl_node
```

## Settings

`Settings::apply_env` reads these names (G1, G3–G6). Blank values count as unset; an
invalid value is an error, and the application exits with code 78. An application may
also fill `Settings` itself (Strom does, to keep its own aliases and config file).

| Variable | Default | Meaning |
| --- | --- | --- |
| `NMOS_SEED` | unset | Every id is UUIDv5-derived from it: the node from the seed, the other resources from the node id and the application's resource key. Same seed, same ids after a restart. |
| `NMOS_LABEL` | the application's | Node label, and the prefix of every Device label. |
| `NMOS_TAGS` | `{}` | JSON object of tag name → array of strings, added to the Node and every Device. |
| `NMOS_HOST_ADDRESS` | first non-loopback IPv4 | IPv4 literal announced in `href`, `api.endpoints` and Device `controls`. Never a name, `0.0.0.0` or loopback. |
| `NMOS_PORT` | the application's | Port of the Node and Connection APIs. |
| `NMOS_REGISTRY_ADDRESS`, `NMOS_REGISTRY_PORT` | unset | Static registry (both or neither). Unset: no registration. |
| `NMOS_QUERY_ADDRESS`, `NMOS_QUERY_PORT` | registry address, registration port + 1 | Query API used for readiness. |
| `NMOS_DNS_SD` | `false` | mDNS registry browsing and node advertisement. Needs the `dns-sd` feature; off means neither, and nothing needs Avahi or D-Bus. |

Other `Settings` fields with their defaults: `heartbeat` 5 s, `activation_timeout`
2 s, `registry_timeout` 2 s, `registration` true, `https` false, `id_path` (file for a
random node id when there is no seed), `service_name_prefix` (mDNS instance name).

## Behaviour

- **Node API v1.3 and Connection API v1.2** under `/x-nmos`: `staged`, `active`,
  `constraints` (the visible domain ids; never `auto`), `transporttype`, `transportfile`
  404 for MXL senders, empty `interface_bindings`, `/bulk/senders` and
  `/bulk/receivers`, immediate and scheduled (absolute and relative) activations, the
  NMOS error format, CORS headers on every response including errors and `OPTIONS`.
- **Activation callback, per resource.** Called once for each Sender or Receiver
  activated (single, bulk, scheduled, restore) with the resolved `mxl_domain_id`, its
  directory, `mxl_flow_id`, `master_enable` and the peer id. `Ok` means the application
  took the parameters, not that media flows: a Receiver whose flow does not exist yet is
  accepted and waits for it. Only on `Ok` does the crate update `active`, the IS-04
  `subscription` and `version`. `Err(Rejected)` or the timeout answers 500 with the
  reason and leaves `active` unchanged. Nothing is restarted by the crate.
- **`master_enable: false`** is a normal activation, not a reject.
- **`auto`.** A Sender's `auto` resolves to its configured `mxl_domain_id` /
  `mxl_flow_id` (a Sender without a configured flow id gets a random one, once), else
  the only visible domain. Receivers accept `auto` only for `mxl_domain_id`.
  `NodeImplementation::on_resolve_auto` replaces these rules. The MXL flow id may differ
  from the IS-04 Flow id (BCP-007-03).
- **Scheduled activations use TAI.** Absolute times are compared with `CLOCK_TAI`;
  relative ones count from the TAI now. The PATCH answers 202 and `staged` is locked
  (423) until the activation ran or a PATCH with `activation.mode: null` cancelled it.
  The host's kernel TAI offset must be set (37 s), as on the platform nodes; otherwise
  `CLOCK_TAI` equals UTC and every TAI time is 37 s off.
  `clock::tai_utc_offset_secs()` tells the application.
- **Bulk.** Every entry is staged and activated on its own; one reject or error fails
  only that entry (per-entry `code`, `error`, `debug`); the response is 200.
- **Versions** are TAI and strictly increasing per resource, even for two changes in the
  same nanosecond, and change on every activation.
- **Registration**: Node → Device → Source → Flow → Sender/Receiver; changes are pushed as
  they happen; deletion in reverse order with the Node last. A heartbeat 404 registers
  everything again in order. A 200 on the first Node POST (a stale registration) deletes
  the old Node and registers afresh. Failures back off from 1 s, doubling to 10 s, or move
  to the next registry found by DNS-SD.
- **Readiness vs liveness.** `is_registered()` is true when the last registration and
  heartbeat succeeded and, with a Query API configured, the Query API lists the node. It is
  for `/readyz` only; nothing in the crate makes liveness depend on the registry.
- **Shutdown.** `shutdown(timeout)` stops registering and DELETEs every resource, the Node
  last, within the timeout (G8).
- **Persistence (G9).** `connections()` returns the active state of every Sender and
  Receiver (serde); `on_connections_changed` is called after each activation;
  `restore()` re-applies saved connections through the callback at start.

## Tests

`cargo test` runs the unit tests, the IS-05 state machine on an injected TAI clock, the
IS-04 resource and version rules, registration against a mock registry, and validates
every served document against the official AMWA schemas (`tests/schemas`). CI also runs:

- `ci/registry-it.sh`: the crate against an nmos-cpp registry container (register,
  heartbeats, re-registration after a registry restart, Query API readiness,
  deregistration);
- `ci/amwa.sh off|on`: AMWA nmos-testing IS-04-01, IS-05-01, IS-05-02 and BCP-007-03-01
  against the example node, with DNS-SD off (nmos-cpp registry) and on (the tool's mock
  registries). Every result that is not a Pass is listed with its reason in
  `ci/amwa-expected-off.json` / `ci/amwa-expected-on.json`; any other result fails.

See [`docs/development.md`](docs/development.md) for the local set-up.

## Licence

`MIT OR Apache-2.0`, at your option: [`LICENSE-MIT`](LICENSE-MIT),
[`LICENSE-APACHE`](LICENSE-APACHE).

The code was extracted from the NMOS node of `LeeO86/strom` (at `c2e27a6`), a fork of
Eyevinn's Strom. Strom is dual-licensed `MIT OR Apache-2.0`, and the NMOS module was
contributed to the fork under that licence, so the crate keeps the same dual licence.
Both licence files are copied from Strom unchanged, with their copyright notices. The
history of the extracted files is kept (imported with `git filter-repo`).

The AMWA schemas in `tests/schemas` are Apache-2.0 and keep the specifications'
`LICENSE` and `NOTICE` files.
