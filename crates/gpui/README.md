# Welcome to GPUI!

GPUI is a hybrid immediate and retained mode, GPU accelerated, UI framework
for Rust, designed to support a wide variety of applications.

## Getting Started

This is the maintained GPUI extraction described in the [repository README](../../README.md),
not a crates.io release. Use the pinned Rust toolchain and pin all four maintained
crates to the same repository commit, following the root README's consumer patch
and lockfile instructions. GPUI is pre-1.0; API changes may require application updates.

Everything in a standalone GPUI app starts with an `Application`. You can create one with `gpui_platform::application()`, which picks the windowing and text backends for the host OS, and kick off your application by passing a callback to `Application::run()`. Inside this callback, you can create a new window with `App::open_window()` and register your first root view.

```rust,no_run
use gpui::*;

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        // ..
    });
}
```

### `gpui_platform`

The features on `gpui_platform` are platform-specific. Linux/WGPU is the maintained
integration here; the other platform branches below are retained but not certified.

- **macOS** — Rendering uses Metal and is always available, but glyph rasterization needs `font-kit`. Without it, GPUI falls back to a placeholder text system that lays text out but renders no glyphs.

- **Linux / FreeBSD** — enable at least one windowing backend for desktop windows: `wayland`, `x11`, or both. These features also compile the renderer and text system, so no separate text feature is needed.

- **Windows** — no features are required. Windowing uses Win32 and text uses DirectWrite. `font-kit` has no effect here.

### Composable image sampling effects (WGPU)

`img(source).with_effects(ImageEffects { ..Default::default() })` accepts a
fixed, independently composable schema: linear/radial alpha feathers, a
source-only Gaussian blur, continuous reflected sine-wave UV displacement,
stable local paper grain, and RGB quantization. These operate on the original
cached atlas image, without derived bitmap decodes or per-frame bitmap uploads.
Rounded corners, content masks, opacity, object fit and transformations retain
their normal meaning. Defaults leave image appearance unchanged.

Linear feather start/end use a normalized 0..1 projection along the angle in
radians; radial start/end use elliptical distance (0 at center, 1 at the image
edge). Each feather is disabled when end <= start, and simultaneous feathers
multiply source alpha. Blur radius, wave amplitude and grain cell size use
logical pixels. Blur support is clamped to 32 logical pixels and uses fixed 9x9
Gaussian quadrature (+/-3 sigma), with premultiplied bilinear interpolation.
Every fetch is clamped to the source image's own atlas tile. Wave frequency is
cycles across the visible image height, phase is radians, and edge reflection
is continuous, not a set of translated strips. Grain strength is 0..1 and is
anchored to local image coordinates; it has no frame-time seed. Quantization
uses 2..256 levels per RGB channel (below 2 disables it), before adding grain.
Nonfinite effect inputs are sanitized before device scaling; pixel-valued limits
apply in logical units. Active effects interpolate fractional source alpha in
premultiplied space, ignoring hidden transparent RGB. Ordinary no-effect images
retain the existing hardware sampler. Bilinear interpolation skips zero-weight
taps at exact texel centers and clamped edges.
Native Metal rejects nondefault image effects explicitly rather than silently
substituting a different effect. The low-level equivalent is
`Window::paint_image_with_effects`.

### Ordered scene filters (WGPU)

`div().backdrop_blur(px(sigma))` captures the pixels already painted beneath
that panel before painting its own background and content. Sigma is in logical
pixels, capped at 32 physical pixels after display scaling. Nonfinite or
nonpositive sigma disables the operation. This is a scene capture, distinct
from source-only image blur; it respects the inherited content mask and cumulative
element opacity. Fractional opacity interpolates premultiplied color and alpha
between the original backdrop and its blurred result; zero opacity submits no blur.
The final blur pass blends directly into the original target without an extra
texture or render pass.

`div().group_edge_fade(px(width))` isolates the children, then fades their
composed premultiplied alpha once at the top and bottom edges. Overlapping text
and icons compose normally before the mask. Width uses logical pixels and is
capped at half the group's height; nonfinite or nonpositive width disables the
mask. Layout, hit testing and clipping semantics are unchanged. Low-level paint
scopes use `Window::paint_backdrop_blur` and `Window::with_group_edge_fade`.
Pair the latter with `Window::prepaint_group_edge_fade` around child prepaint
to preserve cached subtree reuse under a stable group context. Paint records
the actual group context when it differs from prepaint, so a hidden subtree is
rebuilt when it becomes visible. Fully clipped groups retain paint bookkeeping
and input hitboxes, but submit no filters or child pixels, including cached replay.
Native Metal rejects active scene filters explicitly.

Filters are painter-order barriers, including cached scene replay. Nested blur
inside an isolated group captures its parent's scene; replacement compositing
can lower alpha correctly on translucent parents. Fresh glyphs request grayscale
rasterization during isolation, and retained subpixel glyphs use scalar coverage
instead of dual-source blending against a transparent destination.

An allocation-free preflight validates balanced groups before lazy effect-resource
creation. Per-window `Window::set_scene_filter_limits` controls target bytes and
nested layers, defaulting to 128 MiB and four layers; hard limits remain four
layers and 64 ordered filter operations. Exceeding those limits or the device's
filter-texture dimensions skips all scene filters for that frame, preserving
ordinary scene content. Unbalanced groups remain errors. Later in-budget frames
resume filtering automatically, including cached scene replay.
Fade-only scenes draw the root directly into the output and retain one viewport
target per active group depth. Backdrop scenes additionally need an offscreen
root and shared scratch; target-byte admission accounts for those actual targets.
Only offscreen roots require a final output-copy pass.
Scenes without admitted filters allocate no filter owner or targets. Targets are
reused for unchanged demand and retired when demand disappears, the surface
changes, or frame recording fails. Later valid frames recreate current demand;
submitted textures are released by ownership rather than destroyed in flight.
Scenes without filters or water bypass the ordered preparation traversal using
their recorded effect collections; no duplicate presence flags are maintained.

### Procedural CG water (WGPU)

`Window::paint_cg_water(bounds, CgWaterParams { phase, amplitude, highlight,
light_mode, style })` paints procedural water in painter order, respecting the
inherited content mask and element opacity. `CgWaterStyle::Perspective` reflects
a perspective day/night sky using six analytic wave-normal scales, Fresnel,
absorption and glints. `Still` refracts a teal shallow bed with sparse ripples.
`Stream` advects elongated caustics, `Lake` breaks a wide sky reflection into
horizontal wind ripples, `Rain` emits sparse fading impacts, `Garden` projects
soft leaf shadows over a refracting bed, and `Crystal` bends light through clear
curved sheets. Every style supports light/dark palettes and never samples scene
contents, scene depth or artwork.
Phase wraps at one, with identical zero/one endpoints; packet envelopes fade to
zero value and slope at their wrap boundaries. Nonfinite controls become zero;
amplitude and highlight clamp to 0..1. Zero amplitude is phase-independent and
motionless in every style. Highlight controls the selected material's light,
not a shared sun column.
The caller owns the clock and repaint scheduling; retained/static scenes have no
implicit animation. Native Metal rejects this operation explicitly.

The renderer lazily creates one pipeline and a bounded 256-panel uniform buffer
(256 times the device-aligned 64-byte placement stride). The typed style occupies
the existing spare uniform slot: each panel encodes 64 bytes and draws six
procedural vertices, with bounded analytic fragment work and no texture, bitmap
or geometry grid uploads. A nonempty frame uses one staging write spanning
`(panel_count - 1) * aligned_stride + 64` bytes, including initialized alignment
gaps; batching reduces queue writes but can transfer more bytes than separate
64-byte writes. More than 256 panels skips all procedural water for that frame
without growing a cache or suppressing ordinary content or admitted scene filters.
Water and filter admission are independent; subsequent in-limit frames resume
water rendering automatically.

### Cached views and animated images

`Entity::cached(style)` retains the caller-provided definite layout style while
reusing the rendered subtree only under matching inherited render context,
including rem size, inherited opacity and paint-only group state. Active
accessibility disables subtree reuse so nodes and actions are rebuilt each frame;
it does not discard the supplied layout style.

Animated WebP data uses the same frame, alpha, EXIF orientation and delay decoding
for `Image` and resource inputs. Orientation applies to each complete frame and
its dimensions; frame delays remain unchanged. Decoding retains all accepted
frames, rather than a single still image. Hidden and display-none images do not
sustain animation callbacks;
becoming visible restores demand. Clipping alone does not suspend animation.

### Additional Topics

- [Ownership and data flow](src/_ownership_and_data_flow.rs)
- [Accessibility](src/_accessibility.rs)
- [Contexts](docs/contexts.md)
- [Key dispatch](docs/key_dispatch.md)

### Dependencies

GPUI has various system dependencies that it needs in order to work.

#### macOS

On macOS, GPUI uses Metal for rendering. In order to use Metal, you need to do the following:

- Install [Xcode](https://apps.apple.com/us/app/xcode/id497799835?mt=12) from the macOS App Store, or from the [Apple Developer](https://developer.apple.com/download/all/) website. Note this requires a developer account.

> Ensure you launch Xcode after installing, and install the macOS components, which is the default option.

- Install [Xcode command line tools](https://developer.apple.com/xcode/resources/)

  ```sh
  xcode-select --install
  ```

- Ensure that the Xcode command line tools are using your newly installed copy of Xcode:

  ```sh
  sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
  ```

## The Big Picture

GPUI offers three different [registers](<https://en.wikipedia.org/wiki/Register_(sociolinguistics)>) depending on your needs:

- State management and communication with `Entity`'s. Whenever you need to store application state that communicates between different parts of your application, you'll want to use GPUI's entities. Entities are owned by GPUI and are only accessible through an owned smart pointer similar to an `Rc`. See the `app::context` module for more information.

- High level, declarative UI with views. All UI in GPUI starts with a view. A view is simply an `Entity` that can be rendered, by implementing the `Render` trait. At the start of each frame, GPUI will call this render method on the root view of a given window. Views build a tree of `elements`, lay them out and style them with a tailwind-style API, and then give them to GPUI to turn into pixels. See the `div` element for an all purpose swiss-army knife of rendering.

- Low level, imperative UI with Elements. Elements are the building blocks of UI in GPUI, and they provide a nice wrapper around an imperative API that provides as much flexibility and control as you need. Elements have total control over how they and their child elements are rendered and can be used for making efficient views into large lists, implement custom layouting for a code editor, and anything else you can think of. See the `element` module for more information.

Each of these registers has one or more corresponding contexts that can be accessed from all GPUI services. This context is your main interface to GPUI, and is used extensively throughout the framework.

## Other Resources

In addition to the systems above, GPUI provides a range of smaller services that are useful for building complex applications:

- Actions are user-defined structs that are used for converting keystrokes into logical operations in your UI. Use this for implementing keyboard shortcuts, such as cmd-q. See the `action` module for more information.

- Platform services, such as `quit the app` or `open a URL` are available as methods on the `app::App`.

- An async executor integrated with the platform's event loop. See the `executor` module for more information.

- The `[gpui::test]` macro provides a convenient way to write tests for your GPUI applications. Tests also have their own kind of context, a `TestAppContext` which provides ways of simulating common platform input. See `app::test_context` and `test` modules for more details.

The pinned [Zed source](https://github.com/zed-industries/zed/tree/df48a139ce9c292738b523252c12f044697e673f)
provides application examples. See the [repository README](../../README.md) for
local extensions, compatibility boundaries and development commands.
