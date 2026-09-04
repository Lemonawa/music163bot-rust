//! `NetEase` eapi crypto primitives shared by the bot and the `refresh_hires`
//! binary (included via `#[path]` there, so this file may only depend on
//! `anyhow` plus the crypto crates — never on crate internals).

use aes::Aes128;
use cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, block_padding::Pkcs7};
use ecb::{Decryptor, Encryptor};

/// `NetEase` eapi AES key.
pub(crate) const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";

/// The `User-Agent` header sent for eapi requests.
pub(crate) const EAPI_USER_AGENT: &str = "NeteaseMusic/9.3.40.1753206443(164);Dalvik/2.1.0 (Linux; U; Android 9; MIX 2 MIUI/V12.0.1.0.PDECNXM)";

fn eapi_splice(path: &str, json: &str) -> String {
    let text = format!("nobody{path}use{json}md5forencrypt");
    let digest = md5::compute(text.as_bytes());
    let hex_digest = format!("{digest:x}");
    // Pre-allocate: path + "-36cd479b6b5-" + json + "-36cd479b6b5-" + hex_digest
    let mut result = String::with_capacity(path.len() + json.len() + hex_digest.len() + 26);
    result.push_str(path);
    result.push_str("-36cd479b6b5-");
    result.push_str(json);
    result.push_str("-36cd479b6b5-");
    result.push_str(&hex_digest);
    result
}

pub(crate) fn eapi_encrypt(data: &str) -> anyhow::Result<String> {
    eapi_encrypt_with_key(data, EAPI_KEY)
}

pub(crate) fn eapi_encrypt_with_key(data: &str, key: &[u8]) -> anyhow::Result<String> {
    let block_size = 16;
    let data_len = data.len();
    let padded_len = ((data_len + block_size) / block_size) * block_size;
    let mut buf = vec![0u8; padded_len];
    buf[..data_len].copy_from_slice(data.as_bytes());
    let encrypted = Encryptor::<Aes128>::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("Invalid eapi key length"))?
        .encrypt_padded::<Pkcs7>(&mut buf, data_len)
        .map_err(|_| anyhow::anyhow!("Failed to encrypt eapi payload"))?;
    Ok(hex::encode_upper(encrypted))
}

pub(crate) fn eapi_decrypt(hex_data: &str) -> anyhow::Result<String> {
    eapi_decrypt_with_key(hex_data, EAPI_KEY)
}

pub(crate) fn eapi_decrypt_with_key(hex_data: &str, key: &[u8]) -> anyhow::Result<String> {
    let mut bytes = hex::decode(hex_data).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let decrypted = Decryptor::<Aes128>::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("Invalid eapi key length"))?
        .decrypt_padded::<Pkcs7>(&mut bytes)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    String::from_utf8(decrypted.to_vec()).map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Build the `params=` body for an eapi request.
pub(crate) fn eapi_params(path: &str, json: &str) -> anyhow::Result<String> {
    let data = eapi_splice(path, json);
    let encrypted = eapi_encrypt(&data)?;
    Ok(format!("params={encrypted}"))
}

/// Build the `Cookie` header sent for eapi requests. Without a `MUSIC_U`
/// carries the public anonymous `MUSIC_A` token (carried over from the
/// upstream Go project) so a fresh deployment can hit the search/eapi
/// endpoints without per-user login.
pub(crate) fn eapi_cookie(music_u: Option<&str>) -> String {
    let device_id = uuid::Uuid::new_v4().simple().to_string();
    let appver = "9.3.40";
    let buildver = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(
            |_| "0".to_string(),
            |duration| duration.as_secs().to_string(),
        );
    let mut cookie_parts = vec![
        format!("deviceId={device_id}"),
        format!("appver={appver}"),
        format!("buildver={}", &buildver[..buildver.len().min(10)]),
        "resolution=1920x1080".to_string(),
        "os=Android".to_string(),
    ];

    if let Some(music_u) = music_u {
        cookie_parts.push(format!("MUSIC_U={music_u}"));
    } else {
        cookie_parts.push("MUSIC_A=4ee5f776c9ed1e4d5f031b09e084c6cb333e43ee4a841afeebbef9bbf4b7e4152b51ff20ecb9e8ee9e89ab23044cf50d1609e4781e805e73a138419e5583bc7fd1e5933c52368d9127ba9ce4e2f233bf5a77ba40ea6045ae1fc612ead95d7b0e0edf70a74334194e1a190979f5fc12e9968c3666a981495b33a649814e309366".to_string());
    }

    cookie_parts.join("; ")
}
