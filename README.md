# gpui-wgpu

Pinned GPUI and WGPU renderer patches, extracted from Rivu. This repository
maintains rendering and framework changes; it is not a replacement for the
complete Zed workspace. GPU material APIs remain experimental. The maintained
application integration is Linux/WGPU; other platforms are not certified here.

## Source and history

- Upstream: https://github.com/zed-industries/zed
- Baseline: `df48a139ce9c292738b523252c12f044697e673f`
- Baseline snapshot tag: `upstream/df48a13`
- Rivu extraction snapshot: `d1ea1cb80d035ddf38eef46fc9f2e1ee99d59651`

The root commit imports the two unmodified upstream crate directories and the
Apache-2.0 license, not the full Zed history. It depends on Zed's workspace and is
an archival baseline, not a standalone build. The next commit isolates package
manifests and prunes unneeded examples. Subsequent commits project all 47 relevant
Rivu commits onto `crates/gpui` and `crates/gpui_wgpu`, preserving original authors,
author dates and the material branch merge. Each patch commit names its complete
Rivu source hash. Subjects describing application features are narrowed to the
framework portion actually imported.

The imported crate contents are unchanged from Rivu's `vendor/gpui` and
`vendor/gpui_wgpu`. Final integration commits supply licensed font fixtures,
standalone lockfiles and development commands. Historical dependency stages are
validated together at the final tree; individual intermediate commits are not
claimed to build independently. Rivu's application changes, integration tests and
benchmark claims are not imported. No application dependency cutover is included.

## Layout and responsibilities

- `crates/gpui`: drawing APIs, scene recording/replay, UI and text state
- `crates/gpui_wgpu`: GPU execution, shaders, uploads and resource retirement
- `tests/fixtures/fonts`: deterministic upstream font fixtures with their licenses

Unmodified platform and support crates still come from the pinned Zed revision.
Keep both patched crates on the same commit and review Rust/shader layouts,
composition order and resource lifetimes together when updating upstream.

## Development

Use the pinned Rust toolchain. Linux builds need a C/C++ toolchain, pkg-config and
the native development dependencies for GPUI's enabled Wayland/X11, font and
graphics features. WGPU behavior checks require an available adapter. Headless
checks do not establish native-window, input or presentation correctness.

From the repository root:

```sh
make lint
make test
```

Build outputs default to `.cache/target`. Both crates intentionally retain
separate regression workspaces; there is no root Cargo workspace. Their lockfiles
are committed.

The imported sources have existing rustfmt differences. `make lint` reports
those differences before reaching Clippy; this extraction intentionally does not
mix formatting cleanup into the provenance-preserving patch history.

For focused checks:

```sh
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui/Cargo.toml --lib gpu::tests --no-default-features
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui/Cargo.toml --lib scene::reclamation_tests --no-default-features
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui_wgpu/Cargo.toml --lib
```

Consumers must patch both `gpui` and `gpui_wgpu` at the application workspace root
and keep other Zed crates on the baseline revision. This repository's nested
manifest patches apply only to its standalone regression workspaces; they do not
propagate into consumers. Keep any required third-party patches, including Zed's
nonblocking `calloop` revision, in the consuming application's root manifest.

Apache-2.0 applies to the imported crates. Font fixtures retain the licenses in
their respective directories.
