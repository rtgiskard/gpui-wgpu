# gpui-wgpu

Pinned GPUI framework, WGPU renderer and Linux platform patches, extracted from
Rivu and Zed. This repository is not a replacement for the complete Zed workspace.
GPU material APIs remain experimental. The maintained
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

The glass-materials branch adds composable source-image effects, bounded ordered
scene filters and procedural water, with image/cache and resource-lifetime fixes.
Their migration commits name the original Rivu source; application material state,
artwork demand, controls and configuration remain in Rivu. These extensions build
on the maintained text, renderer and Taffy updates rather than replacing them.

Still Water is a calm field of localized raindrop impacts. Sixteen bounded event
slots produce 32 unevenly timed arrivals per wrapping phase cycle; each epoch
changes an impact's position, strength and scale. Complete circular wavefronts
are mildly foreshortened into perspective ellipses. A compact expanding packet
contains up to four oscillations, with cylindrical spreading and lifetime damping
leaving quiet gaps rather than broad swell blobs. Analytic heights and gradients
overlap before a single normal drives shallow-bed refraction and broad sky
reflection: rings are surface motion, not luminous overlays or painted streaks.
Smooth birth/death envelopes and silent epoch margins keep the phase C1 across
every replacement and wrap; zero amplitude is static. Highlight controls reflected
light and subtle procedural caustics. Pixel-footprint attenuation suppresses
unresolved carriers before they reach the normal.

The field has a fixed 16-slot loop with no nested sampling. Its per-fragment upper
bound is 16 arithmetic hashes, 16 square roots, 16 inverse square roots and 16
sine/cosine pairs; inactive, unresolved and spatially excluded packets return
early. There are no finite-difference normal samples, textures, additional
buffers, render passes or CPU event storage. The other six water styles retain
their independent shading.

## Layout and responsibilities

- `crates/gpui`: drawing APIs, scene recording/replay, UI and text state
- `crates/gpui_wgpu`: GPU execution, shaders, uploads and resource retirement
- `crates/gpui_linux`: Linux/FreeBSD windowing, input, accessibility and desktop integration
- `crates/gpui_platform`: cross-platform application/platform constructors
- `tests/fixtures/fonts`: deterministic upstream font fixtures with their licenses

The Linux and platform-constructor sources were initially imported unchanged from
Zed's baseline; adopted fixes are recorded below. Other platform/support crates use that pinned
revision. Keep all four maintained crates on the same commit and review framework
types, platform adapters, Rust/shader layouts and resource lifetimes together.

Window-local atlas removal invalidates lookup immediately; the frame-discard
boundary releases storage only after the sole replayable scene no longer uses it.
Sprite ABI stays unchanged. A hidden/no-draw window may retain removed tiles until
its scene is discarded, the atlas is reset, or the window is destroyed.

Atlas pixels go directly to WGPU staging without changing CPU image format or
alpha semantics. Deleting an image cannot cancel that staged upload, so a valid
device must advance pending uploads even when presentation is skipped or the
surface is detached. Repeated skips without new uploads do not keep submitting.
Device loss hands the old queue to recovery; submitted commands retain their resources.

WebGL bindings belong to each texture allocation; rollover must preserve earlier
draws rather than overwrite or release their storage.

## Upstream tracking

External support and platform crates remain pinned to the baseline above. Backport
selected fixes rather than replacing the maintained crates with upstream HEAD: renderer resource
retirement, material uploads and transformed sprites have local contracts that must
survive each update. Sprite record layouts must also remain compatible with the
pinned platform renderers; texture batching can be updated without changing ABI.

Consumers should pin all four maintained crates to the same full commit.
Immutable release tags mark integration-tested milestones of this patch set,
independently of Rivu's version; the `upstream/*` tags identify source snapshots,
not supported releases. A consumer's lockfile records the exact adopted revision.

For each upstream review, record the scanned Zed commit and the disposition of
candidate patches. Advancing that scan point does not advance the dependency
baseline: platform and support API migrations require a separate coordinated update.

### Reviewed upstream changes

Last scanned Zed revision: `f0fa51bb510fc91f6da7ec55f3e0f317f1f975ec`
(2026-11-14). The support-crate baseline remains
`df48a139ce9c292738b523252c12f044697e673f`.

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

The 2026-11-14 review triaged the 165 Zed commits since the previous scan
`ba63a022c9a22ca66d68ef2a8f0bb9b5b0a9984d`, inspecting selected framework,
Linux and renderer patches. Adoption and remaining candidates are recorded below:

| Upstream commits | Disposition |
| --- | --- |
| `985bbf42e684` | Apply cosmic-text glyph x/y offsets when building ShapedGlyph positions, preserving mark placement |
| `decd2399b180` | Capture cached/deferred paint reads and restore live subscriptions; isolate nested window construction/draw contexts while retaining local replay ownership |
| `bfcd5949ff82` | Measure Wayland frame signals from callback receipt; remove protocol-clock inference and its obsolete tests |
| `c7bea343838f`, `78fde14d124d` | Review drag cursor precedence and list horizontal padding with application layout/input acceptance |
| `4023e3c88e0a`, `22580c6e6970`, `ddb90e5e89d3`, `62682ebde9e7` | Acquire wrappers only for truncation, skip absent backgrounds, share empty features, avoid copied cache-hit text and sort tab stops lazily; preserve local window-thread caches, replay and font-run scratch |
| `415d310e5260` | Requires coordinating the external gpui_shared_string crate, not just the four maintained crates |
| `ba490197cfb8`, `caead827d77c`, `ffe0fa29e41e`, `1d8eda94bd4d`, `be07e43ea0ad`, `d3fc053d5b62` | Separate rendering migration: clipping, paths, batching, depth and sorting interact with local materials, scene filters and replay |
| `aff99ecc3249` | Separate retained SVG-animation migration; preserve local transformed-sprite/replay contracts |
| `64ef8026c7bc` | Coordinated framework/Linux window-placement API migration |
| `8e474e159887` | No change needed: local renderer already passes premultiplied-alpha state through uniforms |

Cached/deferred reads are recorded per window across prepaint and paint. Opening
a window synchronously from those callbacks suspends the enclosing recording
context; inner construction/draw reads do not subscribe the outer window, and
released entities are filtered when restoring it. Ordinary non-nested draws reuse
their recording storage. A plain non-view model read subscribes the window but
does not itself create a cached-subtree invalidation path; existing view
invalidation and explicit refresh semantics still apply.

WGPU was compared from the pinned `cddb2f5d80dc0009b2ef6767635140cacf90a007`
to `bceb82ba1d7fa61dfe0afab3a26514c6367cab93` (268 commits). Selected candidates
are Vulkan descriptor-pool recovery (`573944a9712a`) and upload barrier coalescing
(`2edafa5240fd`). Do not advance the pin mechanically: `a979c33dcd6d` changes
`Queue::submit` to return `Option<SubmissionIndex>`, and buffer mapping changes
include `f4455f178d83`. Submission completion/resource retirement and readback
must migrate together, with Naga kept on the same revision. The free-chunk
discard API (`c0c938bb0bd6`) is not a direct local improvement: the maintained
renderer does not use WGPU's StagingBelt. No WGPU dependency update was adopted.

The WGPU migration changes presentation and mapped-buffer APIs, including local
material uploads and readback. Consumers must update their lockfile when adopting
this patch set. `Window::text_system()` now returns `&Rc<WindowTextSystem>`;
consumers explicitly storing that handle must use Rc and keep it on the UI thread.
The unused hash-layout APIs have been removed. Font-generation invalidation and
ordinary/wrapped double-frame cache reclamation remain in place. Platform/input/
scheduler migrations and hot reload are deferred. Taffy is pinned to 0.15.0;
layout consumers should verify grid/flex sizing and measured content when updating.

SVG parsing and rasterization use usvg/resvg 0.50. Font coverage checks use the
same Skrifa 0.49 release line as usvg, preserving collection-face selection and
missing-glyph fallback without a direct ttf-parser dependency. Consumers must
refresh their lockfile when adopting this change. Cosmic-text 0.19 still uses
fontdb 0.23, which retains ttf-parser; removing that remaining dependency requires
a separate text-stack migration. Platform/support crates and their AccessKit
integration remain on the pinned Zed baseline.

### Registry dependency refresh

The 2026-11-14 refresh updates the workspace lockfile within existing version
requirements, without advancing Git revisions, the exact Taffy pin or incompatible
release lines. It replaces yanked async-executor 1.14.0 with 1.14.1 and resolves
the certificate advisories with rustls-webpki 0.103.17 and webpki-roots /
webpki-root-certs 1.0.12. Consumers must refresh their own lockfiles; this
repository's lockfile does not constrain downstream resolution.

notify-rust 4.19 uses zbus 6 alongside zbus 5 retained by AccessKit/AT-SPI, ashpd
and oo7. Updating notifications alone does not remove the unmaintained
zbus_names, zvariant and zvariant_derive crates. Remaining unmaintained paths
also include cosmic-text/fontdb's ttf-parser, development HTTP support's
rustls-pemfile and the pinned macOS renderer's metal/paste. The EXR dependency
no longer introduces paste. These maintenance advisories are not suppressed;
`cargo deny check advisories` still fails on them.

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

Build outputs default to `.cache/target`. All four crates share the root Cargo
workspace and committed `Cargo.lock`; the former per-crate lockfiles are removed.
Existing `--manifest-path crates/<crate>/Cargo.toml` commands use this workspace.

`make lint` checks workspace formatting, then runs Clippy for all members, targets
and features. Clippy may emit warnings; a successful command is not a warning-free
result. `make test` runs each crate's library tests, including Linux platform tests.

For focused checks:

```sh
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui/Cargo.toml --lib gpu::tests --no-default-features
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui/Cargo.toml --lib scene::reclamation_tests --no-default-features
CARGO_TARGET_DIR=.cache/target cargo test --locked --manifest-path crates/gpui_wgpu/Cargo.toml --lib
```

Consumers must patch `gpui`, `gpui_wgpu`, `gpui_linux` and `gpui_platform` at the
application workspace root to the same repository commit, keeping other Zed crates
on the baseline revision. These workspace patches do not propagate into consumers.
The dependency direction remains platform constructors to Linux adapters to the
framework/renderer; GPUI does not acquire a runtime dependency on its platform
adapter. Keep any required third-party patches, including Zed's nonblocking
`calloop` revision, in the consuming application's root manifest.

The platform import preserves AccessKit, portal and credential integration and all
existing target branches/features; it does not upgrade their dependencies or
advance the Zed baseline. Future incompatible AccessKit changes must coordinate
framework types with platform adapters. Non-Linux platform branches are retained
but are not certified by Linux-only checks.

Apache-2.0 applies to the imported crates. Font fixtures retain the licenses in
their respective directories.
