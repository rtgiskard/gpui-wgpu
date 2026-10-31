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

## Upstream tracking

Support and platform crates remain pinned to the baseline above. Backport selected
fixes rather than replacing these two crates with upstream HEAD: renderer resource
retirement, material uploads and transformed sprites have local contracts that must
survive each update. Sprite record layouts must also remain compatible with the
pinned platform renderers; texture batching can be updated without changing ABI.

Consumers should pin both crates to the same full commit during development.
Immutable release tags mark integration-tested milestones of this patch set,
independently of Rivu's version; the `upstream/*` tags identify source snapshots,
not supported releases. A consumer's lockfile records the exact adopted revision.

For each upstream review, record the scanned Zed commit and the disposition of
candidate patches. Advancing that scan point does not advance the dependency
baseline: platform and support API migrations require a separate coordinated update.

### Reviewed upstream changes

Last scanned Zed revision: `ba63a022c9a22ca66d68ef2a8f0bb9b5b0a9984d`.
The support-crate baseline remains `df48a139ce9c292738b523252c12f044697e673f`.

| Upstream commits | Disposition |
| --- | --- |
| `2529740084` | Arena ownership isolation, retaining local nested-draw reclamation |
| `e4a3732b94` | Cache-only line-wrapper pooling, retaining checkout-safe reclamation |
| `8fa085e397` | Inherited-opacity cache invalidation and paint-time correction |
| `e18de65b75` | WGPU path source-over alpha; other platform renderers unchanged |
| `402675e805`, `24a2823456` | WGPU and Naga migration to `cddb2f5d80dc0009b2ef6767635140cacf90a007` |
| `e20d19d32f` | Single surface configuration, retaining local backend selection |
| `b4ecc45422` | Texture-index sprite sorting only; record layout changes deferred |
| `3389d87577`, `b7d50b95e8` | Resolved-font caching and explicit-request warnings |
| `370591f3d1` | Deferred native-close cleanup |
| `99e70ee4a0` | Executor-clock animation timing |
| `2a74c8dff6` | Removed unused hash-layout APIs and cache state; ordinary and wrapped replay retained |
| `772a8a9100` | Window-local Rc/RefCell text caches and font-run scratch; shared font state remains synchronized |
| `b488c4e814` | Taffy 0.15 measurement/alignment API migration, retaining device-pixel snapping and relative positioning |

The WGPU migration changes presentation and mapped-buffer APIs, including local
material uploads and readback. Consumers must update their lockfile when adopting
this patch set. `Window::text_system()` now returns `&Rc<WindowTextSystem>`;
consumers explicitly storing that handle must use Rc and keep it on the UI thread.
The unused hash-layout APIs have been removed. Font-generation invalidation and
ordinary/wrapped double-frame cache reclamation remain in place. Platform/input/
scheduler migrations and hot reload are deferred. Taffy is pinned to 0.15.0;
layout consumers should verify grid/flex sizing and measured content when updating.

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
