use std::io::Cursor;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{ImageFormat, ImageReader, Limits};

const MAX_LOGO_BYTES: usize = 1024 * 1024;
const MAX_LOGO_DIMENSION: u32 = 1024;
const MAX_LOGO_ALLOCATION: u64 = 16 * 1024 * 1024;
const MAX_NORMALIZED_LOGO_BYTES: usize = 5 * 1024 * 1024;
const MAX_ENCODED_LOGO_BYTES: usize = MAX_LOGO_BYTES.div_ceil(3) * 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrandingError {
    InvalidName,
    InvalidLogo,
}

pub(crate) fn validate_display_name(name: &str) -> Result<String, BrandingError> {
    let name = name.trim();
    let chars = name.chars().count();
    if !(1..=80).contains(&chars) || name.chars().any(char::is_control) {
        return Err(BrandingError::InvalidName);
    }
    Ok(name.to_owned())
}

pub(crate) fn normalize_logo_data_uri(data: &str) -> Result<String, BrandingError> {
    let (prefix, encoded) = data.split_once(',').ok_or(BrandingError::InvalidLogo)?;
    let declared = match prefix {
        "data:image/png;base64" => ImageFormat::Png,
        "data:image/jpeg;base64" => ImageFormat::Jpeg,
        "data:image/webp;base64" => ImageFormat::WebP,
        _ => return Err(BrandingError::InvalidLogo),
    };
    if encoded.len() > MAX_ENCODED_LOGO_BYTES + 4 {
        return Err(BrandingError::InvalidLogo);
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| BrandingError::InvalidLogo)?;
    if bytes.len() > MAX_LOGO_BYTES || image::guess_format(&bytes).ok() != Some(declared) {
        return Err(BrandingError::InvalidLogo);
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), declared);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_LOGO_DIMENSION);
    limits.max_image_height = Some(MAX_LOGO_DIMENSION);
    limits.max_alloc = Some(MAX_LOGO_ALLOCATION);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| BrandingError::InvalidLogo)?;
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|_| BrandingError::InvalidLogo)?;
    if png.len() > MAX_NORMALIZED_LOGO_BYTES {
        return Err(BrandingError::InvalidLogo);
    }
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}
