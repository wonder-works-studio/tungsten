use std::collections::HashMap;

use anyhow::{Context, Result};
use image::RgbaImage;

use crate::core::assets::img::compress::CompressOptions;
use crate::core::assets::img::pack;
use crate::core::postsync::codegen::{parse_dpi_suffix, strip_dpi_suffix};
use crate::core::postsync::lockfile::hash_parts;

/// Maps a base asset name to its DPI variants: `(scale_factor, image)` pairs.
pub type DpiGroups = HashMap<String, Vec<(u8, pack::InputImage)>>;

/// Part of every image cache key. Bump it only for a processing change that
/// should re-upload every image already in the lockfile.
const IMAGE_KEY_VERSION: &[u8] = b"tungsten-image-v1;";

/// The settings that shape an image's processed bytes.
fn processing_fingerprint(bleed: bool, compress: Option<&CompressOptions>) -> String {
    let compress = match compress {
        Some(c) => format!(
            "j{},p{},m{}",
            c.jpeg_quality, c.png_quality, c.keep_metadata
        ),
        None => "none".to_string(),
    };
    format!("bleed={};compress={};", bleed, compress)
}

/// Lockfile key for an image uploaded on its own: its source bytes plus the
/// settings that process it. The processed bytes themselves are not part of
/// it, since PNG encoding and compression come out differently per machine.
pub fn image_key(source_hash: &str, bleed: bool, compress: Option<&CompressOptions>) -> String {
    hash_parts(&[
        IMAGE_KEY_VERSION,
        b"single;",
        processing_fingerprint(bleed, compress).as_bytes(),
        source_hash.as_bytes(),
    ])
}

/// Lockfile key for a packed sheet: the settings plus every sprite's name,
/// rect and source. Rects are part of it, so a cached sheet is reused only
/// when this machine lays the sheet out exactly as the upload did.
pub fn sheet_key(
    sheet: &pack::Spritesheet,
    sources: &HashMap<String, String>,
    bleed: bool,
    compress: Option<&CompressOptions>,
    uniform: Option<&pack::UniformOptions>,
) -> String {
    let mut layout = format!(
        "{}uniform={};size={}x{};",
        processing_fingerprint(bleed, compress),
        uniform
            .map(|u| format!("{:?}/{:?}", u.cell_size, u.fit))
            .unwrap_or_else(|| "none".to_string()),
        sheet.image.width(),
        sheet.image.height()
    );
    let mut sprites: Vec<&pack::PackedImage> = sheet.images.iter().collect();
    sprites.sort_by(|a, b| a.name.cmp(&b.name));
    for s in sprites {
        let source = sources.get(&s.name).map(String::as_str).unwrap_or("");
        layout.push_str(&format!(
            "\n{}\0{},{},{},{}\0{}",
            s.name, s.x, s.y, s.width, s.height, source
        ));
    }
    hash_parts(&[IMAGE_KEY_VERSION, b"sheet;", layout.as_bytes()])
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let capacity = (image.width() * image.height() * 4) as usize;
    let mut bytes: Vec<u8> = Vec::with_capacity(capacity);
    image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("Failed to encode PNG")?;
    Ok(bytes)
}

/// Group InputImages by base name, separating @Nx variants from 1x originals.
///
/// Returns:
/// - `groups`: base_name -> sorted vec of (scale, InputImage)
/// - `non_dpi`: images with no @Nx variant at any scale
pub fn group_dpi_variants(images: Vec<pack::InputImage>) -> (DpiGroups, Vec<pack::InputImage>) {
    let mut has_variants: std::collections::HashSet<String> = std::collections::HashSet::new();

    for img in &images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name);
        if parse_dpi_suffix(stem).is_some() {
            let base_stem = strip_dpi_suffix(stem);
            let prefix = if let Some(slash) = img.name.rfind('/') {
                &img.name[..=slash]
            } else {
                ""
            };
            has_variants.insert(format!("{}{}", prefix, base_stem));
        }
    }

    let mut groups: DpiGroups = HashMap::new();
    let mut non_dpi: Vec<pack::InputImage> = Vec::new();

    for img in images {
        let stem = img.name.rsplit('/').next().unwrap_or(&img.name).to_string();
        let prefix = if let Some(slash) = img.name.rfind('/') {
            img.name[..=slash].to_string()
        } else {
            String::new()
        };

        if let Some(scale) = parse_dpi_suffix(&stem) {
            let base_stem = strip_dpi_suffix(&stem);
            let base_key = format!("{}{}", prefix, base_stem);
            groups.entry(base_key).or_default().push((scale, img));
        } else {
            let base_key = format!("{}{}", prefix, stem);
            if has_variants.contains(&base_key) {
                groups.entry(base_key).or_default().push((1, img));
            } else {
                non_dpi.push(img);
            }
        }
    }

    for variants in groups.values_mut() {
        variants.sort_by_key(|(s, _)| *s);
    }

    (groups, non_dpi)
}
