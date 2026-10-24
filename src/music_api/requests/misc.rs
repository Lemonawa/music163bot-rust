use futures_util::StreamExt;
use serde::Serialize;

use super::super::{
    ALBUM_ART_DOWNLOAD_OVERALL_TIMEOUT, EapiSearchResponse, LyricResponse, MusicApi, Result,
    SHORT_USER_AGENT, SearchSong, eapi_crypto, resize_album_art_to_thumbnail, rewrite_media_url,
    run_with_attempts_and_overall_timeout_with_err,
};
use crate::error::BotError;
use crate::utils::{is_trusted_music_media_url, is_trusted_music_share_url};

impl MusicApi {
    /// # Errors
    /// Returns an error if the API request fails or returns an error code.
    pub async fn get_song_lyric(&self, song_id: u64) -> Result<String> {
        if let Some(cached) = self.get_cached_song_lyric(song_id) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/lyric?id={}&lv=1&tv=1", self.base_url, song_id);

        let mut request = self.client.get(&url);

        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
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

        self.cache_song_lyric(song_id, lyric.clone());

        Ok(lyric)
    }

    /// Search songs
    ///
    /// # Errors
    /// Returns an error if the API request fails or returns an error code.
    pub async fn search_songs(&self, keyword: &str, limit: u32) -> Result<Vec<SearchSong>> {
        #[derive(Serialize)]
        struct SearchPayload<'a> {
            s: &'a str,
            offset: u32,
            limit: u32,
        }

        let path = "/api/v1/search/song/get";
        let url = format!("{}/eapi/v1/search/song/get", self.base_url);
        let payload_str = serde_json::to_string(&SearchPayload {
            s: keyword,
            offset: 0,
            limit: limit.max(1),
        })?;
        let body = eapi_crypto::eapi_params(path, &payload_str)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;
        let request = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", eapi_crypto::EAPI_USER_AGENT)
            .header("Cookie", self.build_eapi_cookie())
            .body(body);

        let response = request.send().await?.error_for_status()?;
        let raw_bytes = response.bytes().await?;
        // Skip leading whitespace bytes
        let trimmed_bytes = raw_bytes
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .map_or(&raw_bytes[..], |pos| &raw_bytes[pos..]);
        let data: EapiSearchResponse = if trimmed_bytes.first() == Some(&b'{') {
            serde_json::from_slice(trimmed_bytes)?
        } else {
            let trimmed_str = std::str::from_utf8(trimmed_bytes)
                .map_err(|e| BotError::MusicApi(format!("Invalid UTF-8 in response: {e}")))?;
            let decrypted = eapi_crypto::eapi_decrypt(trimmed_str)
                .map_err(|e| BotError::MusicApi(e.to_string()))?;
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
    ///
    /// # Errors
    /// Returns an error if the URL is untrusted or the download request fails.
    pub async fn download_file(&self, url: &str) -> Result<reqwest::Response> {
        // Apply host replacement similar to the original Go project.
        // VPS sampling shows the original m704/m804 hosts can return 403 while m701 succeeds.
        let processed_url = rewrite_media_url(url);
        if !is_trusted_music_media_url(processed_url.as_ref()) {
            tracing::warn!("Media download rejected: untrusted host in URL");
            return Err(BotError::MusicApi("Untrusted media host".to_string()));
        }
        let response = self
            .build_audio_download_request(processed_url.as_ref())
            .send()
            .await?;
        Ok(response)
    }

    /// Resolve final URL for share links with minimal body transfer
    ///
    /// # Errors
    /// Returns an error if the URL is untrusted or redirect resolution fails.
    pub async fn resolve_share_link(&self, url: &str) -> Result<reqwest::Url> {
        if !is_trusted_music_share_url(url) {
            return Err(BotError::MusicApi("Untrusted share-link host".to_string()));
        }

        let mut current = reqwest::Url::parse(url)
            .map_err(|e| BotError::MusicApi(format!("Invalid share-link URL: {e}")))?;

        for _ in 0..5 {
            let response = self
                .resolve_client
                .get(current.clone())
                .header("User-Agent", SHORT_USER_AGENT)
                .header("Accept", "*/*")
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await?;

            let status = response.status();
            if matches!(
                status,
                reqwest::StatusCode::MOVED_PERMANENTLY
                    | reqwest::StatusCode::FOUND
                    | reqwest::StatusCode::SEE_OTHER
                    | reqwest::StatusCode::TEMPORARY_REDIRECT
                    | reqwest::StatusCode::PERMANENT_REDIRECT
            ) {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or_else(|| {
                        BotError::MusicApi("Missing Location header in redirect".to_string())
                    })?
                    .to_str()
                    .map_err(|e| BotError::MusicApi(format!("Invalid redirect location: {e}")))?;
                let next = current
                    .join(location)
                    .map_err(|e| BotError::MusicApi(format!("Invalid redirect target URL: {e}")))?;
                if !is_trusted_music_share_url(next.as_str()) {
                    return Err(BotError::MusicApi(
                        "Untrusted share-link redirect host".to_string(),
                    ));
                }
                current = next;
                continue;
            }
            if status.is_redirection() {
                return Err(BotError::MusicApi(format!(
                    "Unsupported redirect status without Location handling: {status}"
                )));
            }

            response.error_for_status_ref()?;
            let final_url = response.url().clone();
            if !is_trusted_music_share_url(final_url.as_str()) {
                return Err(BotError::MusicApi(
                    "Untrusted share-link final host".to_string(),
                ));
            }
            return Ok(final_url);
        }

        Err(BotError::MusicApi(
            "Too many share-link redirects".to_string(),
        ))
    }

    /// Download album art image into memory, optionally resizing to thumbnail
    /// Uses `spawn_blocking` for CPU-intensive image processing to avoid blocking async runtime
    ///
    /// # Errors
    /// Returns an error if the URL is untrusted, the download fails, or image processing fails.
    pub async fn download_album_art_data(&self, pic_url: &str, resize: bool) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }
        if !is_trusted_music_media_url(pic_url) {
            tracing::warn!("Album art download rejected: untrusted host in URL");
            return Err(BotError::MusicApi("Untrusted album art host".to_string()));
        }

        run_with_attempts_and_overall_timeout_with_err(
            Self::album_art_total_attempts(),
            ALBUM_ART_DOWNLOAD_OVERALL_TIMEOUT,
            |attempt| async move {
                let result = self.download_album_art_data_once(pic_url, resize).await;
                if let Err(error) = &result {
                    tracing::warn!(
                        "Album art attempt {} failed: {}",
                        attempt,
                        error.sanitized_chain()
                    );
                }
                result
            },
            |elapsed| BotError::MusicApi(format!("Album art download timed out after {elapsed:?}")),
            |error| !matches!(error, BotError::AlbumArtTooLarge(_)),
        )
        .await
    }

    async fn download_album_art_data_once(&self, pic_url: &str, resize: bool) -> Result<Vec<u8>> {
        // Download the image with common headers
        let request = self.client.get(pic_url);
        let request = Self::apply_image_download_headers(request);

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        const MAX_ART_BYTES: usize = 10 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|len| len > MAX_ART_BYTES as u64)
        {
            return Err(BotError::AlbumArtTooLarge(MAX_ART_BYTES));
        }
        let bytes = read_album_art_stream(response.bytes_stream(), MAX_ART_BYTES).await?;

        if resize {
            // Process image in spawn_blocking to avoid blocking async runtime
            let processed =
                tokio::task::spawn_blocking(move || resize_album_art_to_thumbnail(&bytes))
                    .await
                    .map_err(|e| {
                        BotError::MusicApi(format!("Image processing task failed: {e}"))
                    })??;
            Ok(processed)
        } else {
            Ok(bytes)
        }
    }

    fn build_audio_download_request(&self, url: &str) -> reqwest::RequestBuilder {
        let request = self.client.get(url);
        let request = self.apply_music_u_cookie(request);
        Self::apply_audio_download_headers(request)
    }
}

async fn read_album_art_stream<S>(mut stream: S, limit: usize) -> Result<Vec<u8>>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    let mut data = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > limit.saturating_sub(data.len()) {
            return Err(BotError::AlbumArtTooLarge(limit));
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn album_art_stops_polling_at_size_limit() {
        let chunks = [
            bytes::Bytes::from_static(b"1234"),
            bytes::Bytes::from_static(b"5678"),
        ];
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let stream = futures_util::stream::iter(chunks.into_iter().map(Ok))
            .chain(futures_util::stream::pending())
            .inspect(|_| {
                polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_album_art_stream(stream, 5),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, BotError::AlbumArtTooLarge(5)));
        assert_eq!(polls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn album_art_accepts_exact_size_limit() {
        let stream = futures_util::stream::iter([Ok(bytes::Bytes::from_static(b"12345"))]);
        assert_eq!(read_album_art_stream(stream, 5).await.unwrap(), b"12345");
    }
    #[tokio::test]
    async fn chunked_album_art_exceeding_limit_is_not_retried() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                observed.fetch_add(1, Ordering::Relaxed);
                let mut request = [0; 4096];
                if socket.read(&mut request).await.is_err() {
                    continue;
                }
                if socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.is_err() { continue; }
                let chunk = vec![b'a'; 64 * 1024];
                for _ in 0..200 {
                    if socket.write_all(b"10000\r\n").await.is_err()
                        || socket.write_all(&chunk).await.is_err()
                        || socket.write_all(b"\r\n").await.is_err()
                    {
                        break;
                    }
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        });
        let mut api = MusicApi::new(None, "http://127.0.0.1:1".into());
        api.client = reqwest::Client::builder()
            .no_proxy()
            .resolve("p1.music.126.net", address)
            .build()
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            api.download_album_art_data(
                &format!("http://p1.music.126.net:{}/cover", address.port()),
                false,
            ),
        )
        .await;
        server.abort();
        assert!(matches!(
            result.unwrap(),
            Err(BotError::AlbumArtTooLarge(_))
        ));
        assert_eq!(requests.load(Ordering::Relaxed), 1);
    }
}
