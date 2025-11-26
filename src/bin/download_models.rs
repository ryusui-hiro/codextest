use std::path::PathBuf;

use clap::Parser;
use pdfmodule::ai::{AiError, REALESRGAN_X4PLUS_ONNX, SWINIR_X4_ONNX, download_model};

#[derive(Debug, Parser)]
#[command(about = "Download Real-ESRGAN and SwinIR ONNX models for offline use.")]
struct Args {
    /// Directory to save ONNX model files.
    #[arg(short, long, default_value = "models")]
    output_dir: PathBuf,

    /// Skip downloading files that already exist in the output directory.
    #[arg(long, default_value_t = true)]
    skip_existing: bool,
}

fn main() -> Result<(), AiError> {
    let args = Args::parse();
    download_if_needed(REALESRGAN_X4PLUS_ONNX, &args)?;
    download_if_needed(SWINIR_X4_ONNX, &args)?;
    Ok(())
}

fn download_if_needed(url: &str, args: &Args) -> Result<(), AiError> {
    let filename = url
        .split('/')
        .last()
        .expect("URL should contain a filename");
    let target_path = args.output_dir.join(filename);

    if args.skip_existing && target_path.exists() {
        println!("Skipping existing model: {}", target_path.display());
        return Ok(());
    }

    let saved = download_model(url, &args.output_dir)?;
    println!("Downloaded {} -> {}", url, saved.display());
    Ok(())
}
