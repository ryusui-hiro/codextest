use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use vtracer::{ColorMode, Config, Hierarchical, Mode, conversion};

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
    #[arg(long, default_value_t = 16)]
    colors: usize,

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

    let mut config = Config {
        colormode: match args.color_mode {
            ColorChoice::Color => ColorMode::Color,
            ColorChoice::Binary => ColorMode::Binary,
        },
        hierarchical: match args.hierarchy {
            HierarchyChoice::Stacked => Hierarchical::Stacked,
            HierarchyChoice::Cutout => Hierarchical::Cutout,
        },
        mode: match args.mode {
            ModeChoice::Spline => Mode::Spline,
            ModeChoice::Polygon => Mode::Polygon,
        },
        colors: args.colors,
        ..Default::default()
    };

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

    conversion::convert_image_to_svg(&args.input, &output, config)?;

    println!("Saved SVG to {}", output.display());
    Ok(())
}

fn default_output(input: &Path) -> PathBuf {
    let mut candidate = input.to_path_buf();
    candidate.set_extension("svg");
    candidate
}
