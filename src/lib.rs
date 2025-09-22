use std::cmp::Ordering;
use std::collections::HashMap;

use pdf::content::{Matrix, Op, Point, Rect, TextDrawAdjusted};
use pdf::error::PdfError;
use pdf::file::{CachedFile, FileOptions};
use pdf::font::{Font, FontData, FontDescriptor, ToUnicodeMap, Widths};
use pdf::object::Resolve;
use pdf::object::{ColorSpace, ImageXObject, MaybeRef, Page, Resources, XObject};
use pdf::primitive::PdfString;
use pdfium_render::prelude::{PdfRenderConfig, Pdfium, PdfiumError};
use png::{BitDepth, ColorType, Encoder};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

/// Convert a [`PdfError`] into a Python runtime error.
fn pdf_err(err: PdfError) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
}

fn pdfium_err(err: PdfiumError) -> PyErr {
    match err {
        PdfiumError::LoadLibraryError(inner) => PyRuntimeError::new_err(format!(
            "Failed to load Pdfium library: {}. Place a compatible Pdfium shared library in the working directory or install it system-wide.",
            inner
        )),
        PdfiumError::LoadLibraryFunctionNameError(name) => PyRuntimeError::new_err(format!(
            "Failed to resolve Pdfium symbol '{}'. Ensure the Pdfium library matches the crate configuration.",
            name
        )),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Open a PDF file using the `pdf` crate with the default cached options.
fn open_pdf(path: &str) -> Result<CachedFile<Vec<u8>>, PdfError> {
    FileOptions::cached().open(path)
}

fn load_pdfium() -> Result<Pdfium, PdfiumError> {
    Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
        .or_else(|_| Pdfium::bind_to_system_library())
        .map(Pdfium::new)
}

/// Lightweight representation of a text chunk extracted from a page.
#[derive(Debug, Clone)]
struct TextBlock {
    text: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    baseline_x: f32,
    baseline_y: f32,
}

/// Maintain the current text state while iterating over PDF text operators.
#[derive(Debug)]
struct TextState {
    current_font: Option<String>,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    leading: f32,
    text_rise: f32,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            current_font: None,
            font_size: 12.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 100.0,
            leading: 0.0,
            text_rise: 0.0,
            text_matrix: Matrix::default(),
            text_line_matrix: Matrix::default(),
        }
    }
}

impl TextState {
    fn begin_text(&mut self) {
        self.text_matrix = Matrix::default();
        self.text_line_matrix = Matrix::default();
    }

    fn set_text_matrix(&mut self, matrix: Matrix) {
        self.text_matrix = matrix;
        self.text_line_matrix = matrix;
    }

    fn set_font(&mut self, name: &str, size: f32) {
        self.current_font = Some(name.to_owned());
        self.font_size = size;
    }

    fn set_char_spacing(&mut self, spacing: f32) {
        self.char_spacing = spacing;
    }

    fn set_word_spacing(&mut self, spacing: f32) {
        self.word_spacing = spacing;
    }

    fn set_horizontal_scale(&mut self, scale: f32) {
        self.horizontal_scale = scale;
    }

    fn set_leading(&mut self, leading: f32) {
        self.leading = leading;
    }

    fn set_text_rise(&mut self, rise: f32) {
        self.text_rise = rise;
    }

    fn translate_line(&mut self, tx: f32, ty: f32) {
        let translation = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        };
        self.text_line_matrix = multiply_matrix(&self.text_line_matrix, &translation);
        self.text_matrix = self.text_line_matrix;
    }

    fn newline(&mut self) {
        self.translate_line(0.0, -self.leading);
    }

    fn translate_text(&mut self, tx: f32) {
        let translation = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        };
        self.text_matrix = multiply_matrix(&self.text_matrix, &translation);
    }
}

/// Extract the available font information for a page resource.
struct ResolvedFont {
    widths: Option<Widths>,
    to_unicode: Option<ToUnicodeMap>,
    is_cid: bool,
    metrics: Option<(f32, f32)>,
}

impl ResolvedFont {
    fn from_font(font: &Font, resolver: &impl Resolve) -> Result<Self, PdfError> {
        let widths = font.widths(resolver)?;
        let to_unicode = match font.to_unicode(resolver) {
            Some(map) => Some(map?),
            None => None,
        };
        let metrics = resolve_font_metrics(font);
        Ok(Self {
            widths,
            to_unicode,
            is_cid: font.is_cid(),
            metrics,
        })
    }

    fn decode(&self, text: &PdfString) -> DecodedText {
        let bytes = text.as_bytes();
        if self.is_cid {
            decode_cid(bytes, self.to_unicode.as_ref())
        } else {
            decode_simple(bytes, self.to_unicode.as_ref())
        }
    }

    fn glyph_width(&self, code: u16) -> f32 {
        self.widths
            .as_ref()
            .map(|w| w.get(code as usize))
            .unwrap_or(1000.0)
    }

    fn metrics(&self) -> Option<(f32, f32)> {
        self.metrics
    }
}

fn descriptor_metrics(descriptor: &FontDescriptor) -> (f32, f32) {
    let ascent = descriptor.ascent.unwrap_or(descriptor.font_bbox.top);
    let descent = descriptor.descent.unwrap_or(descriptor.font_bbox.bottom);
    (ascent, descent)
}

fn resolve_font_metrics(font: &Font) -> Option<(f32, f32)> {
    match &font.data {
        FontData::Type0(type0) => type0
            .descendant_fonts
            .get(0)
            .and_then(|descendant| resolve_font_metrics(descendant)),
        FontData::Type1(info) | FontData::TrueType(info) => info
            .font_descriptor
            .as_ref()
            .map(|descriptor| descriptor_metrics(descriptor)),
        FontData::CIDFontType0(cid) | FontData::CIDFontType2(cid) => {
            Some(descriptor_metrics(&cid.font_descriptor))
        }
        _ => None,
    }
}

/// Text decoded from a PDF string along with the glyph identifiers used for width calculation.
struct DecodedText {
    text: String,
    codes: Vec<u16>,
}

fn decode_simple(bytes: &[u8], map: Option<&ToUnicodeMap>) -> DecodedText {
    let mut text = String::new();
    let mut codes = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        let code = byte as u16;
        codes.push(code);
        if let Some(map) = map {
            if let Some(value) = map.get(code) {
                text.push_str(value);
                continue;
            }
        }
        text.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
    }
    DecodedText { text, codes }
}

fn decode_cid(bytes: &[u8], map: Option<&ToUnicodeMap>) -> DecodedText {
    let mut text = String::new();
    let mut codes = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks(2) {
        if chunk.len() != 2 {
            continue;
        }
        let code = u16::from_be_bytes([chunk[0], chunk[1]]);
        codes.push(code);
        if let Some(map) = map {
            if let Some(value) = map.get(code) {
                text.push_str(value);
                continue;
            }
        }
        text.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
    }
    DecodedText { text, codes }
}

fn fallback_decode(text: &PdfString) -> DecodedText {
    DecodedText {
        text: text.to_string_lossy(),
        codes: text.as_bytes().iter().map(|&b| b as u16).collect(),
    }
}

fn compute_text_displacement(font: Option<&ResolvedFont>, codes: &[u16], state: &TextState) -> f32 {
    let mut total = 0.0;
    for &code in codes {
        let glyph_width = font.map(|f| f.glyph_width(code)).unwrap_or(1000.0);
        let mut advance = (glyph_width / 1000.0) * state.font_size;
        advance += state.char_spacing;
        if code == 32 {
            advance += state.word_spacing;
        }
        total += advance;
    }
    total * (state.horizontal_scale / 100.0)
}

const DEFAULT_ASCENT: f32 = 800.0;
const DEFAULT_DESCENT: f32 = -200.0;

fn build_text_block(
    state: &TextState,
    text: String,
    displacement: f32,
    metrics: (f32, f32),
) -> TextBlock {
    let (raw_ascent, raw_descent) = metrics;
    let ascent = (raw_ascent / 1000.0) * state.font_size;
    let descent = (raw_descent / 1000.0) * state.font_size;
    let rise = state.text_rise;
    let points = [
        (0.0, rise),
        (displacement, rise),
        (0.0, ascent + rise),
        (displacement, ascent + rise),
        (0.0, descent + rise),
        (displacement, descent + rise),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(x_offset, y_offset) in &points {
        let (x, y) = apply_matrix(&state.text_matrix, (x_offset, y_offset));
        if x < min_x {
            min_x = x;
        }
        if x > max_x {
            max_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    }
    let (baseline_x, baseline_y) = apply_matrix(&state.text_matrix, (0.0, rise));
    TextBlock {
        text,
        x0: min_x,
        y0: min_y,
        x1: max_x,
        y1: max_y,
        baseline_x,
        baseline_y,
    }
}

fn multiply_matrix(left: &Matrix, right: &Matrix) -> Matrix {
    Matrix {
        a: left.a * right.a + left.b * right.c,
        b: left.a * right.b + left.b * right.d,
        c: left.c * right.a + left.d * right.c,
        d: left.c * right.b + left.d * right.d,
        e: left.e * right.a + left.f * right.c + right.e,
        f: left.e * right.b + left.f * right.d + right.f,
    }
}

fn apply_matrix(matrix: &Matrix, point: (f32, f32)) -> (f32, f32) {
    (
        matrix.a * point.0 + matrix.c * point.1 + matrix.e,
        matrix.b * point.0 + matrix.d * point.1 + matrix.f,
    )
}

fn concat_matrix(current: &Matrix, next: &Matrix) -> Matrix {
    Matrix {
        a: current.a * next.a + current.b * next.c,
        b: current.a * next.b + current.b * next.d,
        c: current.c * next.a + current.d * next.c,
        d: current.c * next.b + current.d * next.d,
        e: current.a * next.e + current.c * next.f + current.e,
        f: current.b * next.e + current.d * next.f + current.f,
    }
}

fn unit_square_bounds(matrix: &Matrix) -> (f32, f32, f32, f32) {
    let corners = [
        apply_matrix(matrix, (0.0, 0.0)),
        apply_matrix(matrix, (1.0, 0.0)),
        apply_matrix(matrix, (0.0, 1.0)),
        apply_matrix(matrix, (1.0, 1.0)),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        if x < min_x {
            min_x = x;
        }
        if x > max_x {
            max_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    }
    (min_x, min_y, max_x, max_y)
}

fn collect_fonts(
    page: &Page,
    resolver: &impl Resolve,
) -> Result<HashMap<String, ResolvedFont>, PdfError> {
    let mut fonts = HashMap::new();
    if let Ok(resources) = page.resources() {
        for (name, font_ref) in resources.fonts.iter() {
            let resolved = ResolvedFont::from_font(font_ref, resolver)?;
            fonts.insert(name.as_str().to_owned(), resolved);
        }
    }
    Ok(fonts)
}

fn handle_text_draw(
    state: &mut TextState,
    fonts: &HashMap<String, ResolvedFont>,
    text: &PdfString,
    blocks: &mut Vec<TextBlock>,
) {
    let font_key = state.current_font.as_ref();
    let font = font_key.and_then(|name| fonts.get(name));
    let decoded = match font {
        Some(resolved) => resolved.decode(text),
        None => fallback_decode(text),
    };
    if decoded.text.is_empty() {
        return;
    }
    let displacement = compute_text_displacement(font, &decoded.codes, state);
    let metrics = font
        .and_then(|resolved| resolved.metrics())
        .unwrap_or((DEFAULT_ASCENT, DEFAULT_DESCENT));
    let block = build_text_block(state, decoded.text, displacement, metrics);
    blocks.push(block);
    if displacement != 0.0 {
        state.translate_text(displacement);
    }
}

fn handle_text_adjusted(
    state: &mut TextState,
    fonts: &HashMap<String, ResolvedFont>,
    array: &[TextDrawAdjusted],
    blocks: &mut Vec<TextBlock>,
) {
    for item in array {
        match item {
            TextDrawAdjusted::Text(text) => handle_text_draw(state, fonts, text, blocks),
            TextDrawAdjusted::Spacing(amount) => {
                let adjustment =
                    -amount / 1000.0 * state.font_size * (state.horizontal_scale / 100.0);
                if adjustment != 0.0 {
                    state.translate_text(adjustment);
                }
            }
        }
    }
}

fn collect_text_blocks(ops: &[Op], fonts: &HashMap<String, ResolvedFont>) -> Vec<TextBlock> {
    let mut state = TextState::default();
    let mut blocks = Vec::new();
    for op in ops {
        match op {
            Op::BeginText => state.begin_text(),
            Op::EndText => {}
            Op::SetTextMatrix { matrix } => state.set_text_matrix(*matrix),
            Op::MoveTextPosition { translation } => {
                state.translate_line(translation.x, translation.y)
            }
            Op::TextNewline => state.newline(),
            Op::TextFont { name, size } => state.set_font(name.as_str(), *size),
            Op::CharSpacing { char_space } => state.set_char_spacing(*char_space),
            Op::WordSpacing { word_space } => state.set_word_spacing(*word_space),
            Op::TextScaling { horiz_scale } => state.set_horizontal_scale(*horiz_scale),
            Op::Leading { leading } => state.set_leading(*leading),
            Op::TextRise { rise } => state.set_text_rise(*rise),
            Op::TextDraw { text } => handle_text_draw(&mut state, fonts, text, &mut blocks),
            Op::TextDrawAdjusted { array } => {
                handle_text_adjusted(&mut state, fonts, array, &mut blocks)
            }
            _ => {}
        }
    }
    blocks
}

type BBox = (f32, f32, f32, f32);

fn bbox_from_block(block: &TextBlock) -> BBox {
    (block.x0, block.y0, block.x1, block.y1)
}

fn bbox_union(a: BBox, b: BBox) -> BBox {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

fn axis_gap(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> f32 {
    if a_max < b_min {
        b_min - a_max
    } else if b_max < a_min {
        a_min - b_max
    } else {
        0.0
    }
}

fn bbox_close(a: BBox, b: BBox, horizontal_margin: f32, vertical_margin: f32) -> bool {
    let x_gap = axis_gap(a.0, a.2, b.0, b.2);
    let y_gap = axis_gap(a.1, a.3, b.1, b.3);
    x_gap <= horizontal_margin && y_gap <= vertical_margin
}

fn bbox_gaps(a: BBox, b: BBox) -> (f32, f32) {
    (axis_gap(a.0, a.2, b.0, b.2), axis_gap(a.1, a.3, b.1, b.3))
}

fn cmp_f32(a: f32, b: f32) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

#[derive(Clone)]
struct TextLine {
    center_y: f32,
    text: String,
}

struct GroupedTextLayout {
    bbox: BBox,
    lines: Vec<TextLine>,
}

struct FinalTextLayout {
    bbox: BBox,
    lines: Vec<String>,
    combined: String,
    is_caption: bool,
}

impl FinalTextLayout {
    fn width(&self) -> f32 {
        self.bbox.2 - self.bbox.0
    }

    fn height(&self) -> f32 {
        self.bbox.3 - self.bbox.1
    }
}

fn looks_like_caption(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let trimmed = trimmed
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(' || c == '[')
        .trim();
    if trimmed.is_empty() {
        return false;
    }
    if matches!(trimmed.chars().next(), Some('図') | Some('表')) {
        return true;
    }
    let lower = trimmed.to_lowercase();
    lower.starts_with("fig ")
        || lower.starts_with("fig.")
        || lower.starts_with("fig(")
        || lower.starts_with("figure")
        || lower.starts_with("table")
}

fn build_text_layouts(blocks: &[TextBlock]) -> Vec<FinalTextLayout> {
    let mut sorted: Vec<&TextBlock> = blocks.iter().collect();
    sorted.sort_by(|a, b| {
        let cmp_y = cmp_f32(b.y1, a.y1);
        if cmp_y == Ordering::Equal {
            cmp_f32(a.x0, b.x0)
        } else {
            cmp_y
        }
    });
    let mut groups: Vec<GroupedTextLayout> = Vec::new();
    for block in sorted {
        let bbox = bbox_from_block(block);
        let block_height = (block.y1 - block.y0).abs().max(1.0);
        let horizontal_margin = block_height * 0.8 + 4.0;
        let vertical_margin = block_height * 1.5 + 4.0;
        if let Some(group) = groups
            .iter_mut()
            .find(|group| bbox_close(group.bbox, bbox, horizontal_margin, vertical_margin))
        {
            group.bbox = bbox_union(group.bbox, bbox);
            group.lines.push(TextLine {
                center_y: (block.y0 + block.y1) / 2.0,
                text: block.text.clone(),
            });
        } else {
            groups.push(GroupedTextLayout {
                bbox,
                lines: vec![TextLine {
                    center_y: (block.y0 + block.y1) / 2.0,
                    text: block.text.clone(),
                }],
            });
        }
    }

    let mut layouts: Vec<FinalTextLayout> = groups
        .into_iter()
        .map(|mut group| {
            group.lines.sort_by(|a, b| cmp_f32(b.center_y, a.center_y));
            let lines: Vec<String> = group.lines.into_iter().map(|line| line.text).collect();
            let combined = lines.join("\n");
            let first_line = lines.first().map(|s| s.as_str()).unwrap_or("");
            let is_caption = looks_like_caption(first_line);
            FinalTextLayout {
                bbox: group.bbox,
                lines,
                combined,
                is_caption,
            }
        })
        .collect();

    layouts.sort_by(|a, b| {
        let cmp_y = cmp_f32(b.bbox.3, a.bbox.3);
        if cmp_y == Ordering::Equal {
            cmp_f32(a.bbox.0, b.bbox.0)
        } else {
            cmp_y
        }
    });
    layouts
}

fn collect_paths(ops: &[Op]) -> Vec<(String, Vec<(f32, f32)>)> {
    let mut segments = Vec::new();
    let mut current_point: Option<Point> = None;
    let mut subpath_start: Option<Point> = None;
    for op in ops {
        match op {
            Op::MoveTo { p } => {
                current_point = Some(*p);
                subpath_start = Some(*p);
            }
            Op::LineTo { p } => {
                if let Some(start) = current_point {
                    segments.push(("line".to_string(), vec![(start.x, start.y), (p.x, p.y)]));
                }
                current_point = Some(*p);
            }
            Op::CurveTo { c1, c2, p } => {
                if let Some(start) = current_point {
                    segments.push((
                        "curve".to_string(),
                        vec![(start.x, start.y), (c1.x, c1.y), (c2.x, c2.y), (p.x, p.y)],
                    ));
                }
                current_point = Some(*p);
            }
            Op::Rect { rect } => {
                let points = rect_to_points(rect);
                segments.push(("rect".to_string(), points.clone()));
                if let Some(first) = points.first() {
                    current_point = Some(Point {
                        x: first.0,
                        y: first.1,
                    });
                    subpath_start = current_point;
                }
            }
            Op::Close => {
                if let (Some(start), Some(first)) = (current_point, subpath_start) {
                    segments.push((
                        "line".to_string(),
                        vec![(start.x, start.y), (first.x, first.y)],
                    ));
                    current_point = Some(first);
                }
            }
            _ => {}
        }
    }
    segments
}

fn rect_to_points(rect: &Rect) -> Vec<(f32, f32)> {
    let Rect {
        x,
        y,
        width,
        height,
    } = *rect;
    let x2 = x + width;
    let y2 = y + height;
    vec![(x, y), (x2, y), (x2, y2), (x, y2), (x, y)]
}

struct ImageResult {
    data: Vec<u8>,
    width: u32,
    height: u32,
    format: String,
}

struct PositionedImage {
    name: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    image: ImageResult,
}

struct RegionImage {
    index: usize,
    page_index: usize,
    input_bbox: (f32, f32, f32, f32),
    bbox: (f32, f32, f32, f32),
    pixel_bounds: (u32, u32, u32, u32),
    image: ImageResult,
    dpi: f32,
    scale: f32,
}

fn collect_positioned_images(
    ops: &[Op],
    resources: Option<&MaybeRef<Resources>>,
    resolver: &impl Resolve,
) -> Result<Vec<PositionedImage>, PdfError> {
    let mut ctm = Matrix::default();
    let mut stack: Vec<Matrix> = Vec::new();
    let mut images = Vec::new();
    let mut inline_index = 0usize;
    for op in ops {
        match op {
            Op::Save => stack.push(ctm),
            Op::Restore => ctm = stack.pop().unwrap_or_default(),
            Op::Transform { matrix } => ctm = concat_matrix(&ctm, matrix),
            Op::XObject { name } => {
                if let Some(res) = resources {
                    if let Some(xobject_ref) = res.xobjects.get(name) {
                        let xobject = resolver.get(*xobject_ref)?;
                        if let XObject::Image(image) = &*xobject {
                            let image_data = extract_image(image, resolver)?;
                            let (x0, y0, x1, y1) = unit_square_bounds(&ctm);
                            images.push(PositionedImage {
                                name: name.as_str().to_owned(),
                                x0,
                                y0,
                                x1,
                                y1,
                                image: image_data,
                            });
                        }
                    }
                }
            }
            Op::InlineImage { image } => {
                let image_data = extract_image(image, resolver)?;
                inline_index += 1;
                let (x0, y0, x1, y1) = unit_square_bounds(&ctm);
                images.push(PositionedImage {
                    name: format!("inline_{}", inline_index),
                    x0,
                    y0,
                    x1,
                    y1,
                    image: image_data,
                });
            }
            _ => {}
        }
    }
    Ok(images)
}

#[derive(Clone)]
struct CaptionInfo {
    text: String,
    bbox: BBox,
}

struct ImageLayout {
    name: String,
    bbox: BBox,
    captions: Vec<CaptionInfo>,
}

struct ObjectLayout {
    bbox: BBox,
    kinds: Vec<String>,
    captions: Vec<CaptionInfo>,
}

const CAPTION_HORIZONTAL_MARGIN: f32 = 40.0;
const CAPTION_VERTICAL_MARGIN: f32 = 80.0;
const CAPTION_EPSILON: f32 = 1e-3;
const OBJECT_MERGE_MARGIN: f32 = 4.0;

fn build_image_layouts(images: &[PositionedImage]) -> Vec<ImageLayout> {
    images
        .iter()
        .map(|image| ImageLayout {
            name: image.name.clone(),
            bbox: (image.x0, image.y0, image.x1, image.y1),
            captions: Vec::new(),
        })
        .collect()
}

fn build_object_layouts(segments: &[(String, Vec<(f32, f32)>)]) -> Vec<ObjectLayout> {
    let mut layouts: Vec<ObjectLayout> = Vec::new();
    for (kind, points) in segments {
        if let Some(bbox) = points_bbox(points) {
            if let Some(layout) = layouts.iter_mut().find(|layout| {
                bbox_close(layout.bbox, bbox, OBJECT_MERGE_MARGIN, OBJECT_MERGE_MARGIN)
            }) {
                layout.bbox = bbox_union(layout.bbox, bbox);
                if !layout.kinds.iter().any(|existing| existing == kind) {
                    layout.kinds.push(kind.clone());
                }
            } else {
                layouts.push(ObjectLayout {
                    bbox,
                    kinds: vec![kind.clone()],
                    captions: Vec::new(),
                });
            }
        }
    }
    layouts
}

fn best_caption_index<T>(
    layouts: &[T],
    caption_bbox: BBox,
    mut bbox_fn: impl FnMut(&T) -> BBox,
) -> Option<usize> {
    let mut best: Option<(usize, f32, f32)> = None;
    for (idx, layout) in layouts.iter().enumerate() {
        let bbox = bbox_fn(layout);
        let (x_gap, y_gap) = bbox_gaps(bbox, caption_bbox);
        if x_gap <= CAPTION_HORIZONTAL_MARGIN && y_gap <= CAPTION_VERTICAL_MARGIN {
            match best {
                Some((_, best_y, best_x))
                    if y_gap > best_y + CAPTION_EPSILON
                        || ((y_gap - best_y).abs() <= CAPTION_EPSILON
                            && x_gap >= best_x - CAPTION_EPSILON) => {}
                _ => {
                    best = Some((idx, y_gap, x_gap));
                }
            }
        }
    }
    best.map(|(idx, _, _)| idx)
}

fn assign_captions_to_images(
    layouts: &mut [ImageLayout],
    text_layouts: &[FinalTextLayout],
    caption_indices: &[usize],
    assigned: &mut [bool],
) {
    for &caption_idx in caption_indices {
        if assigned.get(caption_idx).copied().unwrap_or(false) {
            continue;
        }
        let caption = &text_layouts[caption_idx];
        if let Some(best_idx) = best_caption_index(layouts, caption.bbox, |layout| layout.bbox) {
            let layout = &mut layouts[best_idx];
            layout.bbox = bbox_union(layout.bbox, caption.bbox);
            layout.captions.push(CaptionInfo {
                text: caption.combined.clone(),
                bbox: caption.bbox,
            });
            assigned[caption_idx] = true;
        }
    }
}

fn assign_captions_to_objects(
    layouts: &mut [ObjectLayout],
    text_layouts: &[FinalTextLayout],
    caption_indices: &[usize],
    assigned: &mut [bool],
) {
    for &caption_idx in caption_indices {
        if assigned.get(caption_idx).copied().unwrap_or(false) {
            continue;
        }
        let caption = &text_layouts[caption_idx];
        if let Some(best_idx) = best_caption_index(layouts, caption.bbox, |layout| layout.bbox) {
            let layout = &mut layouts[best_idx];
            layout.bbox = bbox_union(layout.bbox, caption.bbox);
            layout.captions.push(CaptionInfo {
                text: caption.combined.clone(),
                bbox: caption.bbox,
            });
            assigned[caption_idx] = true;
        }
    }
}

fn encode_png(data: &[u8], width: u32, height: u32, color: ColorType) -> Result<Vec<u8>, PdfError> {
    let mut buffer = Vec::new();
    let mut encoder = Encoder::new(&mut buffer, width, height);
    match color {
        ColorType::Grayscale | ColorType::Rgb | ColorType::Rgba => {
            encoder.set_color(color);
            encoder.set_depth(BitDepth::Eight);
        }
        _ => {
            return Err(PdfError::Other {
                msg: format!("unsupported PNG color type: {:?}", color),
            });
        }
    }
    let mut writer = encoder
        .write_header()
        .map_err(|e| PdfError::Other { msg: e.to_string() })?;
    writer
        .write_image_data(data)
        .map_err(|e| PdfError::Other { msg: e.to_string() })?;
    drop(writer);
    Ok(buffer)
}

fn extract_image(image: &ImageXObject, resolver: &impl Resolve) -> Result<ImageResult, PdfError> {
    let raw = image.image_data(resolver)?;
    let width = image.width;
    let height = image.height;
    let bits = image.bits_per_component.unwrap_or(8);
    match (image.color_space.as_ref(), bits) {
        (Some(ColorSpace::DeviceRGB) | None, 8) => {
            let expected = width as usize * height as usize * 3;
            if raw.len() != expected {
                return Err(PdfError::Other {
                    msg: "unexpected RGB image size".into(),
                });
            }
            let png = encode_png(raw.as_ref(), width, height, ColorType::Rgb)?;
            Ok(ImageResult {
                data: png,
                width,
                height,
                format: "png".into(),
            })
        }
        (Some(ColorSpace::DeviceGray), 8) => {
            let expected = width as usize * height as usize;
            if raw.len() != expected {
                return Err(PdfError::Other {
                    msg: "unexpected grayscale image size".into(),
                });
            }
            let png = encode_png(raw.as_ref(), width, height, ColorType::Grayscale)?;
            Ok(ImageResult {
                data: png,
                width,
                height,
                format: "png".into(),
            })
        }
        _ => {
            let (data, filter) = image.raw_image_data(resolver)?;
            let format = match filter {
                Some(pdf::enc::StreamFilter::DCTDecode(_)) => "jpeg",
                Some(pdf::enc::StreamFilter::JPXDecode) => "jpx",
                Some(pdf::enc::StreamFilter::JBIG2Decode(_)) => "jbig2",
                Some(pdf::enc::StreamFilter::CCITTFaxDecode(_)) => "fax",
                Some(pdf::enc::StreamFilter::FlateDecode(_)) => "flate",
                Some(_) => "raw",
                None => "raw",
            };
            Ok(ImageResult {
                data: data.to_vec(),
                width,
                height,
                format: format.into(),
            })
        }
    }
}

fn text_block_to_pydict(py: Python<'_>, block: TextBlock) -> PyResult<Py<PyDict>> {
    let TextBlock {
        text,
        x0,
        y0,
        x1,
        y1,
        baseline_x,
        baseline_y,
    } = block;
    let dict = PyDict::new(py);
    dict.set_item("type", "text")?;
    dict.set_item("text", text)?;
    dict.set_item("x", baseline_x as f64)?;
    dict.set_item("y", baseline_y as f64)?;
    dict.set_item("x0", x0 as f64)?;
    dict.set_item("y0", y0 as f64)?;
    dict.set_item("x1", x1 as f64)?;
    dict.set_item("y1", y1 as f64)?;
    Ok(dict.into())
}

fn text_blocks_to_pydicts(py: Python<'_>, blocks: Vec<TextBlock>) -> PyResult<Vec<Py<PyDict>>> {
    blocks
        .into_iter()
        .map(|block| text_block_to_pydict(py, block))
        .collect()
}

fn positioned_image_to_pydict(py: Python<'_>, positioned: PositionedImage) -> PyResult<Py<PyDict>> {
    let PositionedImage {
        name,
        x0,
        y0,
        x1,
        y1,
        image,
    } = positioned;
    let ImageResult {
        data,
        width,
        height,
        format,
    } = image;
    let dict = PyDict::new(py);
    dict.set_item("type", "image")?;
    dict.set_item("name", name)?;
    dict.set_item("x", x0 as f64)?;
    dict.set_item("y", y0 as f64)?;
    dict.set_item("x0", x0 as f64)?;
    dict.set_item("y0", y0 as f64)?;
    dict.set_item("x1", x1 as f64)?;
    dict.set_item("y1", y1 as f64)?;
    dict.set_item("width", width)?;
    dict.set_item("height", height)?;
    dict.set_item("format", format)?;
    dict.set_item("data", PyBytes::new(py, &data))?;
    Ok(dict.into())
}

fn positioned_images_to_pydicts(
    py: Python<'_>,
    images: Vec<PositionedImage>,
) -> PyResult<Vec<Py<PyDict>>> {
    images
        .into_iter()
        .map(|image| positioned_image_to_pydict(py, image))
        .collect()
}

fn region_image_to_pydict(py: Python<'_>, region: RegionImage) -> PyResult<Py<PyDict>> {
    let RegionImage {
        index,
        page_index,
        input_bbox,
        bbox,
        pixel_bounds,
        image,
        dpi,
        scale,
    } = region;
    let ImageResult {
        data,
        width,
        height,
        format,
    } = image;
    let dict = PyDict::new(py);
    dict.set_item("type", "region_image")?;
    dict.set_item("index", index)?;
    dict.set_item("page_index", page_index)?;
    dict.set_item("x", bbox.0 as f64)?;
    dict.set_item("y", bbox.1 as f64)?;
    set_bbox(&dict, bbox)?;
    dict.set_item("width", width)?;
    dict.set_item("height", height)?;
    dict.set_item("dpi", dpi as f64)?;
    dict.set_item("scale", scale as f64)?;
    dict.set_item("format", format)?;
    dict.set_item("data", PyBytes::new(py, &data))?;
    dict.set_item("pixel_left", pixel_bounds.0)?;
    dict.set_item("pixel_top", pixel_bounds.1)?;
    dict.set_item("pixel_right", pixel_bounds.2)?;
    dict.set_item("pixel_bottom", pixel_bounds.3)?;
    let input_tuple = PyTuple::new(
        py,
        [
            input_bbox.0 as f64,
            input_bbox.1 as f64,
            input_bbox.2 as f64,
            input_bbox.3 as f64,
        ],
    )?;
    dict.set_item("input_rect", input_tuple)?;
    Ok(dict.into())
}

fn region_images_to_pydicts(py: Python<'_>, images: Vec<RegionImage>) -> PyResult<Vec<Py<PyDict>>> {
    images
        .into_iter()
        .map(|image| region_image_to_pydict(py, image))
        .collect()
}

fn points_bbox(points: &[(f32, f32)]) -> Option<(f32, f32, f32, f32)> {
    if points.is_empty() {
        return None;
    }
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(x, y) in points {
        if x < min_x {
            min_x = x;
        }
        if x > max_x {
            max_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    }
    Some((min_x, min_y, max_x, max_y))
}

fn path_segment_to_pydict(
    py: Python<'_>,
    kind: String,
    points: Vec<(f32, f32)>,
) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("type", "path")?;
    dict.set_item("kind", kind)?;
    let coords: Vec<(f64, f64)> = points.iter().map(|&(x, y)| (x as f64, y as f64)).collect();
    dict.set_item("points", coords)?;
    if let Some((x0, y0, x1, y1)) = points_bbox(&points) {
        dict.set_item("x0", x0 as f64)?;
        dict.set_item("y0", y0 as f64)?;
        dict.set_item("x1", x1 as f64)?;
        dict.set_item("y1", y1 as f64)?;
    }
    Ok(dict.into())
}

const DEFAULT_TEXT_LAYOUT_COLOR: (f32, f32, f32) = (0.12, 0.45, 0.85);
const DEFAULT_IMAGE_LAYOUT_COLOR: (f32, f32, f32) = (0.23, 0.70, 0.35);
const DEFAULT_OBJECT_LAYOUT_COLOR: (f32, f32, f32) = (0.86, 0.33, 0.42);
const DEFAULT_CUSTOM_RECT_COLOR: (f32, f32, f32) = (0.95, 0.40, 0.05);

struct LayoutColors {
    text: (f32, f32, f32),
    image: (f32, f32, f32),
    object: (f32, f32, f32),
}

impl LayoutColors {
    fn new(
        text: Option<(f32, f32, f32)>,
        image: Option<(f32, f32, f32)>,
        object: Option<(f32, f32, f32)>,
    ) -> Self {
        Self {
            text: text.unwrap_or(DEFAULT_TEXT_LAYOUT_COLOR),
            image: image.unwrap_or(DEFAULT_IMAGE_LAYOUT_COLOR),
            object: object.unwrap_or(DEFAULT_OBJECT_LAYOUT_COLOR),
        }
    }
}

fn color_to_tuple(color: (f32, f32, f32)) -> (f64, f64, f64) {
    (color.0 as f64, color.1 as f64, color.2 as f64)
}

fn set_bbox(dict: &Bound<PyDict>, bbox: BBox) -> PyResult<()> {
    dict.set_item("x0", bbox.0 as f64)?;
    dict.set_item("y0", bbox.1 as f64)?;
    dict.set_item("x1", bbox.2 as f64)?;
    dict.set_item("y1", bbox.3 as f64)?;
    Ok(())
}

fn captions_to_pydicts(py: Python<'_>, captions: &[CaptionInfo]) -> PyResult<Vec<Py<PyDict>>> {
    let mut dicts = Vec::with_capacity(captions.len());
    for caption in captions {
        let dict = PyDict::new(py);
        dict.set_item("type", "caption")?;
        dict.set_item("text", caption.text.as_str())?;
        dict.set_item("x", caption.bbox.0 as f64)?;
        dict.set_item("y", caption.bbox.1 as f64)?;
        set_bbox(&dict, caption.bbox)?;
        dict.set_item("width", (caption.bbox.2 - caption.bbox.0) as f64)?;
        dict.set_item("height", (caption.bbox.3 - caption.bbox.1) as f64)?;
        dicts.push(dict.into());
    }
    Ok(dicts)
}

fn text_layouts_to_pydicts(
    py: Python<'_>,
    layouts: &[FinalTextLayout],
    color: (f32, f32, f32),
) -> PyResult<Vec<Py<PyDict>>> {
    let mut dicts = Vec::with_capacity(layouts.len());
    for layout in layouts {
        let dict = PyDict::new(py);
        dict.set_item("type", "text_layout")?;
        dict.set_item("x", layout.bbox.0 as f64)?;
        dict.set_item("y", layout.bbox.1 as f64)?;
        set_bbox(&dict, layout.bbox)?;
        dict.set_item("width", layout.width() as f64)?;
        dict.set_item("height", layout.height() as f64)?;
        dict.set_item("color", color_to_tuple(color))?;
        dict.set_item("text", layout.combined.as_str())?;
        let lines_list = PyList::new(py, &layout.lines)?;
        dict.set_item("lines", lines_list)?;
        dict.set_item("is_caption", layout.is_caption)?;
        dicts.push(dict.into());
    }
    Ok(dicts)
}

fn image_layouts_to_pydicts(
    py: Python<'_>,
    layouts: &[ImageLayout],
    color: (f32, f32, f32),
) -> PyResult<Vec<Py<PyDict>>> {
    let mut dicts = Vec::with_capacity(layouts.len());
    for layout in layouts {
        let dict = PyDict::new(py);
        dict.set_item("type", "image_layout")?;
        dict.set_item("name", layout.name.as_str())?;
        dict.set_item("x", layout.bbox.0 as f64)?;
        dict.set_item("y", layout.bbox.1 as f64)?;
        set_bbox(&dict, layout.bbox)?;
        dict.set_item("width", (layout.bbox.2 - layout.bbox.0) as f64)?;
        dict.set_item("height", (layout.bbox.3 - layout.bbox.1) as f64)?;
        dict.set_item("color", color_to_tuple(color))?;
        let caption_dicts = captions_to_pydicts(py, &layout.captions)?;
        let caption_list = PyList::new(py, &caption_dicts)?;
        dict.set_item("captions", caption_list)?;
        dicts.push(dict.into());
    }
    Ok(dicts)
}

fn object_layouts_to_pydicts(
    py: Python<'_>,
    layouts: &[ObjectLayout],
    color: (f32, f32, f32),
) -> PyResult<Vec<Py<PyDict>>> {
    let mut dicts = Vec::with_capacity(layouts.len());
    for layout in layouts {
        let dict = PyDict::new(py);
        dict.set_item("type", "object_layout")?;
        dict.set_item("x", layout.bbox.0 as f64)?;
        dict.set_item("y", layout.bbox.1 as f64)?;
        set_bbox(&dict, layout.bbox)?;
        dict.set_item("width", (layout.bbox.2 - layout.bbox.0) as f64)?;
        dict.set_item("height", (layout.bbox.3 - layout.bbox.1) as f64)?;
        dict.set_item("color", color_to_tuple(color))?;
        let kinds_list = PyList::new(py, &layout.kinds)?;
        dict.set_item("kinds", kinds_list)?;
        let caption_dicts = captions_to_pydicts(py, &layout.captions)?;
        let caption_list = PyList::new(py, &caption_dicts)?;
        dict.set_item("captions", caption_list)?;
        dicts.push(dict.into());
    }
    Ok(dicts)
}

#[pyfunction]
fn get_page_count(path: &str) -> PyResult<usize> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    Ok(pdf.num_pages() as usize)
}

fn get_page<'a>(
    pdf: &'a CachedFile<Vec<u8>>,
    index: usize,
) -> Result<pdf::object::PageRc, PdfError> {
    pdf.get_page(index as u32)
}

#[pyfunction]
fn extract_text_with_coords(
    py: Python<'_>,
    path: &str,
    page_index: usize,
) -> PyResult<Vec<Py<PyDict>>> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    let page = get_page(&pdf, page_index).map_err(pdf_err)?;
    let page_ref: &Page = &page;
    let resolver = pdf.resolver();
    let fonts = collect_fonts(page_ref, &resolver).map_err(pdf_err)?;
    let content = match &page_ref.contents {
        Some(content) => content,
        None => return Ok(vec![]),
    };
    let operations = content.operations(&resolver).map_err(pdf_err)?;
    let blocks = collect_text_blocks(&operations, &fonts);
    text_blocks_to_pydicts(py, blocks)
}

#[pyfunction]
fn extract_images(py: Python<'_>, path: &str, page_index: usize) -> PyResult<Vec<Py<PyDict>>> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    let page = get_page(&pdf, page_index).map_err(pdf_err)?;
    let page_ref: &Page = &page;
    let resolver = pdf.resolver();
    let content = match &page_ref.contents {
        Some(content) => content,
        None => return Ok(vec![]),
    };
    let operations = content.operations(&resolver).map_err(pdf_err)?;
    let resources = page_ref.resources().ok();
    let images = collect_positioned_images(&operations, resources, &resolver).map_err(pdf_err)?;
    positioned_images_to_pydicts(py, images)
}

#[pyfunction]
#[pyo3(signature = (path, page_index, rectangles, dpi = 144.0))]
fn extract_region_images(
    py: Python<'_>,
    path: &str,
    page_index: usize,
    rectangles: Vec<(f32, f32, f32, f32)>,
    dpi: f32,
) -> PyResult<Vec<Py<PyDict>>> {
    if rectangles.is_empty() {
        return Ok(vec![]);
    }
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err(PyRuntimeError::new_err(
            "dpi must be a positive finite value",
        ));
    }

    let pdfium = load_pdfium().map_err(pdfium_err)?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(pdfium_err)?;
    let page_index_u16 = u16::try_from(page_index).map_err(|_| {
        PyRuntimeError::new_err("page_index exceeds Pdfium limits (must be <= 65535)")
    })?;
    let page = document.pages().get(page_index_u16).map_err(pdfium_err)?;
    let page_width = page.width().value;
    let page_height = page.height().value;
    if !page_width.is_finite()
        || !page_height.is_finite()
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return Err(PyRuntimeError::new_err("page has non-positive dimensions"));
    }

    let scale_factor = dpi / 72.0;
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return Err(PyRuntimeError::new_err(
            "invalid rendering scale computed from dpi",
        ));
    }

    let render_config = PdfRenderConfig::new().scale_page_by_factor(scale_factor);
    let bitmap = page
        .render_with_config(&render_config)
        .map_err(pdfium_err)?;
    let rendered = bitmap.as_image();
    let image_width = rendered.width();
    let image_height = rendered.height();
    if image_width == 0 || image_height == 0 {
        return Err(PyRuntimeError::new_err("rendered page has zero dimensions"));
    }

    let scale_x = image_width as f32 / page_width;
    let scale_y = image_height as f32 / page_height;
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        return Err(PyRuntimeError::new_err("unable to compute rendering scale"));
    }

    let mut regions = Vec::with_capacity(rectangles.len());
    let page_height_f = page_height;
    let image_width_f = image_width as f32;
    let image_height_f = image_height as f32;
    for (index, &(x0, y0, x1, y1)) in rectangles.iter().enumerate() {
        let canonical = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        let clamped = (
            canonical.0.clamp(0.0, page_width),
            canonical.1.clamp(0.0, page_height),
            canonical.2.clamp(0.0, page_width),
            canonical.3.clamp(0.0, page_height),
        );
        if clamped.0 >= clamped.2 || clamped.1 >= clamped.3 {
            return Err(PyRuntimeError::new_err(format!(
                "rectangle at index {} does not overlap the page bounds",
                index
            )));
        }

        let left_f = (clamped.0 * scale_x).floor();
        let right_f = (clamped.2 * scale_x).ceil();
        let top_f = ((page_height_f - clamped.3) * scale_y).floor();
        let bottom_f = ((page_height_f - clamped.1) * scale_y).ceil();

        let left_px = left_f.clamp(0.0, image_width_f) as u32;
        let mut right_px = right_f.clamp(0.0, image_width_f) as u32;
        let top_px = top_f.clamp(0.0, image_height_f) as u32;
        let mut bottom_px = bottom_f.clamp(0.0, image_height_f) as u32;

        if right_px <= left_px {
            right_px = right_px.saturating_add(1).min(image_width);
        }
        if bottom_px <= top_px {
            bottom_px = bottom_px.saturating_add(1).min(image_height);
        }
        if right_px <= left_px || bottom_px <= top_px {
            return Err(PyRuntimeError::new_err(format!(
                "rectangle at index {} produced an empty region after scaling",
                index
            )));
        }

        let crop_width = right_px - left_px;
        let crop_height = bottom_px - top_px;
        let cropped = rendered.crop_imm(left_px, top_px, crop_width, crop_height);
        let rgba = cropped.to_rgba8();
        let width_px = rgba.width();
        let height_px = rgba.height();
        let raw = rgba.into_raw();
        let png_data = encode_png(&raw, width_px, height_px, ColorType::Rgba).map_err(pdf_err)?;
        let pixel_bounds = (left_px, top_px, left_px + crop_width, top_px + crop_height);
        regions.push(RegionImage {
            index,
            page_index,
            input_bbox: (x0, y0, x1, y1),
            bbox: clamped,
            pixel_bounds,
            image: ImageResult {
                data: png_data,
                width: width_px,
                height: height_px,
                format: "png".into(),
            },
            dpi,
            scale: scale_factor,
        });
    }

    region_images_to_pydicts(py, regions)
}

#[pyfunction]
fn extract_paths(path: &str, page_index: usize) -> PyResult<Vec<(String, Vec<(f32, f32)>)>> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    let page = get_page(&pdf, page_index).map_err(pdf_err)?;
    let page_ref: &Page = &page;
    let resolver = pdf.resolver();
    let content = match &page_ref.contents {
        Some(content) => content,
        None => return Ok(vec![]),
    };
    let operations = content.operations(&resolver).map_err(pdf_err)?;
    Ok(collect_paths(&operations))
}

#[pyfunction]
#[pyo3(signature = (path, page_index, text_color = None, image_color = None, object_color = None))]
fn extract_layouts(
    py: Python<'_>,
    path: &str,
    page_index: usize,
    text_color: Option<(f32, f32, f32)>,
    image_color: Option<(f32, f32, f32)>,
    object_color: Option<(f32, f32, f32)>,
) -> PyResult<Vec<Py<PyDict>>> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    let page = get_page(&pdf, page_index).map_err(pdf_err)?;
    let page_ref: &Page = &page;
    let resolver = pdf.resolver();
    let fonts = collect_fonts(page_ref, &resolver).map_err(pdf_err)?;
    let resources = page_ref.resources().ok();
    let content = match &page_ref.contents {
        Some(content) => content,
        None => return Ok(vec![]),
    };
    let operations = content.operations(&resolver).map_err(pdf_err)?;
    let text_blocks = collect_text_blocks(&operations, &fonts);
    let text_layouts = build_text_layouts(&text_blocks);
    let caption_indices: Vec<usize> = text_layouts
        .iter()
        .enumerate()
        .filter_map(|(idx, layout)| if layout.is_caption { Some(idx) } else { None })
        .collect();
    let mut caption_assigned = vec![false; text_layouts.len()];
    let images = collect_positioned_images(&operations, resources, &resolver).map_err(pdf_err)?;
    let mut image_layouts = build_image_layouts(&images);
    let path_segments = collect_paths(&operations);
    let mut object_layouts = build_object_layouts(&path_segments);
    assign_captions_to_images(
        &mut image_layouts,
        &text_layouts,
        &caption_indices,
        &mut caption_assigned,
    );
    assign_captions_to_objects(
        &mut object_layouts,
        &text_layouts,
        &caption_indices,
        &mut caption_assigned,
    );
    let colors = LayoutColors::new(text_color, image_color, object_color);

    let mut layouts = text_layouts_to_pydicts(py, &text_layouts, colors.text)?;
    layouts.extend(image_layouts_to_pydicts(py, &image_layouts, colors.image)?);
    layouts.extend(object_layouts_to_pydicts(
        py,
        &object_layouts,
        colors.object,
    )?);
    Ok(layouts)
}

#[pyfunction]
fn extract_page_content(py: Python<'_>, path: &str, page_index: usize) -> PyResult<Py<PyDict>> {
    let pdf = open_pdf(path).map_err(pdf_err)?;
    let page = get_page(&pdf, page_index).map_err(pdf_err)?;
    let page_ref: &Page = &page;
    let resolver = pdf.resolver();
    let fonts = collect_fonts(page_ref, &resolver).map_err(pdf_err)?;
    let resources = page_ref.resources().ok();
    let content = match &page_ref.contents {
        Some(content) => content,
        None => {
            let page_dict = PyDict::new(py);
            page_dict.set_item("page_index", page_index)?;
            page_dict.set_item("text", PyList::empty(py))?;
            page_dict.set_item("images", PyList::empty(py))?;
            page_dict.set_item("objects", PyList::empty(py))?;
            page_dict.set_item("layouts", PyList::empty(py))?;
            page_dict.set_item("items", PyList::empty(py))?;
            return Ok(page_dict.into());
        }
    };
    let operations = content.operations(&resolver).map_err(pdf_err)?;
    let text_blocks = collect_text_blocks(&operations, &fonts);
    let text_layouts = build_text_layouts(&text_blocks);
    let caption_indices: Vec<usize> = text_layouts
        .iter()
        .enumerate()
        .filter_map(|(idx, layout)| if layout.is_caption { Some(idx) } else { None })
        .collect();
    let mut caption_assigned = vec![false; text_layouts.len()];
    let images = collect_positioned_images(&operations, resources, &resolver).map_err(pdf_err)?;
    let mut image_layouts = build_image_layouts(&images);
    let path_segments = collect_paths(&operations);
    let mut object_layouts = build_object_layouts(&path_segments);
    assign_captions_to_images(
        &mut image_layouts,
        &text_layouts,
        &caption_indices,
        &mut caption_assigned,
    );
    assign_captions_to_objects(
        &mut object_layouts,
        &text_layouts,
        &caption_indices,
        &mut caption_assigned,
    );

    let text_entries = text_blocks_to_pydicts(py, text_blocks.clone())?;
    let image_entries = positioned_images_to_pydicts(py, images)?;
    let mut object_entries = Vec::with_capacity(path_segments.len());
    for (kind, points) in &path_segments {
        object_entries.push(path_segment_to_pydict(py, kind.clone(), points.clone())?);
    }

    let colors = LayoutColors::new(None, None, None);
    let mut layout_entries = text_layouts_to_pydicts(py, &text_layouts, colors.text)?;
    layout_entries.extend(image_layouts_to_pydicts(py, &image_layouts, colors.image)?);
    layout_entries.extend(object_layouts_to_pydicts(
        py,
        &object_layouts,
        colors.object,
    )?);

    let text_list = PyList::new(py, &text_entries)?;
    let image_list = PyList::new(py, &image_entries)?;
    let object_list = PyList::new(py, &object_entries)?;
    let layout_list = PyList::new(py, &layout_entries)?;

    let mut all_items: Vec<Py<PyAny>> = Vec::new();
    for entry in &text_entries {
        all_items.push(entry.clone_ref(py).into());
    }
    for entry in &image_entries {
        all_items.push(entry.clone_ref(py).into());
    }
    for entry in &object_entries {
        all_items.push(entry.clone_ref(py).into());
    }
    let items_list = PyList::new(py, &all_items)?;

    let page_dict = PyDict::new(py);
    page_dict.set_item("page_index", page_index)?;
    page_dict.set_item("text", text_list)?;
    page_dict.set_item("images", image_list)?;
    page_dict.set_item("objects", object_list)?;
    page_dict.set_item("layouts", layout_list)?;
    page_dict.set_item("items", items_list)?;
    Ok(page_dict.into())
}

#[pyfunction]
#[pyo3(signature = (x0, y0, x1, y1, color = None))]
fn make_rectangle_outline(
    py: Python<'_>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: Option<(f32, f32, f32)>,
) -> PyResult<Py<PyDict>> {
    let bbox = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
    let color = color.unwrap_or(DEFAULT_CUSTOM_RECT_COLOR);
    let dict = PyDict::new(py);
    dict.set_item("type", "rectangle_outline")?;
    dict.set_item("x", bbox.0 as f64)?;
    dict.set_item("y", bbox.1 as f64)?;
    set_bbox(&dict, bbox)?;
    dict.set_item("width", (bbox.2 - bbox.0) as f64)?;
    dict.set_item("height", (bbox.3 - bbox.1) as f64)?;
    dict.set_item("color", color_to_tuple(color))?;
    Ok(dict.into())
}

#[pymodule]
fn pdfmodule(_py: Python, m: &Bound<PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(get_page_count, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_coords, m)?)?;
    m.add_function(wrap_pyfunction!(extract_images, m)?)?;
    m.add_function(wrap_pyfunction!(extract_region_images, m)?)?;
    m.add_function(wrap_pyfunction!(extract_paths, m)?)?;
    m.add_function(wrap_pyfunction!(extract_layouts, m)?)?;
    m.add_function(wrap_pyfunction!(extract_page_content, m)?)?;
    m.add_function(wrap_pyfunction!(make_rectangle_outline, m)?)?;
    Ok(())
}
