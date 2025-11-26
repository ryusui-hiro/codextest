use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use image::DynamicImage;
use image::imageops::{FilterType, blur, unsharpen};
use pdfmodule::ai::{
    ChannelOrder, SuperResolutionEngine, dynamic_image_to_nchw_f32, nchw_f32_to_dynamic_image,
};
use visioncortex::{CompoundPath, PathSimplifyMode};
use vtracer::{ColorImage, ColorMode, Config, Hierarchical, SvgFile, conversion};

#[derive(Debug, Clone, ValueEnum)]
enum ColorChoice {
    Color,
    Binary,
}

#[derive(Debug, Clone, ValueEnum)]
enum HierarchyChoice {
    Stacked,
    Cutout,
}

#[derive(Debug, Clone, ValueEnum)]
enum ModeChoice {
    Spline,
    Polygon,
}

#[derive(Debug, Clone, ValueEnum)]
enum PrecisionChoice {
    Illustration,
    Natural,
    HighPrecision,
}

#[derive(Debug, Clone, ValueEnum)]
enum PresetChoice {
    /// イラストやマンガ線画を前提に、ノイズ除去と曲線の滑らかさを重視したプリセット
    Illustration,
    /// 写実的な写真など、色数やエッジ保持を優先する汎用プリセット
    Natural,
    /// 色の段差やエッジをできるだけ保持する高精度プリセット
    HighPrecision,
}

#[derive(Debug, Clone, ValueEnum)]
enum ChannelOrderChoice {
    Rgb,
    Bgr,
}

#[derive(Debug, Parser)]
#[command(about = "Convert a raster image into an SVG vector using vtracer.")]
struct Args {
    /// Input raster image (PNG, JPG, etc.)
    input: PathBuf,

    /// Output SVG path. Defaults to replacing the input extension with .svg
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Color handling mode
    #[arg(long, value_enum, default_value_t = ColorChoice::Color)]
    color_mode: ColorChoice,

    /// Layering strategy for overlapping paths
    #[arg(long, value_enum, default_value_t = HierarchyChoice::Stacked)]
    hierarchy: HierarchyChoice,

    /// Tracing mode (smooth spline or polygonal)
    #[arg(long, value_enum, default_value_t = ModeChoice::Spline)]
    mode: ModeChoice,

    /// Maximum number of colors to keep after quantization
    #[arg(long)]
    colors: Option<usize>,

    /// Number of bits to keep per channel during quantization (1-8)
    #[arg(long)]
    color_precision: Option<i32>,

    /// Difference threshold between quantized layers
    #[arg(long)]
    layer_difference: Option<i32>,

    /// Tuned presets for typical use-cases
    #[arg(long, value_enum, default_value_t = PresetChoice::Illustration)]
    preset: PresetChoice,

    /// Quality preset tuned for high precision output
    #[arg(long, value_enum, default_value_t = PrecisionChoice::Illustration)]
    quality: PrecisionChoice,

    /// Optional ONNX super-resolution model to run before tracing
    #[arg(long)]
    superres_model: Option<PathBuf>,

    /// Channel ordering expected by the super-resolution model
    #[arg(long, value_enum, default_value_t = ChannelOrderChoice::Bgr)]
    channel_order: ChannelOrderChoice,

    /// Downscale very large images before/after super-resolution to keep memory usage manageable
    #[arg(long, default_value_t = 4096)]
    max_dimension: u32,

    /// Remove small speckles in the traced result (0 disables filtering)
    #[arg(long)]
    filter_speckle: Option<usize>,

    /// Angle threshold (degrees) for creating corners when simplifying paths
    #[arg(long)]
    corner_threshold: Option<f64>,

    /// Minimum path segment length before simplification skips a point
    #[arg(long)]
    length_threshold: Option<f64>,

    /// Maximum number of curve fitting iterations per path
    #[arg(long)]
    max_iterations: Option<usize>,

    /// Threshold for merging neighboring paths during simplification
    #[arg(long)]
    splice_threshold: Option<f64>,

    /// Control floating point precision of generated SVG paths
    #[arg(long)]
    path_precision: Option<f64>,

    /// Apply a Gaussian blur before vectorization to tame noise
    #[arg(long)]
    denoise_radius: Option<f32>,

    /// Apply unsharp masking after denoise to restore edges
    #[arg(long)]
    unsharp_sigma: Option<f32>,

    /// Threshold for the unsharp mask (0-255)
    #[arg(long)]
    unsharp_threshold: Option<i32>,

    /// Increase contrast to help quantization separate tones
    #[arg(long)]
    enhance_contrast: Option<f32>,

    /// Smooth the SVG paths after tracing
    #[arg(long)]
    smooth_paths: Option<bool>,

    /// Corner sharpness used when smoothing SVG paths
    #[arg(long)]
    smooth_corner_threshold: Option<f64>,

    /// Outset ratio used when smoothing SVG paths
    #[arg(long)]
    smooth_outset_ratio: Option<f64>,

    /// Target segment length (in px) used when smoothing SVG paths
    #[arg(long)]
    smooth_segment_length: Option<f64>,
}

#[derive(Debug, Clone)]
struct PreprocessSettings {
    denoise_radius: Option<f32>,
    unsharp_sigma: Option<f32>,
    unsharp_threshold: Option<i32>,
    enhance_contrast: Option<f32>,
}

#[derive(Debug, Clone)]
struct SmoothingSettings {
    enabled: bool,
    corner_threshold: f64,
    outset_ratio: f64,
    segment_length: f64,
}

fn main() -> Result<(), String> {
    let args = Args::parse();

    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(&args.input));

    let mut config = Config::default();
    config.color_mode = match args.color_mode {
        ColorChoice::Color => ColorMode::Color,
        ColorChoice::Binary => ColorMode::Binary,
    };
    config.hierarchical = match args.hierarchy {
        HierarchyChoice::Stacked => Hierarchical::Stacked,
        HierarchyChoice::Cutout => Hierarchical::Cutout,
    };
    config.mode = match args.mode {
        ModeChoice::Spline => PathSimplifyMode::Spline,
        ModeChoice::Polygon => PathSimplifyMode::Polygon,
    };

    apply_preset(&mut config, &args.preset);
    apply_quality(&mut config, &args.quality);

    if let Some(colors) = args.colors {
        config.color_precision = palette_size_to_bits(colors, config.color_precision);
    }

    if let Some(color_precision) = args.color_precision {
        config.color_precision = color_precision.clamp(1, 8);
    }

    if let Some(layer_difference) = args.layer_difference {
        config.layer_difference = layer_difference.max(0);
    }

    if let Some(filter_speckle) = args.filter_speckle {
        config.filter_speckle = filter_speckle;
    }

    if let Some(corner_threshold) = args.corner_threshold {
        config.corner_threshold = corner_threshold.round() as i32;
    }

    if let Some(length_threshold) = args.length_threshold {
        config.length_threshold = length_threshold;
    }

    if let Some(max_iterations) = args.max_iterations {
        config.max_iterations = max_iterations;
    }

    if let Some(splice_threshold) = args.splice_threshold {
        config.splice_threshold = splice_threshold.round() as i32;
    }

    if let Some(path_precision) = args.path_precision {
        config.path_precision = Some(path_precision.round() as u32);
    }

    let preprocess_settings = resolve_preprocess_settings(&args);
    let smoothing_settings = resolve_smoothing_settings(&args);

    let raster = image::open(&args.input)
        .map_err(|e| format!("failed to open input image {}: {e}", args.input.display()))?;
    let prepared = resize_if_needed(&raster, args.max_dimension);

    let enhanced = if let Some(model_path) = args.superres_model.as_ref() {
        let channel_order = channel_order_choice_to_channel_order(&args.channel_order);
        let engine = SuperResolutionEngine::from_onnx(model_path).map_err(|e| {
            format!(
                "failed to load super-resolution model {}: {e}",
                model_path.display()
            )
        })?;
        let tensor = dynamic_image_to_nchw_f32(&prepared, channel_order);
        let output = engine
            .run(&tensor)
            .map_err(|e| format!("super-resolution inference failed: {e}"))?;
        let upscaled = nchw_f32_to_dynamic_image(&output, channel_order)
            .map_err(|e| format!("failed to convert inference output: {e}"))?;
        resize_if_needed(&upscaled, args.max_dimension)
    } else {
        prepared
    };

    let prefiltered = apply_pre_filters(&enhanced, &preprocess_settings);
    let color_image = dynamic_image_to_color_image(&prefiltered);
    let svg = conversion::convert(color_image, config)?;
    let svg = smooth_svg(svg, &smoothing_settings);
    fs::write(&output, svg.to_string())
        .map_err(|e| format!("failed to write SVG to {}: {e}", output.display()))?;

    println!("Saved SVG to {}", output.display());
    Ok(())
}

fn default_output(input: &Path) -> PathBuf {
    let mut candidate = input.to_path_buf();
    candidate.set_extension("svg");
    candidate
}

fn apply_preset(config: &mut Config, preset: &PresetChoice) {
    match preset {
        PresetChoice::Illustration => {
            // イラストや線画向け: 強めのノイズ除去と曲線優先の設定
            config.color_precision = 6;
            config.layer_difference = 12;
            config.filter_speckle = 4;
            config.corner_threshold = 85;
            config.length_threshold = 2.2;
            config.splice_threshold = 40;
            config.max_iterations = 12;
            config.path_precision = Some(3);
        }
        PresetChoice::Natural => {
            // 汎用向け: vtracerのデフォルトを尊重しつつ、座標の丸めを抑えてディテールを保持
            config.color_precision = 7;
            config.layer_difference = 16;
            config.filter_speckle = 3;
            config.corner_threshold = 95;
            config.length_threshold = 3.0;
            config.splice_threshold = 45;
            config.max_iterations = 12;
            config.path_precision = Some(3);
        }
        PresetChoice::HighPrecision => {
            // 高精度: 色の保持と滑らかなパス生成を最優先
            config.color_precision = 8;
            config.layer_difference = 10;
            config.filter_speckle = 2;
            config.corner_threshold = 120;
            config.length_threshold = 1.8;
            config.splice_threshold = 35;
            config.max_iterations = 16;
            config.path_precision = Some(4);
        }
    }
}

fn apply_quality(config: &mut Config, quality: &PrecisionChoice) {
    if matches!(quality, PrecisionChoice::HighPrecision) {
        config.color_precision = config.color_precision.max(8);
        config.path_precision = config.path_precision.map(|p| p.max(4)).or(Some(4));
        config.length_threshold = (config.length_threshold * 0.8).max(0.5);
    }
}

fn palette_size_to_bits(palette_size: usize, fallback: i32) -> i32 {
    if palette_size == 0 {
        return fallback;
    }
    let bits = (palette_size as f64).log2().ceil() as i32;
    bits.clamp(1, 8)
}

fn resolve_preprocess_settings(args: &Args) -> PreprocessSettings {
    let defaults = match args.quality {
        PrecisionChoice::Illustration => PreprocessSettings {
            denoise_radius: Some(0.8),
            unsharp_sigma: Some(1.0),
            unsharp_threshold: Some(2),
            enhance_contrast: Some(3.0),
        },
        PrecisionChoice::Natural => PreprocessSettings {
            denoise_radius: Some(0.5),
            unsharp_sigma: Some(0.8),
            unsharp_threshold: Some(1),
            enhance_contrast: Some(2.0),
        },
        PrecisionChoice::HighPrecision => PreprocessSettings {
            denoise_radius: Some(1.0),
            unsharp_sigma: Some(1.2),
            unsharp_threshold: Some(2),
            enhance_contrast: Some(4.0),
        },
    };

    PreprocessSettings {
        denoise_radius: args.denoise_radius.or(defaults.denoise_radius),
        unsharp_sigma: args.unsharp_sigma.or(defaults.unsharp_sigma),
        unsharp_threshold: args.unsharp_threshold.or(defaults.unsharp_threshold),
        enhance_contrast: args.enhance_contrast.or(defaults.enhance_contrast),
    }
}

fn resolve_smoothing_settings(args: &Args) -> SmoothingSettings {
    let defaults = match args.quality {
        PrecisionChoice::Illustration => SmoothingSettings {
            enabled: true,
            corner_threshold: 100.0,
            outset_ratio: 0.2,
            segment_length: 6.0,
        },
        PrecisionChoice::Natural => SmoothingSettings {
            enabled: true,
            corner_threshold: 95.0,
            outset_ratio: 0.18,
            segment_length: 7.0,
        },
        PrecisionChoice::HighPrecision => SmoothingSettings {
            enabled: true,
            corner_threshold: 110.0,
            outset_ratio: 0.16,
            segment_length: 5.5,
        },
    };

    SmoothingSettings {
        enabled: args.smooth_paths.unwrap_or(defaults.enabled),
        corner_threshold: args
            .smooth_corner_threshold
            .unwrap_or(defaults.corner_threshold),
        outset_ratio: args.smooth_outset_ratio.unwrap_or(defaults.outset_ratio),
        segment_length: args
            .smooth_segment_length
            .unwrap_or(defaults.segment_length),
    }
}

fn apply_pre_filters(image: &DynamicImage, settings: &PreprocessSettings) -> DynamicImage {
    let mut processed = image.clone();

    if let Some(radius) = settings.denoise_radius {
        if radius > 0.0 {
            processed = blur(&processed, radius);
        }
    }

    if let (Some(sigma), Some(threshold)) = (settings.unsharp_sigma, settings.unsharp_threshold) {
        if sigma > 0.0 && threshold >= 0 {
            processed = unsharpen(&processed, sigma, threshold);
        }
    }

    if let Some(contrast) = settings.enhance_contrast {
        if contrast.abs() > f32::EPSILON {
            processed = processed.adjust_contrast(contrast as f64);
        }
    }

    processed
}

fn smooth_svg(svg: SvgFile, settings: &SmoothingSettings) -> SvgFile {
    if !settings.enabled {
        return svg;
    }

    let mut output = SvgFile {
        paths: Vec::with_capacity(svg.paths.len()),
        width: svg.width,
        height: svg.height,
        path_precision: svg.path_precision,
    };

    for path in svg.paths {
        let smoothed = smooth_compound_path(
            path.path,
            settings.corner_threshold,
            settings.outset_ratio,
            settings.segment_length,
        );
        output.add_path(smoothed, path.color);
    }

    output
}

fn smooth_compound_path(
    path: CompoundPath,
    corner_threshold: f64,
    outset_ratio: f64,
    segment_length: f64,
) -> CompoundPath {
    if corner_threshold <= 0.0 || segment_length <= 0.0 {
        return path;
    }

    path.smooth(corner_threshold, outset_ratio, segment_length)
}

fn dynamic_image_to_color_image(image: &DynamicImage) -> ColorImage {
    let rgba = image.to_rgba8();
    ColorImage {
        pixels: rgba.to_vec(),
        width: rgba.width() as usize,
        height: rgba.height() as usize,
    }
}

fn resize_if_needed(image: &DynamicImage, max_dimension: u32) -> DynamicImage {
    if max_dimension == 0 {
        return image.clone();
    }

    let (width, height) = image.dimensions();
    let longest = width.max(height);
    if longest <= max_dimension {
        return image.clone();
    }

    let scale = max_dimension as f32 / longest as f32;
    let new_width = ((width as f32) * scale).round().max(1.0) as u32;
    let new_height = ((height as f32) * scale).round().max(1.0) as u32;
    image.resize(new_width, new_height, FilterType::Lanczos3)
}

fn channel_order_choice_to_channel_order(choice: &ChannelOrderChoice) -> ChannelOrder {
    match choice {
        ChannelOrderChoice::Rgb => ChannelOrder::Rgb,
        ChannelOrderChoice::Bgr => ChannelOrder::Bgr,
    }
}
