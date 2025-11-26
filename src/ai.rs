use std::fs;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use ndarray::Array4;
use ort::{ExecutionProvider, GraphOptimizationLevel, session::Session, value::Value};
use reqwest::blocking::Client;
use thiserror::Error;

/// Primary error type for AI-related helpers.
#[derive(Debug, Error)]
pub enum AiError {
    #[error("ORT error: {0}")]
    Ort(#[from] ort::OrtError),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected tensor shape: {0:?}")]
    InvalidTensorShape(Vec<usize>),
    #[error("model run did not produce any outputs")]
    MissingOutput,
    #[error("failed to derive filename from URL {0}")]
    MissingFilename(String),
}

/// Channel ordering used when converting between images and tensors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOrder {
    /// Standard red-green-blue layout.
    Rgb,
    /// Blue-green-red layout commonly used by computer vision models such as Real-ESRGAN.
    Bgr,
}

fn channel_indices(order: ChannelOrder) -> (usize, usize, usize) {
    match order {
        ChannelOrder::Rgb => (0, 1, 2),
        ChannelOrder::Bgr => (2, 1, 0),
    }
}

/// Convert an [`image::DynamicImage`] into an `ndarray::Array4<f32>` tensor (NCHW layout).
///
/// Values are normalized to the `[0.0, 1.0]` range to match typical ONNX model expectations.
pub fn dynamic_image_to_nchw_f32(image: &DynamicImage, order: ChannelOrder) -> Array4<f32> {
    let rgb = image.to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut tensor = Array4::<f32>::zeros((1, 3, height as usize, width as usize));
    let (r_idx, g_idx, b_idx) = channel_indices(order);

    for (x, y, pixel) in rgb.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        tensor[(0, r_idx, y, x)] = pixel[0] as f32 / 255.0;
        tensor[(0, g_idx, y, x)] = pixel[1] as f32 / 255.0;
        tensor[(0, b_idx, y, x)] = pixel[2] as f32 / 255.0;
    }

    tensor
}

fn clamp_channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Convert an `ndarray::Array4<f32>` tensor in NCHW layout back into a [`image::DynamicImage`].
pub fn nchw_f32_to_dynamic_image(
    tensor: &Array4<f32>,
    order: ChannelOrder,
) -> Result<DynamicImage, AiError> {
    let shape = tensor.shape();
    if shape.len() != 4 || shape[0] != 1 || shape[1] != 3 {
        return Err(AiError::InvalidTensorShape(shape.to_vec()));
    }

    let height = shape[2];
    let width = shape[3];
    let mut buffer: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width as u32, height as u32);
    let (r_idx, g_idx, b_idx) = channel_indices(order);

    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        let (x, y) = (x as usize, y as usize);
        *pixel = Rgb([
            clamp_channel(tensor[(0, r_idx, y, x)]),
            clamp_channel(tensor[(0, g_idx, y, x)]),
            clamp_channel(tensor[(0, b_idx, y, x)]),
        ]);
    }

    Ok(DynamicImage::ImageRgb8(buffer))
}

/// Light wrapper around an ORT session for running Real-ESRGAN/SwinIR style super-resolution models.
pub struct SuperResolutionEngine {
    session: Session,
}

impl SuperResolutionEngine {
    /// Create a new engine from an ONNX file on disk.
    pub fn from_onnx(model_path: impl AsRef<Path>) -> Result<Self, AiError> {
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers([ExecutionProvider::CPU(Default::default())])?
            .with_model_from_file(model_path)?;

        Ok(Self { session })
    }

    /// Run inference on an input tensor.
    pub fn run(&self, input: &Array4<f32>) -> Result<Array4<f32>, AiError> {
        let allocator = self.session.allocator();
        let input_value = Value::from_array(allocator, input)?;
        let outputs = self.session.run(vec![input_value])?;
        let first_output = outputs.into_iter().next().ok_or(AiError::MissingOutput)?;
        let tensor = first_output.try_extract::<f32>()?;
        Ok(tensor.view().to_owned())
    }
}

/// Default ONNX download URLs for convenient setup.
pub const REALESRGAN_X4PLUS_ONNX: &str =
    "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-x4plus.onnx";
pub const SWINIR_X4_ONNX: &str =
    "https://github.com/JingyunLiang/SwinIR/releases/download/v0.0/swinir_x4.onnx";

/// Download an ONNX model to the specified directory.
pub fn download_model(url: &str, output_dir: impl AsRef<Path>) -> Result<PathBuf, AiError> {
    download_model_with_progress(url, output_dir, None::<fn(u64, Option<u64>)>)
}

/// Download an ONNX model and report incremental progress through the provided callback.
pub fn download_model_with_progress<F>(
    url: &str,
    output_dir: impl AsRef<Path>,
    mut progress: Option<F>,
) -> Result<PathBuf, AiError>
where
    F: FnMut(u64, Option<u64>),
{
    let dir = output_dir.as_ref();
    fs::create_dir_all(dir)?;
    let filename = url
        .split('/')
        .last()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AiError::MissingFilename(url.to_owned()))?;
    let output_path = dir.join(filename);

    let client = Client::builder().gzip(true).brotli(true).build()?;
    let mut response = client.get(url).send()?.error_for_status()?;
    let total_size = response.content_length();
    let mut file = fs::File::create(&output_path)?;

    let mut downloaded = 0u64;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        file.write_all(&buffer[..read])?;
        downloaded += read as u64;

        if let Some(ref mut cb) = progress {
            cb(downloaded, total_size);
        }
    }

    Ok(output_path)
}
