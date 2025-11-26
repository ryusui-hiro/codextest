use std::io::{self, Write};
use std::path::PathBuf;

use clap::Parser;
use pdfvectorizer::ai::{
    AiError, REALESRGAN_X4PLUS_ONNX, SWINIR_X4_ONNX, download_model_with_progress,
};

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

    let mut progress = DownloadBar::new(filename);
    let saved = download_model_with_progress(
        url,
        &args.output_dir,
        Some(|downloaded, total| {
            progress.update(downloaded, total);
        }),
    )?;
    progress.finish();
    println!("Downloaded {} -> {}", url, saved.display());
    Ok(())
}

struct DownloadBar {
    filename: String,
}

impl DownloadBar {
    fn new(filename: impl Into<String>) -> Self {
        let name = filename.into();
        println!("Downloading {name}...");
        Self { filename: name }
    }

    fn update(&mut self, downloaded: u64, total: Option<u64>) {
        if let Some(total) = total {
            let percent = ((downloaded as f64 / total as f64) * 100.0).min(100.0);
            let width = 40;
            let filled = ((percent / 100.0) * width as f64).round() as usize;
            let bar = "#".repeat(filled) + &".".repeat(width.saturating_sub(filled));
            print!("\r[{bar}] {percent:>6.2}% ({downloaded}/{total} bytes)");
        } else {
            print!("\r{downloaded} bytes downloaded for {}", self.filename);
        }
        let _ = io::stdout().flush();
    }

    fn finish(&self) {
        println!();
    }
}
