use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use image::DynamicImage;
use image::imageops::FilterType;
use pdfmodule::ai::{
    ChannelOrder, SuperResolutionEngine, dynamic_image_to_nchw_f32, nchw_f32_to_dynamic_image,
};
use vtracer::{ColorImage, ColorMode, Config, Hierarchical, Mode, conversion};

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
enum PresetChoice {
    /// イラストやマンガ線画を前提に、ノイズ除去と曲線の滑らかさを重視したプリセット
    Illustration,
    /// 写実的な写真など、色数やエッジ保持を優先する汎用プリセット
    Natural,
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

    /// Tuned presets for typical use-cases
    #[arg(long, value_enum, default_value_t = PresetChoice::Illustration)]
    preset: PresetChoice,

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

    /// Whether to round SVG coordinates to integers
    #[arg(long)]
    round_coords: Option<bool>,

    /// Toggle path optimization (smoothing and simplification)
    #[arg(long)]
    optimize_paths: Option<bool>,
}

fn main() -> Result<(), String> {
    let args = Args::parse();

    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(&args.input));

    let mut config = Config::default();
    config.colormode = match args.color_mode {
        ColorChoice::Color => ColorMode::Color,
        ColorChoice::Binary => ColorMode::Binary,
    };
    config.hierarchical = match args.hierarchy {
        HierarchyChoice::Stacked => Hierarchical::Stacked,
        HierarchyChoice::Cutout => Hierarchical::Cutout,
    };
    config.mode = match args.mode {
        ModeChoice::Spline => Mode::Spline,
        ModeChoice::Polygon => Mode::Polygon,
    };

    apply_preset(&mut config, &args.preset);

    if let Some(colors) = args.colors {
        config.colors = colors;
    }

    if let Some(filter_speckle) = args.filter_speckle {
        config.filter_speckle = filter_speckle;
    }

    if let Some(corner_threshold) = args.corner_threshold {
        config.corner_threshold = corner_threshold;
    }

    if let Some(length_threshold) = args.length_threshold {
        config.length_threshold = length_threshold;
    }

    if let Some(max_iterations) = args.max_iterations {
        config.max_iterations = max_iterations;
    }

    if let Some(splice_threshold) = args.splice_threshold {
        config.splice_threshold = splice_threshold;
    }

    if let Some(path_precision) = args.path_precision {
        config.path_precision = path_precision;
    }

    if let Some(round_coords) = args.round_coords {
        config.round_coords = round_coords;
    }

    if let Some(optimize_paths) = args.optimize_paths {
        config.optimize_paths = optimize_paths;
    }

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

    let color_image = dynamic_image_to_color_image(&enhanced);
    let svg = conversion::convert(color_image, config)?;
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
            // イラストや線画向け: 少ない色数・強めのノイズ除去・曲線優先の設定
            config.colors = config.colors.min(12);
            config.filter_speckle = 4;
            config.corner_threshold = 85.0;
            config.length_threshold = 2.0;
            config.splice_threshold = 0.6;
            config.max_iterations = 12;
            config.path_precision = 2.0;
            config.round_coords = true;
            config.optimize_paths = true;
        }
        PresetChoice::Natural => {
            // 汎用向け: vtracerのデフォルトを尊重しつつ、座標の丸めを抑えてディテールを保持
            config.colors = config.colors.max(16);
            config.round_coords = false;
            config.optimize_paths = true;
        }
    }
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
