use crate::{FontId, GlyphId, Pixels, PlatformTextSystem, Point, SharedString, Size, point, px};
use collections::FxHashMap;
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    cell::{Cell, RefCell},
    hash::{Hash, Hasher},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::LineWrapper;

/// A laid out and styled line of text
#[derive(Default, Debug)]
pub struct LineLayout {
    /// The font size for this line
    pub font_size: Pixels,
    /// The width of the line
    pub width: Pixels,
    /// The ascent of the line
    pub ascent: Pixels,
    /// The descent of the line
    pub descent: Pixels,
    /// The shaped runs that make up this line
    pub runs: Vec<ShapedRun>,
    /// The length of the line in utf-8 bytes
    pub len: usize,
}

/// A run of text that has been shaped .
#[derive(Debug, Clone)]
pub struct ShapedRun {
    /// The font id for this run
    pub font_id: FontId,
    /// The glyphs that make up this run
    pub glyphs: Vec<ShapedGlyph>,
}

/// A single glyph, ready to paint.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    /// The ID for this glyph, as determined by the text system.
    pub id: GlyphId,

    /// The position of this glyph in its containing line.
    pub position: Point<Pixels>,

    /// The index of this glyph in the original text.
    pub index: usize,

    /// Whether this glyph is an emoji
    pub is_emoji: bool,
}

impl LineLayout {
    /// The index for the character at the given x coordinate
    pub fn index_for_x(&self, x: Pixels) -> Option<usize> {
        if x >= self.width {
            None
        } else {
            for run in self.runs.iter().rev() {
                for glyph in run.glyphs.iter().rev() {
                    if glyph.position.x <= x {
                        return Some(glyph.index);
                    }
                }
            }
            Some(0)
        }
    }

    /// closest_index_for_x returns the character boundary closest to the given x coordinate
    /// (e.g. to handle aligning up/down arrow keys)
    pub fn closest_index_for_x(&self, x: Pixels) -> usize {
        let mut prev_index = 0;
        let mut prev_x = px(0.);

        for run in self.runs.iter() {
            for glyph in run.glyphs.iter() {
                if glyph.position.x >= x {
                    if glyph.position.x - x < x - prev_x {
                        return glyph.index;
                    } else {
                        return prev_index;
                    }
                }
                prev_index = glyph.index;
                prev_x = glyph.position.x;
            }
        }

        if self.len == 1 {
            if x > self.width / 2. {
                return 1;
            } else {
                return 0;
            }
        }

        self.len
    }

    /// The x position of the character at the given index
    pub fn x_for_index(&self, index: usize) -> Pixels {
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return glyph.position.x;
                }
            }
        }
        self.width
    }

    /// The corresponding Font at the given index
    pub fn font_id_for_index(&self, index: usize) -> Option<FontId> {
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return Some(run.font_id);
                }
            }
        }
        None
    }

    /// Split this layout at a byte index, returning `(prefix, suffix)`.
    ///
    /// - `prefix` contains glyphs for bytes `[0, byte_index)`; `suffix` contains glyphs
    ///   for bytes `[byte_index, len)` with byte indices rebased to 0.
    /// - Byte-ordered glyphs keep their original positions in `prefix`, whose width ends
    ///   at the split point's x; `suffix` is shifted left by that x.
    /// - Otherwise (bidirectional text) each half keeps its glyphs' left-to-right order and
    ///   advances, packed from x=0. Either way the widths sum to the original width.
    /// - `font_size`, `ascent`, and `descent` are copied to both halves.
    pub fn split_at(&self, byte_index: usize) -> (LineLayout, LineLayout) {
        let empty_half = |len| LineLayout {
            font_size: self.font_size,
            width: px(0.),
            ascent: self.ascent,
            descent: self.descent,
            runs: Vec::new(),
            len,
        };
        let mut left = empty_half(byte_index);
        let mut right = empty_half(self.len - byte_index);

        let mut new_x: Vec<Vec<Pixels>> = self
            .runs
            .iter()
            .map(|run| run.glyphs.iter().map(|glyph| glyph.position.x).collect())
            .collect();
        let byte_ordered = self
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index))
            .is_sorted();
        if byte_ordered {
            // Glyph offsets may break x order, so cut at the split glyph's x, not by advances.
            let split_x = self.x_for_index(byte_index);
            for (run, new_x) in self.runs.iter().zip(&mut new_x) {
                for (glyph, x) in run.glyphs.iter().zip(new_x) {
                    if glyph.index >= byte_index {
                        *x -= split_x;
                    }
                }
            }
            left.width = split_x;
            right.width = self.width - split_x;
        } else {
            // Runs list right-to-left glyphs from right to left, so visit glyphs by x. A
            // glyph's advance extends to the next glyph to its right (or the line's end), and
            // the gap before the leftmost glyph belongs to the prefix. Each half lays its
            // glyphs out at its own running width, keeping every advance.
            let mut by_x: Vec<(usize, usize)> = self
                .runs
                .iter()
                .enumerate()
                .flat_map(|(run_ix, run)| (0..run.glyphs.len()).map(move |ix| (run_ix, ix)))
                .collect();
            let x_of = |&(run_ix, ix): &(usize, usize)| self.runs[run_ix].glyphs[ix].position.x;
            by_x.sort_by(|a, b| x_of(a).0.total_cmp(&x_of(b).0));

            left.width = by_x.first().map_or(self.width, x_of);
            for (order, glyph) in by_x.iter().enumerate() {
                let next_x = by_x.get(order + 1).map_or(self.width, x_of);
                let (run_ix, ix) = *glyph;
                let half = if self.runs[run_ix].glyphs[ix].index < byte_index {
                    &mut left
                } else {
                    &mut right
                };
                new_x[run_ix][ix] = half.width;
                half.width += next_x - x_of(glyph);
            }
        }

        for (run, new_x) in self.runs.iter().zip(new_x) {
            let mut left_glyphs = Vec::new();
            let mut right_glyphs = Vec::new();
            for (glyph, x) in run.glyphs.iter().zip(new_x) {
                let (glyphs, index_base) = if glyph.index < byte_index {
                    (&mut left_glyphs, 0)
                } else {
                    (&mut right_glyphs, byte_index)
                };
                glyphs.push(ShapedGlyph {
                    id: glyph.id,
                    position: point(x, glyph.position.y),
                    index: glyph.index - index_base,
                    is_emoji: glyph.is_emoji,
                });
            }
            for (half, glyphs) in [(&mut left, left_glyphs), (&mut right, right_glyphs)] {
                if !glyphs.is_empty() {
                    half.runs.push(ShapedRun {
                        font_id: run.font_id,
                        glyphs,
                    });
                }
            }
        }

        (left, right)
    }

    fn compute_wrap_boundaries(
        &self,
        text: &str,
        wrap_width: Pixels,
        max_lines: Option<usize>,
    ) -> SmallVec<[WrapBoundary; 1]> {
        let mut boundaries = SmallVec::new();
        let mut first_non_whitespace_ix = None;
        let mut last_candidate_ix = None;
        let mut last_candidate_x = px(0.);
        let mut last_boundary = WrapBoundary {
            run_ix: 0,
            glyph_ix: 0,
        };
        let mut last_boundary_x = px(0.);
        let mut prev_ch = '\0';
        let mut glyphs = self
            .runs
            .iter()
            .enumerate()
            .flat_map(move |(run_ix, run)| {
                run.glyphs.iter().enumerate().map(move |(glyph_ix, glyph)| {
                    let character = text[glyph.index..].chars().next().unwrap();
                    (
                        WrapBoundary { run_ix, glyph_ix },
                        character,
                        glyph.position.x,
                    )
                })
            })
            .peekable();

        while let Some((boundary, ch, x)) = glyphs.next() {
            if ch == '\n' {
                continue;
            }

            // Here is very similar to `LineWrapper::wrap_line` to determine text wrapping,
            // but there are some differences, so we have to duplicate the code here.
            if LineWrapper::is_word_char(ch) {
                if prev_ch == ' ' && ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            } else {
                if ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            }

            if ch != ' ' && first_non_whitespace_ix.is_none() {
                first_non_whitespace_ix = Some(boundary);
            }

            let next_x = glyphs.peek().map_or(self.width, |(_, _, x)| *x);
            let width = next_x - last_boundary_x;

            if width > wrap_width && boundary > last_boundary {
                // When used line_clamp, we should limit the number of lines.
                if let Some(max_lines) = max_lines
                    && boundaries.len() >= max_lines.saturating_sub(1)
                {
                    break;
                }

                if let Some(last_candidate_ix) = last_candidate_ix.take() {
                    last_boundary = last_candidate_ix;
                    last_boundary_x = last_candidate_x;
                } else {
                    last_boundary = boundary;
                    last_boundary_x = x;
                }
                boundaries.push(last_boundary);
            }
            prev_ch = ch;
        }

        boundaries
    }
}

/// A line of text that has been wrapped to fit a given width
#[derive(Default, Debug)]
pub struct WrappedLineLayout {
    /// The line layout, pre-wrapping.
    pub unwrapped_layout: Arc<LineLayout>,

    /// The boundaries at which the line was wrapped
    pub wrap_boundaries: SmallVec<[WrapBoundary; 1]>,

    /// The width of the line, if it was wrapped
    pub wrap_width: Option<Pixels>,
}

/// A boundary at which a line was wrapped
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WrapBoundary {
    /// The index in the run just before the line was wrapped
    pub run_ix: usize,
    /// The index of the glyph just before the line was wrapped
    pub glyph_ix: usize,
}

impl WrappedLineLayout {
    /// The length of the underlying text, in utf8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.unwrapped_layout.len
    }

    /// The width of this line, in pixels, whether or not it was wrapped.
    pub fn width(&self) -> Pixels {
        self.wrap_width
            .unwrap_or(Pixels::MAX)
            .min(self.unwrapped_layout.width)
    }

    /// The size of the whole wrapped text, for the given line_height.
    /// can span multiple lines if there are multiple wrap boundaries.
    pub fn size(&self, line_height: Pixels) -> Size<Pixels> {
        Size {
            width: self.width(),
            height: line_height * (self.wrap_boundaries.len() + 1),
        }
    }

    /// The ascent of a line in this layout
    pub fn ascent(&self) -> Pixels {
        self.unwrapped_layout.ascent
    }

    /// The descent of a line in this layout
    pub fn descent(&self) -> Pixels {
        self.unwrapped_layout.descent
    }

    /// The wrap boundaries in this layout
    pub fn wrap_boundaries(&self) -> &[WrapBoundary] {
        &self.wrap_boundaries
    }

    /// The font size of this layout
    pub fn font_size(&self) -> Pixels {
        self.unwrapped_layout.font_size
    }

    /// The runs in this layout, sans wrapping
    pub fn runs(&self) -> &[ShapedRun] {
        &self.unwrapped_layout.runs
    }

    /// The index corresponding to a given position in this layout for the given line height.
    ///
    /// See also [`Self::closest_index_for_position`].
    pub fn index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, false)
    }

    /// The closest index to a given position in this layout for the given line height.
    ///
    /// Closest means the character boundary closest to the given position.
    ///
    /// See also [`LineLayout::closest_index_for_x`].
    pub fn closest_index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, true)
    }

    fn _index_for_position(
        &self,
        mut position: Point<Pixels>,
        line_height: Pixels,
        closest: bool,
    ) -> Result<usize, usize> {
        let wrapped_line_ix = (position.y / line_height) as usize;

        let wrapped_line_start_index;
        let wrapped_line_start_x;
        if wrapped_line_ix > 0 {
            let Some(line_start_boundary) = self.wrap_boundaries.get(wrapped_line_ix - 1) else {
                return Err(0);
            };
            let run = &self.unwrapped_layout.runs[line_start_boundary.run_ix];
            let glyph = &run.glyphs[line_start_boundary.glyph_ix];
            wrapped_line_start_index = glyph.index;
            wrapped_line_start_x = glyph.position.x;
        } else {
            wrapped_line_start_index = 0;
            wrapped_line_start_x = Pixels::ZERO;
        };

        let wrapped_line_end_index;
        let wrapped_line_end_x;
        if wrapped_line_ix < self.wrap_boundaries.len() {
            let next_wrap_boundary_ix = wrapped_line_ix;
            let next_wrap_boundary = self.wrap_boundaries[next_wrap_boundary_ix];
            let run = &self.unwrapped_layout.runs[next_wrap_boundary.run_ix];
            let glyph = &run.glyphs[next_wrap_boundary.glyph_ix];
            wrapped_line_end_index = glyph.index;
            wrapped_line_end_x = glyph.position.x;
        } else {
            wrapped_line_end_index = self.unwrapped_layout.len;
            wrapped_line_end_x = self.unwrapped_layout.width;
        };

        let mut position_in_unwrapped_line = position;
        position_in_unwrapped_line.x += wrapped_line_start_x;
        if position_in_unwrapped_line.x < wrapped_line_start_x {
            Err(wrapped_line_start_index)
        } else if position_in_unwrapped_line.x >= wrapped_line_end_x {
            Err(wrapped_line_end_index)
        } else {
            if closest {
                Ok(self
                    .unwrapped_layout
                    .closest_index_for_x(position_in_unwrapped_line.x))
            } else {
                // The shaper can place a trailing zero-width wrap boundary glyph slightly past
                // the line's width, so the row can extend past where `index_for_x` has glyphs.
                self.unwrapped_layout
                    .index_for_x(position_in_unwrapped_line.x)
                    .ok_or(wrapped_line_end_index)
            }
        }
    }

    /// Returns the pixel position for the given byte index.
    pub fn position_for_index(&self, index: usize, line_height: Pixels) -> Option<Point<Pixels>> {
        let mut line_start_ix = 0;
        let mut line_end_indices = self
            .wrap_boundaries
            .iter()
            .map(|wrap_boundary| {
                let run = &self.unwrapped_layout.runs[wrap_boundary.run_ix];
                let glyph = &run.glyphs[wrap_boundary.glyph_ix];
                glyph.index
            })
            .chain([self.len()])
            .enumerate();
        for (ix, line_end_ix) in line_end_indices {
            let line_y = ix as f32 * line_height;
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_start_ix = line_end_ix;
                continue;
            } else {
                let line_start_x = self.unwrapped_layout.x_for_index(line_start_ix);
                let x = self.unwrapped_layout.x_for_index(index) - line_start_x;
                return Some(point(x, line_y));
            }
        }

        None
    }
}

pub(crate) struct LineLayoutCache {
    previous_frame: RefCell<FrameCache>,
    current_frame: RefCell<FrameCache>,
    platform_text_system: Arc<dyn PlatformTextSystem>,
    /// Advances when [`TextSystem::add_fonts`] successfully changes the font database.
    font_generation: Arc<AtomicUsize>,
    /// Records the generation represented by both frame caches.
    cached_font_generation: Cell<usize>,
}

#[derive(Default)]
struct FrameCache {
    lines: FxHashMap<Arc<CacheKey>, Arc<LineLayout>>,
    wrapped_lines: FxHashMap<Arc<CacheKey>, Arc<WrappedLineLayout>>,
    used_lines: Vec<Arc<CacheKey>>,
    used_wrapped_lines: Vec<Arc<CacheKey>>,

    low_demand_frames: u16,
}

#[derive(Clone, Default)]
pub(crate) struct LineLayoutIndex {
    font_generation: usize,
    lines_index: usize,
    wrapped_lines_index: usize,
}

impl LineLayoutCache {
    pub fn new(
        platform_text_system: Arc<dyn PlatformTextSystem>,
        font_generation: Arc<AtomicUsize>,
    ) -> Self {
        let cached_font_generation = font_generation.load(Ordering::Acquire);
        Self {
            previous_frame: RefCell::default(),
            current_frame: RefCell::default(),
            platform_text_system,
            font_generation,
            cached_font_generation: Cell::new(cached_font_generation),
        }
    }

    pub fn layout_index(&self) -> LineLayoutIndex {
        let font_generation = self.clear_if_font_generation_changed();
        let frame = self.current_frame.borrow();
        LineLayoutIndex {
            font_generation,
            lines_index: frame.used_lines.len(),
            wrapped_lines_index: frame.used_wrapped_lines.len(),
        }
    }

    pub fn reuse_layouts(&self, range: Range<LineLayoutIndex>) {
        let font_generation = self.clear_if_font_generation_changed();
        if range.start.font_generation != font_generation
            || range.end.font_generation != font_generation
        {
            return;
        }
        let current_frame = &mut *self.current_frame.borrow_mut();
        let previous_frame = &mut *self.previous_frame.borrow_mut();

        for key in &previous_frame.used_lines[range.start.lines_index..range.end.lines_index] {
            if let Some((key, line)) = previous_frame.lines.remove_entry(key) {
                current_frame.lines.insert(key, line);
            }
            current_frame.used_lines.push(key.clone());
        }

        for key in &previous_frame.used_wrapped_lines
            [range.start.wrapped_lines_index..range.end.wrapped_lines_index]
        {
            if let Some((key, line)) = previous_frame.wrapped_lines.remove_entry(key) {
                current_frame.wrapped_lines.insert(key, line);
            }
            current_frame.used_wrapped_lines.push(key.clone());
        }
    }

    pub fn truncate_layouts(&self, index: LineLayoutIndex) {
        let font_generation = self.clear_if_font_generation_changed();
        if index.font_generation != font_generation {
            return;
        }
        let mut current_frame = self.current_frame.borrow_mut();
        current_frame.used_lines.truncate(index.lines_index);
        current_frame
            .used_wrapped_lines
            .truncate(index.wrapped_lines_index);
    }

    pub fn finish_frame(&self) {
        let _font_generation = self.clear_if_font_generation_changed();
        let mut curr_frame = self.current_frame.borrow_mut();
        let mut prev_frame = self.previous_frame.borrow_mut();
        std::mem::swap(&mut *prev_frame, &mut *curr_frame);
        let demand = curr_frame
            .lines
            .len()
            .max(curr_frame.wrapped_lines.len())
            .max(curr_frame.used_lines.len())
            .max(curr_frame.used_wrapped_lines.len());
        curr_frame.low_demand_frames = if demand <= 256 {
            curr_frame.low_demand_frames + 1
        } else {
            0
        };
        curr_frame.lines.clear();
        curr_frame.wrapped_lines.clear();
        curr_frame.used_lines.clear();
        curr_frame.used_wrapped_lines.clear();

        // The previous frame still owns all replay indices and Arc layouts.
        // Trim only the discarded half after 120 small uses of that half.
        if curr_frame.low_demand_frames >= 120 {
            curr_frame.low_demand_frames = 0;
            macro_rules! trim {
                ($($field:ident),+ $(,)?) => { $(
                    if curr_frame.$field.capacity() > 1024 {
                        curr_frame.$field.shrink_to(256);
                    }
                )+ };
            }
            trim!(lines, wrapped_lines, used_lines, used_wrapped_lines);
        }
    }

    pub fn layout_wrapped_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> Arc<WrappedLineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let _font_generation = self.clear_if_font_generation_changed();
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width,
            max_lines,
            force_width: None,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.borrow();
        if let Some(layout) = current_frame.wrapped_lines.get(key) {
            return layout.clone();
        }

        let previous_frame_entry = self
            .previous_frame
            .borrow_mut()
            .wrapped_lines
            .remove_entry(key);
        drop(current_frame);
        if let Some((key, layout)) = previous_frame_entry {
            let mut current_frame = self.current_frame.borrow_mut();
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key);
            layout
        } else {
            let text = SharedString::from(text);
            let unwrapped_layout = self.layout_line::<&SharedString>(&text, font_size, runs, None);
            let wrap_boundaries = if let Some(wrap_width) = wrap_width {
                unwrapped_layout.compute_wrap_boundaries(text.as_ref(), wrap_width, max_lines)
            } else {
                SmallVec::new()
            };
            let layout = Arc::new(WrappedLineLayout {
                unwrapped_layout,
                wrap_boundaries,
                wrap_width,
            });
            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width,
                max_lines,
                force_width: None,
            });

            let mut current_frame = self.current_frame.borrow_mut();
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key);

            layout
        }
    }

    pub fn layout_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let _font_generation = self.clear_if_font_generation_changed();
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width: None,
            max_lines: None,
            force_width,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.borrow();
        if let Some(layout) = current_frame.lines.get(key) {
            return layout.clone();
        }

        drop(current_frame);
        let previous_frame_entry = self.previous_frame.borrow_mut().lines.remove_entry(key);
        if let Some((key, layout)) = previous_frame_entry {
            let mut current_frame = self.current_frame.borrow_mut();
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        } else {
            let text = SharedString::from(text);
            let mut layout = self
                .platform_text_system
                .layout_line(&text, font_size, runs);

            if let Some(force_width) = force_width {
                apply_force_width_to_layout(&mut layout, force_width);
            }

            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width: None,
                max_lines: None,
                force_width,
            });
            let layout = Arc::new(layout);
            let mut current_frame = self.current_frame.borrow_mut();
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        }
    }

    fn clear_if_font_generation_changed(&self) -> usize {
        let font_generation = self.font_generation.load(Ordering::Acquire);
        if self.cached_font_generation.get() == font_generation {
            return font_generation;
        }

        *self.current_frame.borrow_mut() = FrameCache::default();
        *self.previous_frame.borrow_mut() = FrameCache::default();
        self.cached_font_generation.set(font_generation);
        font_generation
    }
}

// Combining marks (e.g. Thai vowel signs, Arabic diacritics) are shaped by
// HarfBuzz at the same x position as their base character. The force-width
// loop must not advance the cell counter for these zero-advance glyphs,
// otherwise they get displaced into the next cell. We detect them by checking
// whether shaped x has advanced by at least half a cell beyond the last base.
fn apply_force_width_to_layout(layout: &mut LineLayout, force_width: Pixels) {
    let mut glyph_pos: usize = 0;
    // NEG_INFINITY ensures the first glyph is always classified as a base.
    let mut last_base_shaped_x = px(f32::NEG_INFINITY);
    let mut last_base_actual_x = px(0.);

    for run in layout.runs.iter_mut() {
        for glyph in run.glyphs.iter_mut() {
            let shaped_x = glyph.position.x;

            if shaped_x > last_base_shaped_x + force_width * 0.5 {
                let forced_x = glyph_pos * force_width;
                if (shaped_x - forced_x).abs() > px(1.) {
                    glyph.position.x = forced_x;
                }
                last_base_shaped_x = shaped_x;
                last_base_actual_x = glyph.position.x;
                glyph_pos += 1;
            } else {
                glyph.position.x = last_base_actual_x + (shaped_x - last_base_shaped_x);
            }
        }
    }
}

/// A run of text with a single font.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[expect(missing_docs)]
pub struct FontRun {
    pub len: usize,
    pub font_id: FontId,
}

trait AsCacheKeyRef {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_>;
}

#[derive(Clone, Debug, Eq)]
struct CacheKey {
    text: SharedString,
    font_size: Pixels,
    runs: SmallVec<[FontRun; 1]>,
    wrap_width: Option<Pixels>,
    max_lines: Option<usize>,
    force_width: Option<Pixels>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct CacheKeyRef<'a> {
    text: &'a str,
    font_size: Pixels,
    runs: &'a [FontRun],
    wrap_width: Option<Pixels>,
    max_lines: Option<usize>,
    force_width: Option<Pixels>,
}

impl PartialEq for dyn AsCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsCacheKeyRef) -> bool {
        self.as_cache_key_ref() == other.as_cache_key_ref()
    }
}

impl Eq for dyn AsCacheKeyRef + '_ {}

impl Hash for dyn AsCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state)
    }
}

impl AsCacheKeyRef for CacheKey {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        CacheKeyRef {
            text: &self.text,
            font_size: self.font_size,
            runs: self.runs.as_slice(),
            wrap_width: self.wrap_width,
            max_lines: self.max_lines,
            force_width: self.force_width,
        }
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_cache_key_ref().eq(&other.as_cache_key_ref())
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state);
    }
}

impl<'a> Borrow<dyn AsCacheKeyRef + 'a> for Arc<CacheKey> {
    fn borrow(&self) -> &(dyn AsCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsCacheKeyRef
    }
}

impl AsCacheKeyRef for CacheKeyRef<'_> {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphId;

    #[test]
    fn cold_wrapped_layout_replays_and_invalidates_after_font_changes() {
        let generation = Arc::new(AtomicUsize::new(0));
        let cache = LineLayoutCache::new(Arc::new(crate::NoopTextSystem), generation.clone());
        let start = cache.layout_index();
        let wrapped =
            cache.layout_wrapped_line("cold wrapped miss", px(16.), &[], Some(px(40.)), None);
        let end = cache.layout_index();
        assert_eq!(wrapped.len(), "cold wrapped miss".len());
        cache.finish_frame();
        cache.reuse_layouts(start.clone()..end.clone());
        // Carry a frame using only replay: no layout lookup may rescue an entry
        // left behind in the discarded cache.
        cache.finish_frame();
        cache.reuse_layouts(start.clone()..end.clone());
        let replayed =
            cache.layout_wrapped_line("cold wrapped miss", px(16.), &[], Some(px(40.)), None);
        assert!(Arc::ptr_eq(&wrapped, &replayed));
        cache.truncate_layouts(start.clone());
        assert_eq!(
            cache.layout_index().wrapped_lines_index,
            start.wrapped_lines_index
        );
        generation.fetch_add(1, Ordering::Release);
        cache.reuse_layouts(start.clone()..end);
        cache.truncate_layouts(start);
        let fresh =
            cache.layout_wrapped_line("cold wrapped miss", px(16.), &[], Some(px(40.)), None);
        assert!(!Arc::ptr_eq(&wrapped, &fresh));
        assert!(!Arc::ptr_eq(
            &wrapped.unwrapped_layout,
            &fresh.unwrapped_layout
        ));
        cache.finish_frame();
        cache.finish_frame();
        assert!(cache.previous_frame.borrow().lines.is_empty());
        assert!(cache.previous_frame.borrow().wrapped_lines.is_empty());
        assert!(cache.current_frame.borrow().used_lines.is_empty());
        assert!(cache.current_frame.borrow().used_wrapped_lines.is_empty());
    }

    #[test]
    fn wrapped_layout_cache_distinguishes_line_limits_in_both_orders_and_frames() {
        for cross_frame in [false, true] {
            for limits in [[Some(1), None], [None, Some(1)]] {
                let cache = LineLayoutCache::new(
                    Arc::new(crate::NoopTextSystem),
                    Arc::new(AtomicUsize::new(0)),
                );
                let layout = |max_lines| {
                    cache.layout_wrapped_line(
                        "abcdefghijkl",
                        px(10.),
                        &[],
                        Some(px(24.)),
                        max_lines,
                    )
                };
                let first = layout(limits[0]);
                if cross_frame {
                    cache.finish_frame();
                }
                let second = layout(limits[1]);
                for (wrapped, limit) in [(&first, limits[0]), (&second, limits[1])] {
                    let rows = limit.unwrap_or(3);
                    assert_eq!(wrapped.wrap_boundaries().len(), rows - 1);
                    assert_eq!(wrapped.size(px(16.)).height, px(16.) * rows);
                    assert_eq!(wrapped.width(), px(24.));
                }

                for _ in 0..2 {
                    for (limit, rows) in [(Some(1), 1), (Some(2), 2), (None, 3)] {
                        let wrapped = layout(limit);
                        assert_eq!(wrapped.wrap_boundaries().len(), rows - 1);
                        assert_eq!(wrapped.size(px(16.)).height, px(16.) * rows);
                        assert_eq!(wrapped.width(), px(24.));
                        assert_eq!(wrapped.len(), 12);
                        if rows > 1 {
                            assert_eq!(
                                wrapped.index_for_position(point(px(0.), px(16.)), px(16.)),
                                Ok(4),
                            );
                        }
                    }
                    cache.finish_frame();
                }
            }
        }
    }

    #[test]
    fn wrapped_unicode_layout_reuses_shaping_and_byte_boundaries() {
        let cache = LineLayoutCache::new(
            Arc::new(crate::NoopTextSystem),
            Arc::new(AtomicUsize::new(0)),
        );
        let text = "a😀é中";
        let start = cache.layout_index();
        let wrapped = cache.layout_wrapped_line(text, px(10.), &[], Some(px(12.)), None);
        let clamped = cache.layout_wrapped_line(text, px(10.), &[], Some(px(12.)), Some(1));
        let end = cache.layout_index();
        assert!(Arc::ptr_eq(
            &wrapped.unwrapped_layout,
            &clamped.unwrapped_layout
        ));
        assert!(clamped.wrap_boundaries().is_empty());
        assert!(!wrapped.wrap_boundaries().is_empty());
        assert_eq!(wrapped.len(), text.len());
        assert_eq!(
            wrapped.runs()[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 5, 7],
        );
        for (row, boundary) in wrapped.wrap_boundaries().iter().enumerate() {
            let byte_index = wrapped.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index;
            assert!(text.is_char_boundary(byte_index));
            assert_eq!(
                wrapped.index_for_position(point(px(0.), px(16.) * (row + 1)), px(16.)),
                Ok(byte_index),
            );
        }
        let rows = wrapped.wrap_boundaries().len() + 1;
        assert_eq!(
            wrapped.index_for_position(point(px(0.), px(16.) * rows), px(16.)),
            Err(0),
        );
        for _ in 0..2 {
            cache.finish_frame();
            cache.reuse_layouts(start.clone()..end.clone());
        }
        let replayed = cache.layout_wrapped_line(text, px(10.), &[], Some(px(12.)), None);
        assert!(Arc::ptr_eq(&wrapped, &replayed));
        let replayed_clamped =
            cache.layout_wrapped_line(text, px(10.), &[], Some(px(12.)), Some(1));
        assert!(Arc::ptr_eq(&clamped, &replayed_clamped));
    }

    #[test]
    fn wrapped_hit_test_handles_boundary_past_shaped_width() {
        let wrapped = WrappedLineLayout {
            unwrapped_layout: Arc::new(LineLayout {
                width: px(10.),
                runs: vec![ShapedRun {
                    font_id: FontId(0),
                    glyphs: vec![glyph_at(0., 0), glyph_at(11., 2)],
                }],
                len: "é\u{200b}".len(),
                ..LineLayout::default()
            }),
            wrap_boundaries: SmallVec::from_slice(&[WrapBoundary {
                run_ix: 0,
                glyph_ix: 1,
            }]),
            wrap_width: Some(px(11.)),
        };
        assert_eq!(
            wrapped.index_for_position(point(px(10.5), px(0.)), px(16.)),
            Err(2),
        );
    }

    #[test]
    fn layout_replay_survives_discarded_cache_capacity_reclaim() {
        let cache = LineLayoutCache::new(
            Arc::new(crate::NoopTextSystem),
            Arc::new(AtomicUsize::new(0)),
        );
        for _ in 0..2 {
            for index in 0..2500 {
                cache.layout_line(index.to_string(), px(16.), &[], None);
            }
            cache.finish_frame();
        }
        assert!(cache.current_frame.borrow().lines.capacity() > 1024);
        assert!(cache.previous_frame.borrow().lines.capacity() > 1024);

        let start = cache.layout_index();
        let retained = cache.layout_line("still painted", px(16.), &[], None);
        let end = cache.layout_index();
        cache.finish_frame();
        for _ in 0..244 {
            cache.reuse_layouts(start.clone()..end.clone());
            let layout = cache.layout_line("still painted", px(16.), &[], None);
            assert!(Arc::ptr_eq(&retained, &layout));
            assert_eq!(layout.len, "still painted".len());
            assert_eq!(layout.runs[0].glyphs.len(), "still painted".len());
            cache.finish_frame();
        }
        assert!(cache.current_frame.borrow().lines.capacity() <= 1024);
        assert!(cache.previous_frame.borrow().lines.capacity() <= 1024);
        assert!(cache.current_frame.borrow().used_lines.capacity() <= 256);
        assert!(cache.previous_frame.borrow().used_lines.capacity() <= 256);

        let fresh = cache.layout_line("after reclaim", px(16.), &[], None);
        assert_eq!(fresh.len, "after reclaim".len());
        assert!(fresh.width > px(0.));
    }

    fn glyph_at(x: f32, index: usize) -> ShapedGlyph {
        ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), px(0.)),
            index,
            is_emoji: false,
        }
    }

    fn make_layout(glyphs: Vec<ShapedGlyph>) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(100.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
            }],
            len: 0,
        }
    }

    fn glyph_x_positions(layout: &LineLayout) -> Vec<f32> {
        layout.runs[0]
            .glyphs
            .iter()
            .map(|g| f32::from(g.position.x))
            .collect()
    }

    #[test]
    fn test_force_width_latin_unchanged() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(8., 1), glyph_at(16., 2)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_marks_not_advanced() {
        let cell_width = px(8.);
        // Simulates Thai "กี" — base consonant at x=0, combining vowel also at x=0
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // ี (combining mark, same x)
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0.]);
    }

    #[test]
    fn test_force_width_base_after_combining_mark() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3), glyph_at(8., 6)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 8.]);
    }

    #[test]
    fn test_force_width_multiple_combining_marks() {
        let cell_width = px(8.);
        // Simulates "ก้" — base + vowel + tone mark (two combining marks stacked)
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // vowel (combining)
            glyph_at(0., 6), // tone mark (combining)
            glyph_at(8., 9), // next base
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 0., 8.]);
    }

    #[test]
    fn test_force_width_corrects_drifted_base_positions() {
        let cell_width = px(8.);
        // Font metrics don't perfectly match cell grid — glyphs drift >1px from cell boundary
        let mut layout = make_layout(vec![
            glyph_at(0.5, 0),  // within 1px tolerance, kept as-is
            glyph_at(10.2, 1), // >1px off from 8.0, corrected
            glyph_at(19.8, 2), // >1px off from 16.0, corrected
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_mark_after_within_tolerance_base() {
        let cell_width = px(8.);
        // Base glyph is within 1px of grid so it keeps its shaped position.
        // The combining mark must align to the base's actual position, not the grid slot.
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(0.5, 3)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 0.5]);
    }

    /// Builds a layout whose runs use their position as font id; glyphs are `(byte index, x)`.
    fn split_test_layout(width: f32, len: usize, runs: &[&[(usize, f32)]]) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(width),
            ascent: px(12.),
            descent: px(4.),
            runs: runs
                .iter()
                .enumerate()
                .map(|(font, glyphs)| ShapedRun {
                    font_id: FontId(font),
                    glyphs: glyphs
                        .iter()
                        .map(|&(index, x)| glyph_at(x, index))
                        .collect(),
                })
                .collect(),
            len,
        }
    }

    fn split_runs(layout: &LineLayout) -> Vec<(FontId, Vec<(usize, f32)>)> {
        layout
            .runs
            .iter()
            .map(|run| {
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|g| (g.index, f32::from(g.position.x)))
                    .collect();
                (run.font_id, glyphs)
            })
            .collect()
    }

    #[test]
    fn test_split_at_partitions_rtl_glyphs_by_byte_side() {
        // Right-to-left "abcd": visual order d c b a with advances 10, 15, 5, 12.
        let layout = split_test_layout(42., 4, &[&[(3, 0.), (2, 10.), (1, 25.), (0, 30.)]]);

        let (left, right) = layout.split_at(2);
        assert_eq!(split_runs(&left), vec![(FontId(0), vec![(1, 0.), (0, 5.)])]);
        assert_eq!(left.width, px(17.));
        assert_eq!(left.len, 2);
        assert_eq!(
            split_runs(&right),
            vec![(FontId(0), vec![(1, 0.), (0, 10.)])]
        );
        assert_eq!(right.width, px(25.));
        assert_eq!(right.len, 2);

        for byte_index in 0..=layout.len {
            let (left, right) = layout.split_at(byte_index);
            assert_eq!(left.width + right.width, layout.width, "split={byte_index}");
            let mut glyph_count = 0;
            for half in [&left, &right] {
                for glyph in half.runs.iter().flat_map(|run| &run.glyphs) {
                    assert!(glyph.index < half.len, "split={byte_index}");
                    assert!(glyph.position.x >= px(0.), "split={byte_index}");
                    assert!(glyph.position.x < half.width, "split={byte_index}");
                    glyph_count += 1;
                }
            }
            assert_eq!(glyph_count, 4, "split={byte_index}");
        }
    }

    #[test]
    fn test_split_at_follows_positions_when_glyphs_are_listed_right_to_left() {
        // An RTL paragraph "Xab" (X two bytes) as cosmic-text emits it: glyphs listed from
        // the right edge leftward. X sits rightmost; "ab" stays left-to-right inside it.
        // Advances by position: a 10, b 6, X 14.
        let layout = split_test_layout(30., 4, &[&[(0, 16.), (3, 10.), (2, 0.)]]);

        let (left, right) = layout.split_at(3);
        assert_eq!(
            split_runs(&left),
            vec![(FontId(0), vec![(0, 10.), (2, 0.)])]
        );
        assert_eq!(left.width, px(24.));
        assert_eq!(split_runs(&right), vec![(FontId(0), vec![(0, 0.)])]);
        assert_eq!(right.width, px(6.));

        for byte_index in [0, 2, 3, 4] {
            let (left, right) = layout.split_at(byte_index);
            assert_eq!(left.width + right.width, layout.width, "split={byte_index}");
            for half in [&left, &right] {
                assert!(half.width >= px(0.), "split={byte_index}");
                for glyph in half.runs.iter().flat_map(|run| &run.glyphs) {
                    assert!(glyph.position.x < half.width, "split={byte_index}");
                }
            }
        }
    }

    #[test]
    fn test_split_at_packs_mixed_direction_segments() {
        // "abCDEf" with right-to-left "CDE": visual order a b E D C f across two font runs,
        // with advances 8, 12, 6, 14, 7, 13. Splitting at byte 3 interleaves the halves.
        let layout = split_test_layout(
            60.,
            6,
            &[
                &[(0, 0.), (1, 8.), (4, 20.)],
                &[(3, 26.), (2, 40.), (5, 47.)],
            ],
        );

        let (left, right) = layout.split_at(3);
        assert_eq!(
            split_runs(&left),
            vec![
                (FontId(0), vec![(0, 0.), (1, 8.)]),
                (FontId(1), vec![(2, 20.)]),
            ]
        );
        assert_eq!(left.width, px(27.));
        assert_eq!(
            split_runs(&right),
            vec![
                (FontId(0), vec![(1, 0.)]),
                (FontId(1), vec![(0, 6.), (2, 20.)]),
            ]
        );
        assert_eq!(right.width, px(33.));
    }

    #[test]
    fn test_split_at_byte_ordered_glyphs_cut_at_split_x() {
        // Leading gap, a combining mark sharing byte 1 offset left of its base (so x order
        // differs from byte order), a multi-byte character at bytes 1..4, and a font run
        // boundary.
        let layout = split_test_layout(
            41.5,
            7,
            &[
                &[(0, 2.5), (1, 11.), (1, 9.25), (4, 17.75)],
                &[(5, 30.125), (6, 36.)],
            ],
        );

        for byte_index in 0..=layout.len {
            let split_x = layout.x_for_index(byte_index);
            let mut expected_left = Vec::new();
            let mut expected_right = Vec::new();
            for (font_id, glyphs) in split_runs(&layout) {
                let (before, after): (Vec<_>, Vec<_>) = glyphs
                    .into_iter()
                    .partition(|&(index, _)| index < byte_index);
                if !before.is_empty() {
                    expected_left.push((font_id, before));
                }
                if !after.is_empty() {
                    let after: Vec<_> = after
                        .into_iter()
                        .map(|(index, x)| (index - byte_index, x - f32::from(split_x)))
                        .collect();
                    expected_right.push((font_id, after));
                }
            }

            let (left, right) = layout.split_at(byte_index);
            assert_eq!(split_runs(&left), expected_left, "split={byte_index}");
            assert_eq!(left.width, split_x, "split={byte_index}");
            assert_eq!(left.len, byte_index);
            assert_eq!(split_runs(&right), expected_right, "split={byte_index}");
            assert_eq!(right.width, layout.width - split_x, "split={byte_index}");
            assert_eq!(right.len, layout.len - byte_index);
        }
    }
}
