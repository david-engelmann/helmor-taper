# Cross-compile builder for `helmor-server`

This directory hosts a thin Docker recipe for cross-compiling
`helmor-server` to `aarch64-unknown-linux-gnu` against the system
libraries the helmor crate transitively links against (webkit, gtk,
soup, libclang, …). The artifact lands at `.builder/helmor-server`
and gets shipped to the e2e Docker container as
`/home/e2e/.helmor/server/helmor-server.real`.

## When to use

When you've made a Rust change to `src-tauri/` and need to verify
it on the linux-arm64 e2e container (`helmor-test-linux-arm64`) —
the upstream CI normally cross-compiles `helmor-server` for that
target, but for fast local iteration this recipe lets you skip the
GitHub-side roundtrip.

## How

```bash
# 1) Build the toolchain image once (cached). ~30s after first pull.
docker build --platform linux/arm64 -t helmor-builder:arm64 .builder

# 2) Apply the rlib-only crate-type pin the CI workflow uses
#    (see .github/workflows/publish-helmor-server.yml for the
#    rationale — OOMs on the full lib build without it).
cp ../helmor/src-tauri/Cargo.toml ../helmor/src-tauri/Cargo.toml.bak
sed -i.tmp 's/^crate-type = \["staticlib", "cdylib", "rlib"\]/crate-type = ["rlib"]/' \
  ../helmor/src-tauri/Cargo.toml
rm ../helmor/src-tauri/Cargo.toml.tmp

# 3) Build (5–8 min cold, ~1 min incremental via the helmor-linux-target volume).
docker volume create helmor-linux-target
docker run --rm --platform linux/arm64 \
  -v $PWD/../helmor:/work:rw \
  -v helmor-linux-target:/work/src-tauri/target:rw \
  -v helmor-cargo-cache:/usr/local/cargo/registry:rw \
  -e CARGO_TERM_COLOR=never \
  -e CARGO_BUILD_RUSTC_WRAPPER="" \
  -w /work/src-tauri \
  helmor-builder:arm64 \
  sh -c 'cargo --config build.rustc-wrapper=\"\" build --release --bin helmor-server'

# 4) Restore the Cargo.toml.
mv ../helmor/src-tauri/Cargo.toml.bak ../helmor/src-tauri/Cargo.toml

# 5) Extract the binary.
docker run --rm --platform linux/arm64 \
  -v helmor-linux-target:/target:ro \
  -v $PWD/.builder:/out:rw \
  helmor-builder:arm64 \
  sh -c 'cp /target/release/helmor-server /out/helmor-server'

# 6) Ship to the container.
docker cp .builder/helmor-server helmor-test-linux-arm64:/home/e2e/.helmor/server/helmor-server.real
docker exec helmor-test-linux-arm64 sh -c \
  'chown e2e:e2e /home/e2e/.helmor/server/helmor-server.real && \
   chmod 755 /home/e2e/.helmor/server/helmor-server.real'

# 7) Bounce the daemon.
docker exec helmor-test-linux-arm64 sh -c 'pkill -TERM -f helmor-server.real' || true
docker exec -u e2e -w /home/e2e helmor-test-linux-arm64 \
  ./.helmor/server/helmor-server --ensure-daemon
```

## Pinning notes

- The image targets Debian **bookworm** because the test container
  is bookworm (glibc 2.36). Building on trixie produces a binary
  that won't load on the container (GLIBC_2.39 not found).
- `cmake`, `clang`, `libclang-dev` are needed by `boring-sys2` and
  `bindgen`; the apt list in the Dockerfile is the minimal set the
  CI workflow's "Install Linux deps" step uses, plus those two.
- The `rust-toolchain.toml` in the helmor repo pins the channel
  (1.94.1) — the bookworm image's 1.95.0 is downloaded down to
  1.94.1 on first invocation via rustup. ~1 minute extra the first
  time; cached after that.

## What's gitignored

`.builder/helmor-server` (the produced 8 MB binary) — regenerated
on every build, not worth tracking. The Dockerfile + this README
stay checked in so the recipe is reproducible.
