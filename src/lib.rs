use std::collections::HashMap;

use pdf::content::{Matrix, Op, Point, Rect, TextDrawAdjusted};
use pdf::error::PdfError;
use pdf::file::{CachedFile, FileOptions};
use pdf::font::{Font, ToUnicodeMap, Widths};
use pdf::object::Resolve;
use pdf::object::{ColorSpace, ImageXObject, Page, XObject};
use pdf::primitive::PdfString;
use png::{BitDepth, ColorType, Encoder};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

/// Convert a [`PdfError`] into a Python runtime error.
fn pdf_err(err: PdfError) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
}

/// Open a PDF file using the `pdf` crate with the default cached options.
fn open_pdf(path: &str) -> Result<CachedFile<Vec<u8>>, PdfError> {
    FileOptions::cached().open(path)
}

/// Lightweight representation of a text chunk extracted from a page.
#[derive(Debug)]
struct TextBlock {
    text: String,
    x: f32,
    y: f32,
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
}

impl ResolvedFont {
    fn from_font(font: &Font, resolver: &impl Resolve) -> Result<Self, PdfError> {
        let widths = font.widths(resolver)?;
        let to_unicode = match font.to_unicode(resolver) {
            Some(map) => Some(map?),
            None => None,
        };
        Ok(Self {
            widths,
            to_unicode,
            is_cid: font.is_cid(),
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
    let (x, mut y) = apply_matrix(&state.text_matrix, (0.0, 0.0));
    y += state.text_rise;
    blocks.push(TextBlock {
        text: decoded.text,
        x,
        y,
    });
    let displacement = compute_text_displacement(font, &decoded.codes, state);
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

fn encode_png(data: &[u8], width: u32, height: u32, color: ColorType) -> Result<Vec<u8>, PdfError> {
    let mut buffer = Vec::new();
    let mut encoder = Encoder::new(&mut buffer, width, height);
    encoder.set_color(color);
    encoder.set_depth(BitDepth::Eight);
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
fn extract_text_with_coords(path: &str, page_index: usize) -> PyResult<Vec<(String, f32, f32)>> {
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
    let mut state = TextState::default();
    let mut blocks = Vec::new();
    for op in operations {
        match op {
            Op::BeginText => state.begin_text(),
            Op::EndText => {}
            Op::SetTextMatrix { matrix } => state.set_text_matrix(matrix),
            Op::MoveTextPosition { translation } => {
                state.translate_line(translation.x, translation.y)
            }
            Op::TextNewline => state.newline(),
            Op::TextFont { name, size } => state.set_font(name.as_str(), size),
            Op::CharSpacing { char_space } => state.set_char_spacing(char_space),
            Op::WordSpacing { word_space } => state.set_word_spacing(word_space),
            Op::TextScaling { horiz_scale } => state.set_horizontal_scale(horiz_scale),
            Op::Leading { leading } => state.set_leading(leading),
            Op::TextRise { rise } => state.set_text_rise(rise),
            Op::TextDraw { text } => handle_text_draw(&mut state, &fonts, &text, &mut blocks),
            Op::TextDrawAdjusted { array } => {
                handle_text_adjusted(&mut state, &fonts, &array, &mut blocks)
            }
            _ => {}
        }
    }
    Ok(blocks.into_iter().map(|b| (b.text, b.x, b.y)).collect())
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
    let mut ctm = Matrix::default();
    let mut stack: Vec<Matrix> = Vec::new();
    let mut images: Vec<PositionedImage> = Vec::new();
    let mut inline_index = 0usize;
    for op in operations {
        match op {
            Op::Save => stack.push(ctm),
            Op::Restore => ctm = stack.pop().unwrap_or_default(),
            Op::Transform { matrix } => ctm = concat_matrix(&ctm, &matrix),
            Op::XObject { name } => {
                if let Some(res) = resources {
                    if let Some(xobject_ref) = res.xobjects.get(&name) {
                        let xobject = resolver.get(*xobject_ref).map_err(pdf_err)?;
                        if let XObject::Image(image) = &*xobject {
                            match extract_image(image, &resolver) {
                                Ok(image_data) => {
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
                                Err(err) => return Err(pdf_err(err)),
                            }
                        }
                    }
                }
            }
            Op::InlineImage { image } => match extract_image(&image, &resolver) {
                Ok(image_data) => {
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
                Err(err) => return Err(pdf_err(err)),
            },
            _ => {}
        }
    }
    let mut output = Vec::with_capacity(images.len());
    for positioned in images {
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
        dict.set_item("name", name)?;
        dict.set_item("x0", x0 as f64)?;
        dict.set_item("y0", y0 as f64)?;
        dict.set_item("x1", x1 as f64)?;
        dict.set_item("y1", y1 as f64)?;
        dict.set_item("width", width)?;
        dict.set_item("height", height)?;
        dict.set_item("format", format)?;
        dict.set_item("data", PyBytes::new(py, &data))?;
        output.push(dict.into());
    }
    Ok(output)
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

#[pymodule]
fn pdfmodule(_py: Python, m: &Bound<PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(get_page_count, m)?)?;
    m.add_function(wrap_pyfunction!(extract_text_with_coords, m)?)?;
    m.add_function(wrap_pyfunction!(extract_images, m)?)?;
    m.add_function(wrap_pyfunction!(extract_paths, m)?)?;
    Ok(())
}
