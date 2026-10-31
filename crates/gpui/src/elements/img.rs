use crate::{
    AnyElement, AnyImageCache, App, Asset, AssetLogger, Bounds, DefiniteLength, Element, ElementId,
    Entity, GlobalElementId, Hitbox, Image, ImageCache, ImageEffects, InspectorElementId,
    InteractiveElement, Interactivity, IntoElement, LayoutId, Length, ObjectFit, Pixels,
    RenderImage, Resource, SharedString, SharedUri, StyleRefinement, Styled, Task, Transformation,
    Window, decode_static_image, decode_webp_image, px,
};
use anyhow::Result;

use futures::Future;
use gpui_util::ResultExt;
use image::{AnimationDecoder, ImageError, ImageFormat, codecs::gif::GifDecoder};
use scheduler::Instant;
use smallvec::SmallVec;
use std::{
    fs,
    io::{self, Cursor},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

use super::{Stateful, StatefulInteractiveElement};

/// The delay before showing the loading state.
pub const LOADING_DELAY: Duration = Duration::from_millis(200);

/// A type alias to the resource loader that the `img()` element uses.
///
/// Note: that this is only for Resources, like URLs or file paths.
/// Custom loaders, or external images will not use this asset loader
pub type ImgResourceLoader = AssetLogger<ImageAssetLoader>;

/// A source of image content.
#[derive(Clone)]
pub enum ImageSource {
    /// The image content will be loaded from some resource location
    Resource(Resource),
    /// Cached image data
    Render(Arc<RenderImage>),
    /// Cached image data
    Image(Arc<Image>),
    /// A custom loading function to use
    Custom(Arc<dyn Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>>>),
}

fn is_uri(uri: &str) -> bool {
    url::Url::from_str(uri).is_ok()
}

impl From<SharedUri> for ImageSource {
    fn from(value: SharedUri) -> Self {
        Self::Resource(Resource::Uri(value))
    }
}

impl<'a> From<&'a str> for ImageSource {
    fn from(s: &'a str) -> Self {
        if is_uri(s) {
            Self::Resource(Resource::Uri(s.to_string().into()))
        } else {
            Self::Resource(Resource::Embedded(s.to_string().into()))
        }
    }
}

impl From<String> for ImageSource {
    fn from(s: String) -> Self {
        if is_uri(&s) {
            Self::Resource(Resource::Uri(s.into()))
        } else {
            Self::Resource(Resource::Embedded(s.into()))
        }
    }
}

impl From<SharedString> for ImageSource {
    fn from(s: SharedString) -> Self {
        s.as_ref().into()
    }
}

impl From<&Path> for ImageSource {
    fn from(value: &Path) -> Self {
        Self::Resource(value.to_path_buf().into())
    }
}

impl From<Arc<Path>> for ImageSource {
    fn from(value: Arc<Path>) -> Self {
        Self::Resource(value.into())
    }
}

impl From<PathBuf> for ImageSource {
    fn from(value: PathBuf) -> Self {
        Self::Resource(value.into())
    }
}

impl From<Arc<RenderImage>> for ImageSource {
    fn from(value: Arc<RenderImage>) -> Self {
        Self::Render(value)
    }
}

impl From<Arc<Image>> for ImageSource {
    fn from(value: Arc<Image>) -> Self {
        Self::Image(value)
    }
}

impl<F> From<F> for ImageSource
where
    F: Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>> + 'static,
{
    fn from(value: F) -> Self {
        Self::Custom(Arc::new(value))
    }
}

/// The style of an image element.
pub struct ImageStyle {
    grayscale: bool,
    object_fit: ObjectFit,
    loading: Option<Box<dyn Fn() -> AnyElement>>,
    fallback: Option<Box<dyn Fn() -> AnyElement>>,
}

impl Default for ImageStyle {
    fn default() -> Self {
        Self {
            grayscale: false,
            object_fit: ObjectFit::Contain,
            loading: None,
            fallback: None,
        }
    }
}

/// Style an image element.
pub trait StyledImage: Sized {
    /// Get a mutable [ImageStyle] from the element.
    fn image_style(&mut self) -> &mut ImageStyle;

    /// Set the image to be displayed in grayscale.
    fn grayscale(mut self, grayscale: bool) -> Self {
        self.image_style().grayscale = grayscale;
        self
    }

    /// Set the object fit for the image.
    fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.image_style().object_fit = object_fit;
        self
    }

    /// Set a fallback function that will be invoked to render an error view should
    /// the image fail to load.
    fn with_fallback(mut self, fallback: impl Fn() -> AnyElement + 'static) -> Self {
        self.image_style().fallback = Some(Box::new(fallback));
        self
    }

    /// Set a fallback function that will be invoked to render a view while the image
    /// is still being loaded.
    fn with_loading(mut self, loading: impl Fn() -> AnyElement + 'static) -> Self {
        self.image_style().loading = Some(Box::new(loading));
        self
    }
}

impl StyledImage for Img {
    fn image_style(&mut self) -> &mut ImageStyle {
        &mut self.style
    }
}

impl StyledImage for Stateful<Img> {
    fn image_style(&mut self) -> &mut ImageStyle {
        &mut self.element.style
    }
}

/// An image element.
pub struct Img {
    interactivity: Interactivity,
    source: ImageSource,
    style: ImageStyle,
    image_cache: Option<AnyImageCache>,
    transformation: Option<Transformation>,
    automatic_animation_frames: bool,
    effects: ImageEffects,
}

/// Create a new image element.
#[track_caller]
pub fn img(source: impl Into<ImageSource>) -> Img {
    Img {
        interactivity: Interactivity::new(),
        source: source.into(),
        style: ImageStyle::default(),
        image_cache: None,
        transformation: None,
        automatic_animation_frames: true,
        effects: ImageEffects::default(),
    }
}

impl Img {
    /// Whether animated images request their own frames. Enabled by default.
    ///
    /// Disable this when an external owner schedules redraws (for example, to
    /// share an FPS cap). The existing per-element, time-based animation cursor
    /// still advances on those redraws; this does not pause the source animation.
    pub fn with_automatic_animation_frames(mut self, automatic: bool) -> Self {
        self.automatic_animation_frames = automatic;
        self
    }

    /// Compose source-image feathers, blur, continuous waves, grain and quantization.
    ///
    /// Requires WGPU. Defaults preserve normal image rendering and image caching.
    pub fn with_effects(mut self, effects: ImageEffects) -> Self {
        self.effects = effects;
        self
    }

    /// Transform the cropped image around the original element's center on the GPU.
    ///
    /// Rounded clipping is applied before transformation; the content mask remains
    /// in screen coordinates. Native Metal backends do not support this operation.
    pub fn with_transformation(mut self, transformation: Transformation) -> Self {
        self.transformation = Some(transformation);
        self
    }

    /// A list of all format extensions currently supported by this img element
    pub fn extensions() -> &'static [&'static str] {
        // This is the list in [image::ImageFormat::from_extension] + `svg`
        &[
            "avif", "jpg", "jpeg", "png", "gif", "webp", "tif", "tiff", "tga", "dds", "bmp", "ico",
            "hdr", "exr", "pbm", "pam", "ppm", "pgm", "ff", "farbfeld", "qoi", "svg",
        ]
    }

    /// Sets the image cache for the current node.
    ///
    /// If the `image_cache` is not explicitly provided, the function will determine the image cache by:
    ///
    /// 1. Checking if any ancestor node of the current node contains an `ImageCacheElement`, If such a node exists, the image cache specified by that ancestor will be used.
    /// 2. If no ancestor node contains an `ImageCacheElement`, the global image cache will be used as a fallback.
    ///
    /// This mechanism provides a flexible way to manage image caching, allowing precise control when needed,
    /// while ensuring a default behavior when no cache is explicitly specified.
    #[inline]
    pub fn image_cache<I: ImageCache>(self, image_cache: &Entity<I>) -> Self {
        Self {
            image_cache: Some(image_cache.clone().into()),
            ..self
        }
    }
}

impl Deref for Stateful<Img> {
    type Target = Img;

    fn deref(&self) -> &Self::Target {
        &self.element
    }
}

impl DerefMut for Stateful<Img> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.element
    }
}

/// The image state between frames
struct ImgState {
    frame_index: usize,
    last_frame_time: Option<Instant>,
    started_loading: Option<(Instant, Task<()>)>,
}

impl ImgState {
    fn advance_animation(&mut self, data: &RenderImage, now: Instant) {
        let frame_count = data.frame_count();
        debug_assert!(frame_count > 1 && self.frame_index < frame_count);
        let Some(last_frame_time) = self.last_frame_time else {
            self.last_frame_time = Some(now);
            return;
        };
        let mut elapsed = now - last_frame_time;
        let mut cycle_duration = Duration::ZERO;

        // Usually only the current frame is visited. A late redraw scans at most
        // one cycle before skipping whole cycles, never one iteration per missed
        // frame over an arbitrarily long suspension.
        for _ in 0..frame_count {
            let delay = Duration::from(data.delay(self.frame_index));
            if elapsed < delay {
                self.last_frame_time = Some(now - elapsed);
                return;
            }
            elapsed -= delay;
            cycle_duration += delay;
            self.frame_index = (self.frame_index + 1) % frame_count;
        }

        if cycle_duration.is_zero() {
            // No time-based position exists for an all-zero-delay animation.
            // Preserve progress at one frame per redraw without retaining debt.
            self.frame_index = (self.frame_index + 1) % frame_count;
            self.last_frame_time = Some(now);
            return;
        }
        let remainder = elapsed.as_nanos() % cycle_duration.as_nanos();
        elapsed = Duration::new(
            (remainder / 1_000_000_000) as u64,
            (remainder % 1_000_000_000) as u32,
        );
        // The remainder is shorter than a full cycle, including when individual
        // zero-delay frames must be skipped. This second scan is also bounded.
        for _ in 0..frame_count {
            let delay = Duration::from(data.delay(self.frame_index));
            if elapsed < delay {
                break;
            }
            elapsed -= delay;
            self.frame_index = (self.frame_index + 1) % frame_count;
        }
        self.last_frame_time = Some(now - elapsed);
    }
}

/// The image layout state between frames
pub struct ImgLayoutState {
    frame_index: usize,
    replacement: Option<AnyElement>,
}

impl Element for Img {
    type RequestLayoutState = ImgLayoutState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.interactivity.source_location()
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut layout_state = ImgLayoutState {
            frame_index: 0,
            replacement: None,
        };

        window.with_optional_element_state(global_id, |state, window| {
            let mut state = state.map(|state| {
                state.unwrap_or(ImgState {
                    frame_index: 0,
                    last_frame_time: None,
                    started_loading: None,
                })
            });

            let mut frame_index = state.as_ref().map(|state| state.frame_index).unwrap_or(0);

            let layout_id = self.interactivity.request_layout(
                global_id,
                inspector_id,
                window,
                cx,
                |mut style, window, cx| {
                    let mut replacement_id = None;

                    match self.source.use_data(
                        self.image_cache
                            .clone()
                            .or_else(|| window.image_cache_stack.last().cloned()),
                        window,
                        cx,
                    ) {
                        Some(Ok(data)) => {
                            let frame_count = data.frame_count();
                            let max_frame_index = frame_count.saturating_sub(1);

                            if let Some(state) = &mut state {
                                state.frame_index = state.frame_index.min(max_frame_index);
                                if frame_count > 1 && !cx.reduce_motion() {
                                    if window.is_window_active() {
                                        state.advance_animation(&data, Instant::now());
                                    } else {
                                        state.last_frame_time = None;
                                    }
                                } else {
                                    state.last_frame_time = None;
                                }
                                state.started_loading = None;
                                frame_index = state.frame_index;
                            }

                            let image_size = data.render_size(frame_index);

                            if style.aspect_ratio.is_none() {
                                style.aspect_ratio = Some(image_size.width / image_size.height);
                            }

                            if let Length::Auto = style.size.width {
                                style.size.width = match style.size.height {
                                    Length::Definite(DefiniteLength::Absolute(abs_length)) => {
                                        let height_px = abs_length.to_pixels(window.rem_size());
                                        Length::Definite(
                                            px(image_size.width.0 * height_px.0
                                                / image_size.height.0)
                                            .into(),
                                        )
                                    }
                                    _ => Length::Definite(image_size.width.into()),
                                };
                            }

                            if let Length::Auto = style.size.height {
                                style.size.height = match style.size.width {
                                    Length::Definite(DefiniteLength::Absolute(abs_length)) => {
                                        let width_px = abs_length.to_pixels(window.rem_size());
                                        Length::Definite(
                                            px(image_size.height.0 * width_px.0
                                                / image_size.width.0)
                                            .into(),
                                        )
                                    }
                                    _ => Length::Definite(image_size.height.into()),
                                };
                            }
                        }
                        Some(_err) => {
                            if let Some(fallback) = self.style.fallback.as_ref() {
                                let mut element = fallback();
                                replacement_id = Some(element.request_layout(window, cx));
                                layout_state.replacement = Some(element);
                            }
                            if let Some(state) = &mut state {
                                state.started_loading = None;
                            }
                        }
                        None => {
                            if let Some(state) = &mut state {
                                if let Some((started_loading, _)) = state.started_loading {
                                    if started_loading.elapsed() > LOADING_DELAY
                                        && let Some(loading) = self.style.loading.as_ref()
                                    {
                                        let mut element = loading();
                                        replacement_id = Some(element.request_layout(window, cx));
                                        layout_state.replacement = Some(element);
                                    }
                                } else {
                                    let current_view = window.current_view();
                                    let task = window.spawn(cx, async move |cx| {
                                        cx.background_executor().timer(LOADING_DELAY).await;
                                        cx.update(move |_, cx| {
                                            cx.notify(current_view);
                                        })
                                        .ok();
                                    });
                                    state.started_loading = Some((Instant::now(), task));
                                }
                            }
                        }
                    }

                    window.request_layout(style, replacement_id, cx)
                },
            );

            layout_state.frame_index = frame_index;

            ((layout_id, layout_state), state)
        })
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.interactivity.prepaint(
            global_id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            |_, _, hitbox, window, cx| {
                if let Some(replacement) = &mut request_layout.replacement {
                    replacement.prepaint(window, cx);
                }

                hitbox
            },
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let source = &self.source;
        self.interactivity.paint(
            global_id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |style, window, cx| {
                if let Some(Ok(data)) = source.use_data(
                    self.image_cache
                        .clone()
                        .or_else(|| window.image_cache_stack.last().cloned()),
                    window,
                    cx,
                ) {
                    if data.frame_count() == 0 {
                        return;
                    }

                    // Hidden elements never reach this paint continuation. Use its
                    // final computed display, rather than the layout-time style,
                    // without changing cursor advancement or paint bookkeeping.
                    if style.display != crate::Display::None
                        && global_id.is_some()
                        && self.automatic_animation_frames
                        && data.frame_count() > 1
                        && window.is_window_active()
                        && !cx.reduce_motion()
                    {
                        window.request_animation_frame();
                    }
                    let new_bounds = self
                        .style
                        .object_fit
                        .get_bounds(bounds, data.size(layout_state.frame_index));
                    let corner_radii = style.corner_radii.to_pixels(window.rem_size());
                    window
                        .paint_image_with_effects(
                            bounds,
                            new_bounds,
                            corner_radii,
                            data,
                            layout_state.frame_index,
                            self.style.grayscale,
                            self.transformation,
                            self.effects,
                        )
                        .log_err();
                } else if let Some(replacement) = &mut layout_state.replacement {
                    replacement.paint(window, cx);
                }
            },
        )
    }
}

impl Styled for Img {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

impl InteractiveElement for Img {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl IntoElement for Img {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl StatefulInteractiveElement for Img {}

impl ImageSource {
    pub(crate) fn use_data(
        &self,
        cache: Option<AnyImageCache>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        match self {
            ImageSource::Resource(resource) => {
                if let Some(cache) = cache {
                    cache.load(resource, window, cx)
                } else {
                    window.use_asset::<ImgResourceLoader>(resource, cx)
                }
            }
            ImageSource::Custom(loading_fn) => loading_fn(window, cx),
            ImageSource::Render(data) => Some(Ok(data.to_owned())),
            ImageSource::Image(data) => window.use_asset::<AssetLogger<ImageDecoder>>(data, cx),
        }
    }

    pub(crate) fn get_data(
        &self,
        cache: Option<AnyImageCache>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        match self {
            ImageSource::Resource(resource) => {
                if let Some(cache) = cache {
                    cache.load(resource, window, cx)
                } else {
                    window.get_asset::<ImgResourceLoader>(resource, cx)
                }
            }
            ImageSource::Custom(loading_fn) => loading_fn(window, cx),
            ImageSource::Render(data) => Some(Ok(data.to_owned())),
            ImageSource::Image(data) => window.get_asset::<AssetLogger<ImageDecoder>>(data, cx),
        }
    }

    /// Remove this image source from the asset system
    pub fn remove_asset(&self, cx: &mut App) {
        match self {
            ImageSource::Resource(resource) => {
                cx.remove_asset::<ImgResourceLoader>(resource);
            }
            ImageSource::Custom(_) | ImageSource::Render(_) => {}
            ImageSource::Image(data) => cx.remove_asset::<AssetLogger<ImageDecoder>>(data),
        }
    }

    /// Check whether this image source is present in the asset system (loading
    /// or loaded), without fetching it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_asset_cached(&self, cx: &App) -> bool {
        match self {
            ImageSource::Resource(resource) => cx.has_asset::<ImgResourceLoader>(resource),
            ImageSource::Custom(_) | ImageSource::Render(_) => false,
            ImageSource::Image(data) => cx.has_asset::<AssetLogger<ImageDecoder>>(data),
        }
    }
}

#[derive(Clone)]
enum ImageDecoder {}

impl Asset for ImageDecoder {
    type Source = Arc<Image>;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move { source.to_image_data(renderer).map_err(Into::into) }
    }
}

/// An image loader for the GPUI asset system
#[derive(Clone)]
pub enum ImageAssetLoader {}

impl Asset for ImageAssetLoader {
    type Source = Resource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let client = cx.http_client();
        // TODO: Can we make SVGs always rescale?
        // let scale_factor = cx.scale_factor();
        let svg_renderer = cx.svg_renderer();
        let asset_source = cx.asset_source().clone();
        async move {
            let bytes = match source.clone() {
                Resource::Path(uri) => fs::read(uri.as_ref())?,
                Resource::Uri(uri) => {
                    use anyhow::Context as _;
                    use futures::AsyncReadExt as _;

                    let mut response = client
                        .get(uri.as_ref(), ().into(), true)
                        .await
                        .with_context(|| format!("loading image asset from {uri:?}"))?;
                    let mut body = Vec::new();
                    response.body_mut().read_to_end(&mut body).await?;
                    if !response.status().is_success() {
                        let mut body = String::from_utf8_lossy(&body).into_owned();
                        let first_line = body.lines().next().unwrap_or("").trim_end();
                        body.truncate(first_line.len());
                        return Err(ImageCacheError::BadStatus {
                            uri,
                            status: response.status(),
                            body,
                        });
                    }
                    body
                }
                Resource::Embedded(path) => {
                    let data = asset_source.load(&path).ok().flatten();
                    if let Some(data) = data {
                        data.to_vec()
                    } else {
                        return Err(ImageCacheError::Asset(
                            format!("Embedded resource not found: {}", path).into(),
                        ));
                    }
                }
            };

            if let Ok(format) = image::guess_format(&bytes) {
                let data = match format {
                    ImageFormat::Gif => {
                        let decoder = GifDecoder::new(Cursor::new(&bytes))?;
                        let mut frames = SmallVec::new();

                        for frame in decoder.into_frames() {
                            match frame {
                                Ok(mut frame) => {
                                    // Convert from RGBA to BGRA.
                                    for pixel in frame.buffer_mut().chunks_exact_mut(4) {
                                        pixel.swap(0, 2);
                                    }
                                    frames.push(frame);
                                }
                                Err(err) => {
                                    log::debug!(
                                        "Skipping GIF frame in {source:?} due to decode error: {err}"
                                    );
                                }
                            }
                        }

                        if frames.is_empty() {
                            return Err(ImageCacheError::Other(Arc::new(anyhow::anyhow!(
                                "GIF could not be decoded: all frames failed ({source:?})"
                            ))));
                        }

                        frames
                    }
                    ImageFormat::WebP => decode_webp_image(&bytes)?,
                    _ => decode_static_image(&bytes, format)?,
                };

                Ok(Arc::new(RenderImage::new(data)))
            } else {
                svg_renderer
                    .render_single_frame(&bytes, 1.0)
                    .map_err(Into::into)
            }
        }
    }
}

/// An error that can occur when interacting with the image cache.
#[derive(Debug, Error, Clone)]
pub enum ImageCacheError {
    /// Some other kind of error occurred
    #[error("error: {0}")]
    Other(#[from] Arc<anyhow::Error>),
    /// An error that occurred while reading the image from disk.
    #[error("IO error: {0}")]
    Io(Arc<std::io::Error>),
    /// An error that occurred while processing an image.
    #[error("unexpected http status for {uri}: {status}, body: {body}")]
    BadStatus {
        /// The URI of the image.
        uri: SharedUri,
        /// The HTTP status code.
        status: http_client::StatusCode,
        /// The HTTP response body.
        body: String,
    },
    /// An error that occurred while processing an asset.
    #[error("asset error: {0}")]
    Asset(SharedString),
    /// An error that occurred while processing an image.
    #[error("image error: {0}")]
    Image(Arc<ImageError>),
    /// An error that occurred while processing an SVG.
    #[error("svg error: {0}")]
    Usvg(Arc<usvg::Error>),
}

impl From<anyhow::Error> for ImageCacheError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(Arc::new(value))
    }
}

impl From<io::Error> for ImageCacheError {
    fn from(value: io::Error) -> Self {
        Self::Io(Arc::new(value))
    }
}

impl From<usvg::Error> for ImageCacheError {
    fn from(value: usvg::Error) -> Self {
        Self::Usvg(Arc::new(value))
    }
}

impl From<image::ImageError> for ImageCacheError {
    fn from(value: image::ImageError) -> Self {
        Self::Image(Arc::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AppContext as _, ParentElement as _, TestAppContext, canvas, div, point, px, size,
    };
    use image::{Frame, ImageBuffer, Rgba};

    const TEST_IMG_ID: &str = "test-img";

    fn test_image(frame_count: usize) -> Arc<RenderImage> {
        let frame = Frame::new(ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 0])));
        Arc::new(RenderImage::new(SmallVec::from_iter(
            (0..frame_count).map(|_| frame.clone()),
        )))
    }

    fn test_image_with_size(width: u32, height: u32) -> Arc<RenderImage> {
        let frame = Frame::new(ImageBuffer::from_pixel(width, height, Rgba([0, 0, 0, 0])));
        Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1)))
    }

    fn timed_test_image(delays_ms: &[u32]) -> RenderImage {
        RenderImage::new(SmallVec::from_iter(delays_ms.iter().map(|&delay| {
            Frame::from_parts(
                ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 0])),
                0,
                0,
                image::Delay::from_numer_denom_ms(delay, 1),
            )
        })))
    }

    fn animation_cursor(frame_index: usize, last_frame_time: Option<Instant>) -> ImgState {
        ImgState {
            frame_index,
            last_frame_time,
            started_loading: None,
        }
    }

    #[test]
    fn animation_cursor_skips_expired_variable_delay_frames_and_preserves_remainder() {
        let data = timed_test_image(&[100, 200, 300]);
        let start = Instant::now();
        let mut state = animation_cursor(0, None);
        state.advance_animation(&data, start);
        assert_eq!(state.frame_index, 0);
        assert_eq!(state.last_frame_time, Some(start));

        for (elapsed_ms, expected_frame, frame_start_ms) in [
            (350, 2, 300),
            (590, 2, 300),
            (600, 0, 600),
            (850, 1, 700),
            (1_200, 0, 1_200),
        ] {
            state.advance_animation(&data, start + Duration::from_millis(elapsed_ms));
            assert_eq!(state.frame_index, expected_frame);
            assert_eq!(
                state.last_frame_time,
                Some(start + Duration::from_millis(frame_start_ms)),
            );
        }
    }

    #[test]
    fn animation_cursor_skips_whole_cycles_after_long_elapsed_time() {
        let data = timed_test_image(&[100, 200, 300]);
        let start = Instant::now();
        let mut state = animation_cursor(1, Some(start));
        // Ten years is an exact multiple of this 600 ms cycle. Starting from
        // frame 1 also proves the cycle skip does not assume a frame-0 origin.
        let now = start + Duration::from_secs(10 * 365 * 24 * 60 * 60) + Duration::from_millis(350);
        state.advance_animation(&data, now);
        assert_eq!(state.frame_index, 2);
        assert_eq!(
            state.last_frame_time,
            Some(now - Duration::from_millis(150))
        );
        state.advance_animation(&data, now + Duration::from_millis(149));
        assert_eq!(state.frame_index, 2);
        state.advance_animation(&data, now + Duration::from_millis(150));
        assert_eq!(state.frame_index, 0);
        assert_eq!(
            state.last_frame_time,
            Some(now + Duration::from_millis(150))
        );
    }

    #[test]
    fn animation_cursor_skips_zero_delay_frames_and_bounds_an_all_zero_cycle() {
        let data = timed_test_image(&[0, 100, 0, 200]);
        let start = Instant::now();
        let mut state = animation_cursor(0, Some(start));
        for (elapsed_ms, expected_frame, frame_start_ms) in [
            (0, 1, 0),
            (50, 1, 0),
            (100, 3, 100),
            (300, 1, 300),
            (550, 3, 400),
            (600, 1, 600),
        ] {
            state.advance_animation(&data, start + Duration::from_millis(elapsed_ms));
            assert_eq!(state.frame_index, expected_frame);
            assert_eq!(
                state.last_frame_time,
                Some(start + Duration::from_millis(frame_start_ms)),
            );
        }

        let data = timed_test_image(&[0, 0, 0]);
        let mut state = animation_cursor(0, Some(start));
        let now = start + Duration::from_secs(10 * 365 * 24 * 60 * 60);
        for expected_frame in [1, 2, 0, 1] {
            state.advance_animation(&data, now);
            assert_eq!(state.frame_index, expected_frame);
            assert_eq!(state.last_frame_time, Some(now));
        }
    }

    /// Overwrites the cached `frame_index` of the sibling `img` during paint.
    fn seed_frame_index(frame_index: usize) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.with_global_id(TEST_IMG_ID.into(), |id, window| {
                    window.with_element_state::<ImgState, _>(id, |state, _| {
                        let mut state = state.expect("img state should be initialized");
                        state.frame_index = frame_index;
                        ((), state)
                    });
                });
            },
        )
    }

    #[gpui::test]
    fn external_frame_owner_preserves_cursor_without_requesting_frames(cx: &mut TestAppContext) {
        struct AnimationView {
            automatic: bool,
            seed: bool,
            frame_index: std::rc::Rc<std::cell::Cell<usize>>,
        }

        impl crate::Render for AnimationView {
            fn render(&mut self, _: &mut Window, _: &mut crate::Context<Self>) -> impl IntoElement {
                let seed = self.seed;
                let frame_index = self.frame_index.clone();
                div()
                    .child(
                        img(test_image(2))
                            .with_automatic_animation_frames(!seed && self.automatic)
                            .id(TEST_IMG_ID),
                    )
                    .child(canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            window.with_global_id(TEST_IMG_ID.into(), |id, window| {
                                window.with_element_state::<ImgState, _>(id, |state, _| {
                                    let mut state = state.unwrap();
                                    if seed {
                                        state.frame_index = 0;
                                        state.last_frame_time =
                                            Some(Instant::now() - Duration::from_secs(1));
                                    }
                                    frame_index.set(state.frame_index);
                                    ((), state)
                                });
                            });
                        },
                    ))
            }
        }

        let window = cx.add_empty_window();
        window.update(|window, _| window.activate_window());
        window.run_until_parked();

        for automatic in [false, true] {
            let frame_index = std::rc::Rc::new(std::cell::Cell::new(0));
            let view = window.update(|_, cx| {
                cx.new(|_| AnimationView {
                    automatic,
                    seed: true,
                    frame_index: frame_index.clone(),
                })
            });
            window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                view.clone().into_any_element()
            });
            assert_eq!(
                window.update(|window, cx| window.simulate_next_frame(cx)),
                0
            );

            window.update(|_, cx| view.update(cx, |view, _| view.seed = false));
            window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                view.clone().into_any_element()
            });
            assert_eq!(
                frame_index.get(),
                1,
                "external redraws still advance the cursor"
            );
            assert_eq!(
                window.update(|window, cx| window.simulate_next_frame(cx)),
                usize::from(automatic),
                "ordinary Img retains automatic animation requests",
            );
        }
    }

    #[gpui::test]
    fn hidden_animated_images_stop_requesting_frames_and_resume_when_visible(
        cx: &mut TestAppContext,
    ) {
        struct AnimationView {
            hidden_mode: usize,
            image: Arc<RenderImage>,
        }

        impl crate::Render for AnimationView {
            fn render(&mut self, _: &mut Window, _: &mut crate::Context<Self>) -> impl IntoElement {
                let mut image = img(self.image.clone()).id(TEST_IMG_ID).size(px(10.));
                if self.hidden_mode == 1 {
                    image = image.invisible();
                } else if self.hidden_mode == 2 {
                    image = image.hidden();
                }
                let mut parent = div().child(image);
                if self.hidden_mode == 3 {
                    parent = parent.invisible();
                }
                parent
            }
        }

        let window = cx.add_empty_window();
        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        let view = window.update(|_, cx| {
            cx.new(|_| AnimationView {
                hidden_mode: 0,
                image: test_image(2),
            })
        });

        for hidden_mode in [1, 2, 3] {
            window.update(|_, cx| view.update(cx, |view, _| view.hidden_mode = hidden_mode));
            for _ in 0..2 {
                window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                    view.clone().into_any_element()
                });
                assert_eq!(
                    window.update(|window, cx| window.simulate_next_frame(cx)),
                    0,
                    "hidden mode {hidden_mode} must not sustain animation callbacks",
                );
            }

            window.update(|_, cx| view.update(cx, |view, _| view.hidden_mode = 0));
            window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                view.clone().into_any_element()
            });
            assert_eq!(
                window.update(|window, cx| window.simulate_next_frame(cx)),
                1,
                "showing an animated image must restore animation demand",
            );
            window.update(|window, _| {
                assert!(
                    window
                        .rendered_frame
                        .scene
                        .polychrome_sprites
                        .last()
                        .is_some()
                );
            });
        }
    }

    #[gpui::test]
    fn hovered_animated_images_use_final_visibility_for_frame_requests(cx: &mut TestAppContext) {
        struct AnimationView;

        impl crate::Render for AnimationView {
            fn render(&mut self, _: &mut Window, _: &mut crate::Context<Self>) -> impl IntoElement {
                img(test_image(2))
                    .id(TEST_IMG_ID)
                    .size(px(20.))
                    .hover(|style| style.invisible())
            }
        }

        let window = cx.open_window(size(px(100.), px(100.)), |_, _| AnimationView);
        cx.update_window(window.into(), |_, window, cx| {
            window.activate_window();
            window.simulate_mouse_move(point(px(10.), px(10.)), cx);
        })
        .unwrap();
        cx.run_until_parked();
        let requests = cx
            .update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.simulate_next_frame(cx)
            })
            .unwrap();
        assert_eq!(
            requests, 0,
            "hover-hidden image must not request another frame"
        );

        let requests = cx
            .update_window(window.into(), |_, window, cx| {
                window.simulate_mouse_move(point(px(50.), px(50.)), cx);
                window.refresh();
                window.draw(cx).clear(cx);
                window.simulate_next_frame(cx)
            })
            .unwrap();
        assert!(
            requests > 0,
            "leaving the image must restore animation demand"
        );
    }

    #[gpui::test]
    fn fully_clipped_animated_images_keep_requesting_frames(cx: &mut TestAppContext) {
        struct AnimationView;

        impl crate::Render for AnimationView {
            fn render(&mut self, _: &mut Window, _: &mut crate::Context<Self>) -> impl IntoElement {
                div()
                    .size(px(10.))
                    .overflow_hidden()
                    .child(img(test_image(2)).id(TEST_IMG_ID).size(px(10.)).ml(px(20.)))
            }
        }

        let window = cx.add_empty_window();
        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        let view = window.update(|_, cx| cx.new(|_| AnimationView));
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            view.clone().into_any_element()
        });
        assert_eq!(
            window.update(|window, cx| window.simulate_next_frame(cx)),
            1
        );
    }

    #[gpui::test]
    fn image_and_resource_sources_decode_webp_pixels_and_delays_identically(
        cx: &mut TestAppContext,
    ) {
        // Non-square frames with EXIF Orientation 6 must rotate clockwise.
        let animated_frames: &[(&[u8], u64)] = &[
            (&[0x77, 0x22, 0x11, 0x80, 0xC3, 0xB2, 0xA1, 0xD0], 70),
            (&[0x33, 0x44, 0x99, 0x40, 0x65, 0x43, 0x21, 0xE0], 130),
        ];
        let static_frames: &[(&[u8], u64)] = &[(&[0x77, 0x22, 0x11, 0x80], 0)];
        for (fixture, bytes, expected_size, expected_frames) in [
            (
                "two-frame-alpha.webp",
                include_bytes!("../../examples/image/two-frame-alpha.webp").as_slice(),
                size(1.into(), 2.into()),
                animated_frames,
            ),
            (
                "single-frame-alpha.webp",
                include_bytes!("../../examples/image/single-frame-alpha.webp").as_slice(),
                size(1.into(), 1.into()),
                static_frames,
            ),
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/image")
                .join(fixture);
            let image = Image::from_bytes(crate::ImageFormat::Webp, bytes.to_vec());
            let image_decoded = cx
                .update(|cx| image.to_image_data(cx.svg_renderer()))
                .expect("synthetic WebP image should decode");
            let resource_decoded = cx
                .update(|cx| {
                    futures::executor::block_on(ImageAssetLoader::load(Resource::from(path), cx))
                })
                .expect("synthetic WebP resource should decode");
            for decoded in [image_decoded, resource_decoded] {
                assert_eq!(decoded.frame_count(), expected_frames.len(), "{fixture}");
                for (index, (pixel, delay_ms)) in expected_frames.iter().enumerate() {
                    assert_eq!(
                        decoded.size(index),
                        expected_size,
                        "{fixture} frame {index}"
                    );
                    assert_eq!(
                        decoded.as_bytes(index).unwrap(),
                        *pixel,
                        "{fixture} frame {index}"
                    );
                    assert_eq!(
                        Duration::from(decoded.delay(index)),
                        Duration::from_millis(*delay_ms),
                        "{fixture} frame {index}",
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn zero_frame_image_does_not_panic_on_paint(cx: &mut TestAppContext) {
        cx.add_empty_window()
            .draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                img(ImageSource::Render(test_image(0))).into_any_element()
            });
    }

    #[gpui::test]
    fn image_object_fit_cover_crops_to_element_bounds(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        let image = test_image_with_size(200, 100);
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(image.clone()))
                .size_full()
                .object_fit(ObjectFit::Fill)
                .into_any_element()
        });
        let full_tile_bounds = window.update(|window, _| {
            window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("fill image should paint a sprite")
                .tile
                .bounds
        });

        window.draw(point(px(10.), px(20.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(image))
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        });

        let (rendered_bounds, rendered_tile_bounds, scale_factor) = window.update(|window, _| {
            let sprite = window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("cover image should paint a sprite");
            (sprite.bounds, sprite.tile.bounds, window.scale_factor())
        });
        assert_eq!(
            rendered_bounds,
            Bounds {
                origin: point(px(10.).scale(scale_factor), px(20.).scale(scale_factor)),
                size: size(px(100.).scale(scale_factor), px(100.).scale(scale_factor)),
            }
        );
        assert_eq!(
            (
                rendered_tile_bounds.origin.x.0 - full_tile_bounds.origin.x.0,
                rendered_tile_bounds.origin.y.0 - full_tile_bounds.origin.y.0,
                rendered_tile_bounds.size.width.0,
                rendered_tile_bounds.size.height.0,
            ),
            (50, 0, 100, 100),
        );
    }

    #[gpui::test]
    fn explicit_aspect_ratio_is_not_overridden_by_intrinsic_ratio(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();

        // A portrait image in a square container
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            div()
                .size(px(100.))
                .overflow_hidden()
                .child(
                    img(ImageSource::Render(test_image_with_size(100, 200)))
                        .size_full()
                        .aspect_square()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        });

        let (rendered_bounds, scale_factor) = window.update(|window, _| {
            let sprite = window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("contained image should paint a sprite");
            (sprite.bounds, window.scale_factor())
        });

        // The element stays 100x100, so the image is letterboxed horizontally
        assert_eq!(
            rendered_bounds,
            Bounds {
                origin: point(px(25.).scale(scale_factor), px(0.).scale(scale_factor)),
                size: size(px(50.).scale(scale_factor), px(100.).scale(scale_factor)),
            }
        );
    }

    #[gpui::test]
    fn image_object_fit_cover_clamps_corner_radii_to_visible_bounds(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(test_image_with_size(200, 100)))
                .size_full()
                .rounded(px(100.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        });

        let (corner_radius, expected_corner_radius) = window.update(|window, _| {
            (
                window
                    .rendered_frame
                    .scene
                    .polychrome_sprites
                    .last()
                    .map(|sprite| sprite.corner_radii.top_left),
                px(50.).scale(window.scale_factor()),
            )
        });
        assert_eq!(corner_radius, Some(expected_corner_radius));
    }

    #[gpui::test]
    fn fractional_image_rotation_keeps_the_painted_center_fixed(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        let image = test_image_with_size(200, 100);
        for degrees in [0.0_f32, 45.0, 90.0, 180.0, 270.0] {
            window.draw(
                point(px(100.25), px(100.25)),
                size(px(200.0), px(200.0)),
                |_, _| {
                    img(ImageSource::Render(image.clone()))
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .rounded_full()
                        .with_transformation(Transformation::rotate(crate::radians(
                            degrees.to_radians(),
                        )))
                        .into_any_element()
                },
            );
            window.update(|window, _| {
                let sprite = window
                    .rendered_frame
                    .scene
                    .polychrome_sprites
                    .last()
                    .unwrap();
                let center = sprite.bounds.center().map(|coordinate| px(coordinate.0));
                let rotated = sprite.transformation.apply(center);
                assert!((f32::from(rotated.x) - f32::from(center.x)).abs() < 0.001);
                assert!((f32::from(rotated.y) - f32::from(center.y)).abs() < 0.001);
            });
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    #[gpui::test]
    fn composed_image_effects_reuse_source_tile_and_preserve_crop(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        let image = test_image_with_size(200, 100);
        let effects = ImageEffects {
            linear_fade_start: 0.4,
            linear_fade_end: 1.0,
            radial_fade_start: 0.6,
            radial_fade_end: 1.0,
            blur_radius: 12.0,
            wave_amplitude: 8.0,
            wave_frequency: 2.0,
            grain: 0.08,
            grain_scale: 2.0,
            posterize_levels: 6.0,
            ..Default::default()
        };
        let mut baseline = None;
        for active in [false, true] {
            window.draw(point(px(10.), px(20.)), size(px(100.), px(100.)), |_, _| {
                img(ImageSource::Render(image.clone()))
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .rounded(px(20.0))
                    .with_effects(if active {
                        effects
                    } else {
                        ImageEffects::default()
                    })
                    .into_any_element()
            });
            let (sprite, scale) = window.update(|window, _| {
                (
                    *window
                        .rendered_frame
                        .scene
                        .polychrome_sprites
                        .last()
                        .unwrap(),
                    window.scale_factor(),
                )
            });
            if let Some(original) = baseline {
                let original: crate::PolychromeSprite = original;
                assert_eq!(sprite.tile, original.tile);
                assert_eq!(sprite.bounds, original.bounds);
                assert_eq!(sprite.content_mask, original.content_mask);
                assert_eq!(sprite.corner_radii, original.corner_radii);
                assert_eq!(sprite.transformation, original.transformation);
                assert_eq!(sprite.source_tile_bounds, original.source_tile_bounds);
                assert!(sprite.source_tile_bounds.size.width > sprite.tile.bounds.size.width);
                assert_eq!(sprite.effects, effects.scaled(scale));
            } else {
                baseline = Some(sprite);
            }
        }
    }

    #[gpui::test]
    fn stale_frame_index_is_clamped_when_image_changes(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();

        // Assert that a cached frame_index from a previous multi-frame image
        // does not cause an out-of-bounds panic when the image is replaced
        // with one that has fewer frames.
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            div()
                .child(img(ImageSource::Render(test_image(5))).id(TEST_IMG_ID))
                .child(seed_frame_index(4))
                .into_any_element()
        });
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(test_image(1)))
                .id(TEST_IMG_ID)
                .into_any_element()
        });
    }
}
