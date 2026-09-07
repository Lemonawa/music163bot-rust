use std::collections::HashMap;

use super::super::eapi_crypto;
use super::super::{MusicApi, Result, ServedSize, SongUrlResponse};
use crate::error::BotError;

impl MusicApi {
    /// Probe the eapi song-url endpoint for a batch of song ids at `hires`
    /// level, returning what the account would currently be served per id
    /// (bitrate, size, format) — the same endpoint and decoding the request
    /// path uses, but uncached and without URL selection. Ids whose served
    /// size is zero (tier unavailable) are omitted.
    ///
    /// Used by maintenance tooling to compare the ground-truth served file
    /// against a cached copy.
    pub async fn get_served_sizes_batch(&self, ids: &[i64]) -> Result<HashMap<i64, ServedSize>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let path = "/api/song/enhance/player/url/v1";
        let url = format!("{}/eapi/song/enhance/player/url/v1", self.base_url);
        let ids_str = format!(
            "[{}]",
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        );
        let payload = serde_json::json!({
            "ids": ids_str,
            "level": "hires",
            "encodeType": "mp3",
            "header": "{}",
        });
        let payload_str = serde_json::to_string(&payload)?;
        let body = eapi_crypto::eapi_params(path, &payload_str)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;

        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", eapi_crypto::EAPI_USER_AGENT)
            .header("Cookie", self.build_eapi_cookie())
            .body(body)
            .send()
            .await?
            .error_for_status()?;

        let raw_bytes = response.bytes().await?;
        let trimmed_bytes = raw_bytes
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .map_or(&raw_bytes[..], |pos| &raw_bytes[pos..]);
        let data: SongUrlResponse = if trimmed_bytes.first() == Some(&b'{') {
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

        Ok(data
            .data
            .into_iter()
            .filter(|entry| entry.size > 0)
            .map(|entry| {
                (
                    i64::try_from(entry.id).unwrap_or(0),
                    ServedSize {
                        br: u64_to_i64_saturating(entry.br),
                        size: u64_to_i64_saturating(entry.size),
                        format: entry.format,
                    },
                )
            })
            .collect())
    }
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
