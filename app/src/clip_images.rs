use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder, RgbaImage};
use ml_storage::ClipImageRow;
use serde::Serialize;
use sha2::{Digest, Sha256};

const MAX_STORED_DIM: u32 = 1600;
const THUMB_DIM: u32 = 220;
const DATA_JPEG_QUALITY: u8 = 78;
const THUMB_JPEG_QUALITY: u8 = 55;

pub struct CompressedClipImage {
    pub width: u32,
    pub height: u32,
    pub mime: &'static str,
    pub thumb: Vec<u8>,
    pub data: Vec<u8>,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipImageItem {
    pub id: i64,
    pub ts: i64,
    pub app: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub thumb_url: String,
}

impl From<ClipImageRow> for ClipImageItem {
    fn from(r: ClipImageRow) -> Self {
        let thumb_url = format!("data:{};base64,{}", r.mime, B64.encode(&r.thumb));
        Self {
            id: r.id,
            ts: r.ts,
            app: r.app,
            width: r.width,
            height: r.height,
            size_bytes: r.size_bytes,
            thumb_url,
        }
    }
}

pub fn compress_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<CompressedClipImage, String> {
    let buf = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "invalid clipboard image dimensions".to_string())?;
    let mut img = DynamicImage::ImageRgba8(buf);
    if img.width() > MAX_STORED_DIM || img.height() > MAX_STORED_DIM {
        img = img.resize(MAX_STORED_DIM, MAX_STORED_DIM, FilterType::Triangle);
    }
    let out_w = img.width();
    let out_h = img.height();

    let rgb = img.to_rgb8();
    let mut data = Vec::with_capacity(64 * 1024);
    JpegEncoder::new_with_quality(&mut data, DATA_JPEG_QUALITY)
        .write_image(rgb.as_raw(), out_w, out_h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("jpeg encode error: {e}"))?;

    let thumb_img = img.resize(THUMB_DIM, THUMB_DIM, FilterType::Triangle).to_rgb8();
    let mut thumb = Vec::with_capacity(4 * 1024);
    JpegEncoder::new_with_quality(&mut thumb, THUMB_JPEG_QUALITY)
        .write_image(
            thumb_img.as_raw(),
            thumb_img.width(),
            thumb_img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("thumb encode error: {e}"))?;

    let sha256: [u8; 32] = Sha256::digest(&data).into();
    Ok(CompressedClipImage {
        width: out_w,
        height: out_h,
        mime: "image/jpeg",
        thumb,
        data,
        sha256,
    })
}

pub fn to_data_url(mime: &str, data: &[u8]) -> String {
    format!("data:{mime};base64,{}", B64.encode(data))
}

pub fn decode_to_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::load_from_memory(data).map_err(|e| format!("decode error: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}
