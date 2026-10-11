# Development environment

Everything here runs without special hardware; CI runs the same steps
(`.github/workflows/ci.yml`).

## Requirements

- Linux (Ubuntu 24.04 tested; WSL2 or a dev container work the same), x86_64.
- Rust 1.97.1 (`rust-toolchain.toml`; rustup installs it).
- Docker, for the nmos-cpp registry. Without systemd (some containers and VMs), start
  the daemon by hand: `sudo dockerd > /tmp/dockerd.log 2>&1 &`.
- Python 3.12 with `venv` and the headers (`python3-venv python3-dev`), for AMWA
  nmos-testing.
- The kernel TAI offset set to 37 s. Check with
  `python3 -c "import time; print(time.clock_gettime(time.CLOCK_TAI) - time.clock_gettime(time.CLOCK_REALTIME))"`;
  if it prints `0.0`, run `sudo ci/set-tai-offset.py` (lost on reboot). NMOS
  controllers and nmos-testing compute TAI as UTC + 37 s; without the offset every
  scheduled activation lands 37 s late.

## Build and unit tests

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                    # and: cargo test --all-features
```

## nmos-cpp registry

`ci/registry.json` configures the registry like the platform: Registration API on
8010, Query API on 8011, Query WebSocket on 8012 (its own Node API on 8013). The image
comes from `mirror.gcr.io` (Docker Hub's rate limits do not apply), pinned by digest in
`ci/amwa.sh`, `ci/registry-it.sh` and the workflow.

```sh
ci/registry-it.sh             # the integration test, starts and removes the container
```

## AMWA nmos-testing

Installed from source at the commit pinned in the workflow (`NMOS_TESTING_REF`):

```sh
REF=90018513758e6fbc096c26d7ef001f700b28b510
mkdir -p /tmp/nmos-testing
curl -fsSL https://codeload.github.com/AMWA-TV/nmos-testing/tar.gz/$REF | tar xz -C /tmp/nmos-testing --strip-components=1
python3 -m venv /tmp/nmos-venv
/tmp/nmos-venv/bin/pip install -r /tmp/nmos-testing/requirements.txt

cargo build --example mxl_node
ci/amwa.sh off target/debug/examples/mxl_node     # DNS-SD off, nmos-cpp registry

cargo build --example mxl_node --features dns-sd --target-dir target/dns-sd
ci/amwa.sh on target/dns-sd/debug/examples/mxl_node   # DNS-SD on, mock registries over mDNS
```

Results (JSON, logs, `summary.md`) go to `ci-artifacts/amwa-<mode>/`. `ci/check_amwa.py`
fails on any result that is neither a Pass nor listed with its reason in
`ci/amwa-expected-<mode>.json`. When a change makes a listed test pass, remove it from
the list. The DNS-SD run needs multicast on the host interface (`HOST_IP`, default: the
source address of the default route).
