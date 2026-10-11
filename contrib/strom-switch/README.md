# Switching Strom to mxl-nmos v0.1.0

Four patches against `LeeO86/strom` `main` at `c2e27a6` (release 1.0.1). They are the
"Switching Strom to the crate" part of `SPECIFICATION.md` and
`docs/extraction-plan.md` §8. They were prepared here because the environment that
built them could not push to the Strom repository.

| Patch | Change |
| --- | --- |
| 0001 | `backend/Cargo.toml`: `mxl-nmos = { git = …, tag = "v0.1.0", features = ["dns-sd"] }` |
| 0002 | `backend/src/nmos/`: `node.rs`, `http.rs`, `register.rs` are replaced by the crate; `glue.rs` maps flows to its resources with Strom's keys (ids unchanged) and keeps the activation handler that restarts the flow (30 s timeout). `domain.rs` and the settings stay. |
| 0003 | CI job `nmos-amwa`: AMWA BCP-007-03-01 against Strom with an nmos-cpp registry (`scripts/nmos-amwa/`). |
| 0004 | `docs/CHANGELOG.md` (Unreleased), `docs/NMOS_MXL.md`, `IMPLEMENTATION_PLAN.md` (incl. the follow-up: per-receiver activation without a flow restart). |

## Apply

```sh
git clone https://github.com/LeeO86/strom && cd strom
git checkout -b nmos-mxl-nmos-crate c2e27a6     # or current main, if it has not moved
git am /path/to/mxl-nmos/contrib/strom-switch/*.patch
git push -u origin nmos-mxl-nmos-crate          # then open the PR into main
```

The patches keep the author and commit messages.

## Tested (Ubuntu 24.04, Rust 1.97.1)

- `cargo test --package strom --features mxl --lib nmos::`: 12 passed. The two tests
  of moved code (MAC format, registry URL) are in the crate now.
- `cargo test --package strom --package gst-plugin-v210gl --features efp,mxl,nvidia`:
  885 passed, 0 failed, 4 ignored (as on `c2e27a6`).
- `cargo clippy --package strom --package gst-plugin-v210gl --all-targets --features efp,mxl,nvidia -- -D warnings`: clean.
- `scripts/nmos-amwa/run.sh` (nmos-testing `9001851`): BCP-007-03-01 Pass 55,
  Manual 1 (test_15), 14 IS-10 checks disabled, 3 not applicable, 0 Fail. Same as the
  run recorded in Strom PR #11.
- Every IS-04 resource and IS-05 `staged`/`active`/`constraints` document of Strom
  1.0.1 and of the patched Strom, same seed, labels, tags and flows, compared without
  `version`: identical except the Device `controls` (now lists the Connection API) and
  the Node `href` (now the API base URL).

The new CI job has not run on GitHub yet; it needs the runner to set the kernel TAI
offset, which the job does with `adjtimex` (`sudo`).
