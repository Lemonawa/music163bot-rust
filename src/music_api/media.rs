fn rewrite_media_url(url: &str) -> Cow<'_, str> {
    for (from_prefix, to_prefix) in MEDIA_URL_REWRITE_RULES {
        if let Some(rest) = url.strip_prefix(from_prefix) {
            return Cow::Owned(format!("{to_prefix}{rest}"));
        }
    }
    Cow::Borrowed(url)
}

pub fn resize_album_art_to_thumbnail(image_bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| BotError::MusicApi(format!("Failed to decode image: {e}")))?;

    let resized = resize_image_with_padding(img, 320, 320);

    let mut cursor = Cursor::new(Vec::with_capacity(32 * 1024));
    resized
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .map_err(|e| BotError::MusicApi(format!("Failed to encode image: {e}")))?;

    Ok(cursor.into_inner())
}

fn deserialize_string_or_null<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}


/// Parse artists into a formatted string
#[must_use]
pub fn format_artists(artists: &[Artist]) -> String {
    let mut iter = artists.iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    // Pre-allocate: sum of name lengths + separators
    let capacity =
        artists.iter().map(|a| a.name.len()).sum::<usize>() + artists.len().saturating_sub(1);
    let mut formatted = String::with_capacity(capacity);
    formatted.push_str(&first.name);
    for artist in iter {
        formatted.push('/');
        formatted.push_str(&artist.name);
    }
    formatted
}

/// Resize image with black padding to maintain aspect ratio (like the original Go project)
fn resize_image_with_padding(
    img: DynamicImage,
    target_width: u32,
    target_height: u32,
) -> DynamicImage {
    use image::RgbImage;

    // Guard against zero dimensions to prevent division by zero and panics
    if target_width == 0 || target_height == 0 {
        return img;
    }
    let (orig_width, orig_height) = img.dimensions();
    if orig_width == 0 || orig_height == 0 {
        return DynamicImage::ImageRgb8(RgbImage::new(target_width, target_height));
    }

    let aspect_ratio = orig_width as f32 / orig_height as f32;
    let target_aspect_ratio = target_width as f32 / target_height as f32;

    // Calculate new dimensions while maintaining aspect ratio
    let (new_width, new_height) = if aspect_ratio > target_aspect_ratio {
        // Image is wider than target ratio, fit by width
        let new_width = target_width;
        let new_height = (target_width as f32 / aspect_ratio) as u32;
        (new_width, new_height)
    } else {
        // Image is taller than target ratio, fit by height
        let new_height = target_height;
        let new_width = (target_height as f32 * aspect_ratio) as u32;
        (new_width, new_height)
    };

    // Resize the image
    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);

    // Create black background canvas
    let mut canvas = RgbImage::new(target_width, target_height);

    // Calculate position to center the resized image
    let offset_x = (target_width - new_width) / 2;
    let offset_y = (target_height - new_height) / 2;

    // Overlay resized image onto canvas using imageops::overlay (avoids per-pixel loop)
    image::imageops::overlay(
        &mut canvas,
        &resized.to_rgb8(),
        i64::from(offset_x),
        i64::from(offset_y),
    );

    DynamicImage::ImageRgb8(canvas)
}
