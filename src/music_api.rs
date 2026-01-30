use crate::error::{BotError, Result};
use aes::Aes128;
use cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyInit};
use ecb::{Decryptor, Encryptor};
use hex::encode_upper;
use image::{DynamicImage, GenericImageView, ImageFormat};
use md5::compute as md5_compute;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct MusicApi {
    client: Client,
    pub music_u: Option<String>,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongDetailResponse {
    pub code: i32,
    pub songs: Vec<SongDetail>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongDetail {
    pub id: u64,
    pub name: String,
    #[serde(alias = "duration")]
    pub dt: Option<u64>, // Duration in milliseconds (may be missing)
    #[serde(alias = "artists")]
    pub ar: Option<Vec<Artist>>, // Artists array (may be missing)
    #[serde(alias = "album")]
    pub al: Option<Album>, // Album info (may be missing)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Artist {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Album {
    pub id: u64,
    pub name: String,
    #[serde(rename = "picUrl")]
    pub pic_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongUrlResponse {
    pub code: i32,
    pub data: Vec<SongUrl>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongUrl {
    pub id: u64,
    pub url: String,
    pub br: u64,
    pub size: u64,
    pub md5: String,
    #[serde(rename = "type")]
    pub format: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricResponse {
    pub code: i32,
    pub lrc: Option<LyricContent>,
    pub tlyric: Option<LyricContent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricContent {
    pub lyric: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub code: i32,
    pub result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct EapiSearchResponse {
    pub code: i32,
    pub result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub songs: Vec<SearchSong>,
    #[serde(rename = "songCount")]
    pub song_count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchSong {
    pub id: u64,
    pub name: String,
    pub artists: Vec<Artist>,
    pub album: Album,
    pub duration: u64,
}

impl MusicApi {
    #[must_use]
    pub fn new(music_u: Option<String>, base_url: String) -> Self {
        let mut client_builder = Client::builder();

        // Use rustls TLS for better compatibility
        client_builder = client_builder.use_rustls_tls();

        // Performance optimizations
        // pool_max_idle_per_host(0) prevents connection pool memory accumulation
        client_builder = client_builder
            .tcp_nodelay(true)
            .pool_max_idle_per_host(0)
            .connect_timeout(std::time::Duration::from_secs(10));

        // Add user agent
        client_builder = client_builder
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36");

        let client = client_builder.build().unwrap();

        Self {
            client,
            music_u,
            base_url,
        }
    }

    fn build_eapi_cookie(&self) -> String {
        let device_id = Uuid::new_v4().simple().to_string();
        let appver = "9.3.40";
        let buildver = SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
            |_| "0".to_string(),
            |duration| duration.as_secs().to_string(),
        );
        let mut cookie_parts = vec![
            format!("deviceId={}", device_id),
            format!("appver={}", appver),
            format!("buildver={}", &buildver[..buildver.len().min(10)]),
            "resolution=1920x1080".to_string(),
            "os=Android".to_string(),
        ];

        if let Some(music_u) = &self.music_u {
            cookie_parts.push(format!("MUSIC_U={music_u}"));
        } else {
            cookie_parts.push("MUSIC_A=4ee5f776c9ed1e4d5f031b09e084c6cb333e43ee4a841afeebbef9bbf4b7e4152b51ff20ecb9e8ee9e89ab23044cf50d1609e4781e805e73a138419e5583bc7fd1e5933c52368d9127ba9ce4e2f233bf5a77ba40ea6045ae1fc612ead95d7b0e0edf70a74334194e1a190979f5fc12e9968c3666a981495b33a649814e309366".to_string());
        }

        cookie_parts.join("; ")
    }

    fn eapi_splice(path: &str, json: &str) -> String {
        let marker = "36cd479b6b5";
        let text = format!("nobody{path}use{json}md5forencrypt");
        let digest = format!("{:x}", md5_compute(text.as_bytes()));
        format!("{path}-{marker}-{json}-{marker}-{digest}")
    }

    fn eapi_encrypt(data: &str) -> String {
        let block_size = 16;
        let data_len = data.len();
        let padded_len = ((data_len + block_size) / block_size) * block_size;
        let mut buf = vec![0u8; padded_len];
        buf[..data_len].copy_from_slice(data.as_bytes());
        let encrypted = Encryptor::<Aes128>::new_from_slice(b"e82ckenh8dichen8")
            .expect("eapi key length")
            .encrypt_padded_mut::<Pkcs7>(&mut buf, data_len)
            .map_err(|_| BotError::MusicApi("Failed to encrypt eapi payload".to_string()))
            .unwrap_or(&[]);
        encode_upper(encrypted)
    }

    fn eapi_decrypt(hex_data: &str) -> Result<String> {
        let mut bytes = hex::decode(hex_data).map_err(|e| BotError::MusicApi(e.to_string()))?;
        let decrypted = Decryptor::<Aes128>::new_from_slice(b"e82ckenh8dichen8")
            .expect("eapi key length")
            .decrypt_padded_mut::<Pkcs7>(&mut bytes)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;
        String::from_utf8(decrypted.to_vec()).map_err(|e| BotError::MusicApi(e.to_string()))
    }

    fn eapi_params(path: &str, json: &str) -> String {
        let data = Self::eapi_splice(path, json);
        let encrypted = Self::eapi_encrypt(&data);
        format!("params={encrypted}")
    }

    fn choose_eapi_user_agent() -> &'static str {
        "NeteaseMusic/9.3.40.1753206443(164);Dalvik/2.1.0 (Linux; U; Android 9; MIX 2 MIUI/V12.0.1.0.PDECNXM)"
    }

    /// Get song details
    pub async fn get_song_detail(&self, song_id: u64) -> Result<SongDetail> {
        let url = format!("{}/api/song/detail", self.base_url);
        let mut params = HashMap::new();
        params.insert("id", song_id.to_string());
        params.insert("ids", format!("[{song_id}]"));

        let mut request = self.client.post(url).form(&params);

        // Add MUSIC_U cookie if available
        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        let response = request.send().await?;
        let data: SongDetailResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        data.songs
            .into_iter()
            .next()
            .ok_or_else(|| BotError::MusicApi("No song found".to_string()))
    }

    /// Get song download URL
    pub async fn get_song_url(&self, song_id: u64, br: u64) -> Result<SongUrl> {
        let url = format!("{}/api/song/enhance/player/url", self.base_url);
        let mut params = HashMap::new();
        params.insert("ids", format!("[{song_id}]"));
        params.insert("br", br.to_string());

        let mut request = self.client.post(url).form(&params);

        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        let response = request.send().await?;
        let data: SongUrlResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        data.data
            .into_iter()
            .next()
            .ok_or_else(|| BotError::MusicApi("No download URL found".to_string()))
    }

    /// Get song lyrics
    pub async fn get_song_lyric(&self, song_id: u64) -> Result<String> {
        let url = format!("{}/api/song/lyric?id={}&lv=1&tv=1", self.base_url, song_id);

        let mut request = self.client.get(&url);

        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        let response = request.send().await?;
        let data: LyricResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let lyric = data
            .lrc
            .map_or_else(|| "No lyrics available".to_string(), |l| l.lyric);

        Ok(lyric)
    }

    /// Search songs
    pub async fn search_songs(&self, keyword: &str, limit: u32) -> Result<Vec<SearchSong>> {
        let path = "/api/v1/search/song/get";
        let url = format!("{}/eapi/v1/search/song/get", self.base_url);
        let payload = serde_json::json!({
            "s": keyword,
            "offset": 0,
            "limit": limit.max(1),
        });
        let payload_str = payload.to_string();
        let body = Self::eapi_params(path, &payload_str);
        let request = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", Self::choose_eapi_user_agent())
            .header("Cookie", self.build_eapi_cookie())
            .body(body);

        let response = request.send().await?;
        let raw_body = response.text().await?;
        let trimmed = raw_body.trim_start();
        let data: EapiSearchResponse = if trimmed.starts_with('{') {
            serde_json::from_str(trimmed)?
        } else {
            let decrypted = Self::eapi_decrypt(trimmed)?;
            serde_json::from_str(&decrypted)?
        };

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        Ok(data.result.songs)
    }

    /// Download file with proper headers and cookies
    pub async fn download_file(&self, url: &str) -> Result<reqwest::Response> {
        // Apply host replacement similar to the original Go project
        // This helps avoid 403 errors from NetEase servers
        let processed_url = url
            .replace("m8.", "m7.")
            .replace("m801.", "m701.")
            .replace("m804.", "m701.")
            .replace("m704.", "m701.");

        let mut request = self.client.get(&processed_url);

        // Add MUSIC_U cookie if available
        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        // Add comprehensive headers to avoid 403 errors
        request = request
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
            .header("Referer", "https://music.163.com/")
            .header("Accept", "audio/mpeg, audio/*, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Cache-Control", "no-cache")
            .header("DNT", "1")
            .header("Sec-Fetch-Dest", "audio")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "cross-site");

        let response = request.send().await?;
        Ok(response)
    }

    /// Download and resize album art image
    pub async fn download_album_art(&self, pic_url: &str, output_path: &Path) -> Result<()> {
        let data = self.download_album_art_data(pic_url).await?;
        tokio::fs::write(output_path, data).await?;
        Ok(())
    }

    /// Download and resize album art image into memory
    /// Uses spawn_blocking for CPU-intensive image processing to avoid blocking async runtime
    pub async fn download_album_art_data(&self, pic_url: &str) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        // Download the image
        let mut request = self.client.get(pic_url);

        // Add headers for image download
        request = request
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            );

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;
        let bytes_vec = bytes.to_vec();

        // Process image in spawn_blocking to avoid blocking async runtime
        // Use a dedicated blocking task that completes and releases resources
        let processed = tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&bytes_vec)
                .map_err(|e| BotError::MusicApi(format!("Failed to decode image: {e}")))?;

            // Resize to 320x320 with black padding (like original Go project)
            let resized = resize_image_with_padding(img, 320, 320);

            // Save as JPEG into memory
            let mut cursor = Cursor::new(Vec::new());
            resized
                .write_to(&mut cursor, ImageFormat::Jpeg)
                .map_err(|e| BotError::MusicApi(format!("Failed to encode image: {e}")))?;

            Ok::<Vec<u8>, BotError>(cursor.into_inner())
        })
        .await
        .map_err(|e| BotError::MusicApi(format!("Image processing task failed: {e}")))??;

        Ok(processed)
    }

    /// Download original high-resolution album art without resizing (for embedding in audio files)
    pub async fn download_album_art_original(&self, pic_url: &str) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        // Download the image
        let mut request = self.client.get(pic_url);

        // Add headers for image download
        request = request
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            );

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;

        Ok(bytes.to_vec())
    }
}

/// Parse artists into a formatted string
#[must_use]
pub fn format_artists(artists: &[Artist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resize image with black padding to maintain aspect ratio (like the original Go project)
fn resize_image_with_padding(
    img: DynamicImage,
    target_width: u32,
    target_height: u32,
) -> DynamicImage {
    use image::{Rgb, RgbImage};

    let (orig_width, orig_height) = img.dimensions();
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
    for pixel in canvas.pixels_mut() {
        *pixel = Rgb([0, 0, 0]); // Black background
    }

    // Calculate position to center the resized image
    let offset_x = (target_width - new_width) / 2;
    let offset_y = (target_height - new_height) / 2;

    // Convert resized image to RGB and overlay on canvas
    let resized_rgb = resized.to_rgb8();
    for (x, y, pixel) in resized_rgb.enumerate_pixels() {
        if x + offset_x < target_width && y + offset_y < target_height {
            canvas.put_pixel(x + offset_x, y + offset_y, *pixel);
        }
    }

    DynamicImage::ImageRgb8(canvas)
}
