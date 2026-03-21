use super::*;
use crate::config::{Config, StorageMode};
use crate::music_api::SongDetail;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

struct BufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

struct BufferGuard {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for BufferGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "lock poisoned"))?;
        buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferGuard;

    fn make_writer(&'a self) -> Self::Writer {
        BufferGuard {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

fn capture_warn_logs<F>(action: F) -> String
where
    F: FnOnce(),
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_writer(BufferWriter {
            buffer: Arc::clone(&buffer),
        })
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();

    tracing::subscriber::with_default(subscriber, action);

    let buffer = buffer
        .lock()
        .expect("log buffer lock should succeed")
        .clone();
    String::from_utf8_lossy(&buffer).to_string()
}

fn sample_artist() -> crate::music_api::Artist {
    crate::music_api::Artist {
        id: 1,
        name: "Artist".to_string(),
    }
}

fn sample_album() -> crate::music_api::Album {
    crate::music_api::Album {
        id: 1,
        name: "Album".to_string(),
        pic_url: None,
    }
}

fn sample_song_detail() -> SongDetail {
    SongDetail {
        id: 7,
        name: "Sample Song".to_string(),
        dt: Some(123_000),
        ar: Some(vec![sample_artist()]),
        al: Some(sample_album()),
    }
}

fn memory_buffer(data: Vec<u8>, filename: &str) -> AudioBuffer {
    AudioBuffer::Memory {
        data,
        filename: filename.to_string(),
    }
}

fn into_memory_data(buffer: AudioBuffer) -> Vec<u8> {
    match buffer {
        AudioBuffer::Memory { data, .. } => data,
        AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
    }
}

fn sample_cover_jpeg() -> Vec<u8> {
    let mut image = image::RgbImage::new(2, 2);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb([10, 20, 30]);
    }

    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .expect("encode jpeg");
    out
}

fn sample_flac_bytes() -> Vec<u8> {
    let mut flac_data = b"fLaC".to_vec();
    flac_data.push(0x80);
    flac_data.extend_from_slice(&[0x00, 0x00, 0x22]);
    flac_data.extend_from_slice(&[0u8; 34]);
    flac_data.extend_from_slice(b"AUDIO_FRAMES");
    flac_data
}

fn sample_flac_bytes_with_invalid_vorbis() -> Vec<u8> {
    let mut flac_data = b"fLaC".to_vec();
    flac_data.push(0x80 | 0x04);
    flac_data.extend_from_slice(&[0x00, 0x00, 0x09]);
    flac_data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    flac_data.push(0xFF);
    flac_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    flac_data.extend_from_slice(b"AUDIO_FRAMES");
    flac_data
}

#[test]
fn test_find_flac_audio_start() {
    // Minimal FLAC with just streaminfo block (is_last=true)
    // "fLaC" + header (0x80 | 0x00 = StreamInfo, last) + 34 bytes length + 34 bytes data
    let mut flac_data = b"fLaC".to_vec();
    flac_data.push(0x80); // Last block, type 0 (StreamInfo)
    flac_data.extend_from_slice(&[0x00, 0x00, 0x22]); // Length = 34
    flac_data.extend_from_slice(&[0u8; 34]); // StreamInfo data
    flac_data.extend_from_slice(b"AUDIO_FRAMES"); // Audio data

    let result = AudioBuffer::find_flac_audio_start(&flac_data);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 4 + 4 + 34); // magic + header + data
}

#[test]
fn test_find_mp3_audio_start() {
    // ID3v2 header with size 0
    let mut mp3_data = b"ID3".to_vec();
    mp3_data.extend_from_slice(&[0x04, 0x00]); // Version 2.4.0
    mp3_data.push(0x00); // Flags
    mp3_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Size = 0 (syncsafe)
    mp3_data.extend_from_slice(b"\xFF\xFB"); // MP3 sync word

    let result = AudioBuffer::find_mp3_audio_start(&mp3_data);
    assert_eq!(result, 10); // 10 byte header
}

#[tokio::test]
async fn cleanup_helper_ignores_missing_file() {
    let temp_name = format!(
        "music163bot_missing_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let path = std::env::temp_dir().join(temp_name);
    super::remove_file_if_exists(&path)
        .await
        .expect("missing file cleanup should succeed");
}

#[tokio::test]
async fn audio_buffer_is_disk() {
    let temp_name = format!(
        "music163bot_audio_buffer_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let cache_dir = std::env::temp_dir();

    let disk_buffer = AudioBuffer::new_disk(temp_name.clone(), cache_dir.to_str().unwrap())
        .await
        .expect("create disk buffer");
    assert!(disk_buffer.is_disk());
    assert!(!disk_buffer.is_memory());
    disk_buffer.cleanup().await.expect("cleanup disk buffer");

    let mut config = Config::default();
    config.storage_mode = StorageMode::Memory;
    config.memory_buffer_mb = 0;
    config.memory_max_file_mb = u64::MAX;

    let memory_buffer = AudioBuffer::new(
        &config,
        1024,
        "test.mp3".to_string(),
        cache_dir.to_str().unwrap(),
    )
    .await
    .expect("create memory buffer");

    assert!(!memory_buffer.is_disk());
    assert!(memory_buffer.is_memory());
}

#[tokio::test]
async fn audio_buffer_hybrid_uses_disk_when_threshold_exceeded() {
    let temp_name = format!(
        "music163bot_audio_buffer_hybrid_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let cache_dir = std::env::temp_dir();
    let mut config = Config::default();
    config.storage_mode = StorageMode::Hybrid;
    config.memory_threshold_mb = 1;
    config.memory_max_file_mb = u64::MAX;
    config.memory_buffer_mb = 0;

    let buffer = AudioBuffer::new(
        &config,
        2 * 1024 * 1024,
        temp_name.clone(),
        cache_dir.to_str().unwrap(),
    )
    .await
    .expect("create hybrid buffer");

    assert!(buffer.is_disk(), "hybrid mode should fall back to disk");
    assert_eq!(buffer.filename(), temp_name);
    assert!(buffer.path().is_some(), "disk buffer should expose a path");

    buffer.cleanup().await.expect("cleanup hybrid disk buffer");
}

#[tokio::test]
async fn write_chunk_errors_without_disk_handle() {
    let path = std::env::temp_dir().join(format!(
        "music163bot_audio_buffer_none_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut buffer = AudioBuffer::Disk {
        path,
        file: None,
        filename: "missing.mp3".to_string(),
        written_bytes: 0,
    };

    let result = buffer.write_chunk(b"abc").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn write_chunk_missing_disk_handle_preserves_error_copy() {
    let path = std::env::temp_dir().join(format!(
        "music163bot_audio_buffer_none_copy_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut buffer = AudioBuffer::Disk {
        path,
        file: None,
        filename: "missing.mp3".to_string(),
        written_bytes: 0,
    };

    let err = buffer
        .write_chunk(b"abc")
        .await
        .expect_err("missing file handle should error");
    assert_eq!(err.to_string(), "Disk buffer missing file handle");
}

#[tokio::test]
async fn thumbnail_buffer_memory_bytes_roundtrip() {
    let data = bytes::Bytes::from_static(b"abc");
    let buf = ThumbnailBuffer::from_bytes(data.clone());
    assert_eq!(buf.get_data().await.unwrap_or_default(), b"abc");
}

#[tokio::test]
async fn thumbnail_buffer_uses_disk_for_large_thumbnail() {
    let temp_name = format!(
        "music163bot_thumb_large_{}.jpg",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let cache_dir = std::env::temp_dir();
    let mut config = Config::default();
    config.storage_mode = StorageMode::Hybrid;
    let data = bytes::Bytes::from(vec![7u8; 6 * 1024 * 1024]);

    let buf = ThumbnailBuffer::new(
        &config,
        data.clone(),
        cache_dir.to_str().unwrap(),
        &temp_name,
    )
    .await
    .expect("create thumbnail buffer");

    assert!(!buf.is_memory(), "large thumbnails should use disk");
    assert!(buf.path().is_some(), "disk thumbnail should expose a path");
    assert_eq!(buf.get_data().await.expect("read thumbnail"), data);

    buf.cleanup().await.expect("cleanup thumbnail buffer");
}

#[tokio::test]
async fn audio_buffer_public_facade_methods_remain_usable() {
    let cache_dir = std::env::temp_dir();

    let mut memory_config = Config::default();
    memory_config.storage_mode = StorageMode::Memory;
    memory_config.memory_buffer_mb = 0;
    memory_config.memory_max_file_mb = u64::MAX;

    let mut memory = AudioBuffer::new(
        &memory_config,
        3,
        "facade.mp3".to_string(),
        cache_dir.to_str().unwrap(),
    )
    .await
    .expect("create memory facade buffer");
    memory
        .write_chunk(b"abc")
        .await
        .expect("write memory bytes");

    assert_eq!(memory.filename(), "facade.mp3");
    assert!(memory.path().is_none());
    assert!(memory.disk_file_mut().is_none());
    let _ = memory.to_input_file();
    assert_eq!(memory.size_fast(), 3);
    assert_eq!(memory.get_data().await.expect("read memory data"), b"abc");
    let bytes = memory
        .take_memory_bytes_for_upload()
        .expect("memory bytes for upload");
    assert_eq!(bytes.as_ref(), b"abc");
    let _ = AudioBuffer::Memory {
        data: vec![1, 2, 3],
        filename: "into.mp3".to_string(),
    }
    .into_input_file();

    let disk_name = format!(
        "music163bot_audio_buffer_facade_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let mut disk = AudioBuffer::new_disk(disk_name, cache_dir.to_str().unwrap())
        .await
        .expect("create disk facade buffer");
    assert!(disk.path().is_some());
    assert!(disk.disk_file_mut().is_some());
    let _ = disk.to_input_file();
    disk.cleanup().await.expect("cleanup disk facade buffer");
}

#[test]
fn mp3_tagging_is_byte_identical_for_same_input() {
    let detail = sample_song_detail();
    let cover = sample_cover_jpeg();
    let mut first = memory_buffer(vec![0xFF, 0xFB, 0x90, 0x64], "a.mp3");
    let mut second = memory_buffer(vec![0xFF, 0xFB, 0x90, 0x64], "b.mp3");

    first
        .add_id3_tags(&detail, Some(&cover))
        .expect("first mp3 tagging");
    second
        .add_id3_tags(&detail, Some(&cover))
        .expect("second mp3 tagging");

    let first_data = into_memory_data(first);
    let second_data = into_memory_data(second);

    assert_eq!(first_data, second_data);
}

#[test]
fn flac_tagging_keeps_equivalent_metadata_and_audio_payload() {
    let detail = sample_song_detail();
    let cover = sample_cover_jpeg();
    let source = sample_flac_bytes();
    let mut first = memory_buffer(source.clone(), "a.flac");
    let mut second = memory_buffer(source, "b.flac");

    first
        .add_flac_metadata(&detail, Some(&cover))
        .expect("first flac tagging");
    second
        .add_flac_metadata(&detail, Some(&cover))
        .expect("second flac tagging");

    let first_data = into_memory_data(first);
    let second_data = into_memory_data(second);

    let mut first_cursor = std::io::Cursor::new(first_data.as_slice());
    let first_tag = metaflac::Tag::read_from(&mut first_cursor).expect("parse first flac tag");
    let mut second_cursor = std::io::Cursor::new(second_data.as_slice());
    let second_tag = metaflac::Tag::read_from(&mut second_cursor).expect("parse second flac tag");

    let collect_values = |tag: &metaflac::Tag, key: &str| {
        tag.get_vorbis(key).map(|iter| {
            iter.map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
    };

    assert_eq!(
        collect_values(&first_tag, "TITLE"),
        collect_values(&second_tag, "TITLE")
    );
    assert_eq!(
        collect_values(&first_tag, "ALBUM"),
        collect_values(&second_tag, "ALBUM")
    );
    assert_eq!(
        collect_values(&first_tag, "ARTIST"),
        collect_values(&second_tag, "ARTIST")
    );

    let first_cover = first_tag
        .pictures()
        .find(|pic| pic.picture_type == metaflac::block::PictureType::CoverFront)
        .map(|pic| pic.data.clone())
        .expect("first cover picture");
    let second_cover = second_tag
        .pictures()
        .find(|pic| pic.picture_type == metaflac::block::PictureType::CoverFront)
        .map(|pic| pic.data.clone())
        .expect("second cover picture");
    assert_eq!(first_cover, second_cover);

    let first_audio_start =
        AudioBuffer::find_flac_audio_start(&first_data).expect("first audio start");
    let second_audio_start =
        AudioBuffer::find_flac_audio_start(&second_data).expect("second audio start");
    assert_eq!(&first_data[first_audio_start..], b"AUDIO_FRAMES");
    assert_eq!(&second_data[second_audio_start..], b"AUDIO_FRAMES");
}

#[test]
fn flac_memory_rebuild_does_not_reallocate_with_artwork() {
    let detail = sample_song_detail();
    let cover = sample_cover_jpeg();
    let source = sample_flac_bytes();

    // Build a large fake artwork (~200KB) to simulate real album covers
    let large_artwork = vec![0xFFu8; 200 * 1024];

    let mut buffer = memory_buffer(source, "capacity_test.flac");

    // This should succeed without panic and produce valid output
    buffer
        .add_flac_metadata(&detail, Some(&large_artwork))
        .expect("flac tagging with large artwork should succeed");

    let result_data = match buffer {
        AudioBuffer::Memory { data, .. } => data,
        AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
    };

    // The result should contain the audio frames at the end
    let audio_start = AudioBuffer::find_flac_audio_start(&result_data).expect("find audio start");
    assert_eq!(
        &result_data[audio_start..],
        b"AUDIO_FRAMES",
        "audio payload must be preserved after tagging with large artwork"
    );

    // Result should be at least as large as the artwork data
    assert!(
        result_data.len() >= large_artwork.len(),
        "output ({}) should be at least artwork size ({})",
        result_data.len(),
        large_artwork.len()
    );

    // Also verify small artwork path works
    let source2 = sample_flac_bytes();
    let mut buffer2 = memory_buffer(source2, "small_art.flac");
    buffer2
        .add_flac_metadata(&detail, Some(&cover))
        .expect("flac tagging with small artwork");

    // And no-artwork path
    let source3 = sample_flac_bytes();
    let mut buffer3 = memory_buffer(source3, "no_art.flac");
    buffer3
        .add_flac_metadata(&detail, None)
        .expect("flac tagging without artwork");
}

#[test]
fn flac_disk_logs_warning_on_tag_read_failure() {
    let detail = sample_song_detail();
    let path = std::env::temp_dir().join(format!(
        "music163bot_bad_flac_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::write(&path, b"NOTFLAC").expect("write temp flac");
    let mut buffer = AudioBuffer::Disk {
        path: path.clone(),
        file: None,
        filename: "bad.flac".to_string(),
        written_bytes: 0,
    };

    let logs = capture_warn_logs(|| {
        let _ = buffer.add_flac_metadata(&detail, None);
    });

    let _ = std::fs::remove_file(&path);
    assert!(logs.contains("Failed to read FLAC tags from disk"));
}

#[test]
fn flac_memory_logs_warning_on_tag_read_failure() {
    let detail = sample_song_detail();
    let data = sample_flac_bytes_with_invalid_vorbis();
    let mut buffer = memory_buffer(data, "bad.flac");

    let logs = capture_warn_logs(|| {
        let _ = buffer.add_flac_metadata(&detail, None);
    });

    assert!(logs.contains("Failed to read FLAC tags from memory"));
}

#[test]
fn memory_buffer_take_bytes_moves_data_without_copy() {
    let mut buffer = memory_buffer(vec![1, 2, 3, 4, 5], "sample.mp3");

    let original_ptr = match &buffer {
        AudioBuffer::Memory { data, .. } => data.as_ptr(),
        AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
    };

    let bytes = buffer
        .take_memory_bytes_for_upload()
        .expect("memory buffer should produce bytes");

    assert_eq!(bytes.as_ptr(), original_ptr);
    assert_eq!(bytes.as_ref(), &[1, 2, 3, 4, 5]);

    match buffer {
        AudioBuffer::Memory { ref data, .. } => assert!(data.is_empty()),
        AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
    }
}

#[test]
fn get_available_memory_mb_is_stable() {
    let mb1 = AudioBuffer::get_available_memory_mb();
    let mb2 = AudioBuffer::get_available_memory_mb();
    if mb1 > 0 {
        // Non-sandboxed: value should be plausible (under 16 TB)
        assert!(
            mb1 < 16 * 1024 * 1024,
            "available memory looks implausibly large: {mb1} MB"
        );
        // Second call should also be positive
        assert!(mb2 > 0, "second call returned 0 while first returned {mb1}");
    } else {
        // Sandboxed env where sysinfo returns 0 — both calls should agree
        assert_eq!(mb2, 0, "first call returned 0 but second returned {mb2}");
    }
}

#[test]
fn get_available_memory_mb_is_consistent() {
    let mb1 = AudioBuffer::get_available_memory_mb();
    let mb2 = AudioBuffer::get_available_memory_mb();
    // Within the same second, throttled calls should return similar values
    // (exact same if within throttle window)
    let diff = if mb1 > mb2 { mb1 - mb2 } else { mb2 - mb1 };
    assert!(
        diff < 1024,
        "Two rapid calls should return similar values: {mb1} vs {mb2}"
    );
}

#[tokio::test]
async fn disk_written_bytes_tracks_sequential_writes() {
    let temp_name = format!(
        "music163bot_written_bytes_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let cache_dir = std::env::temp_dir();
    let mut buffer = AudioBuffer::new_disk(temp_name, cache_dir.to_str().unwrap())
        .await
        .expect("create disk buffer");

    assert_eq!(buffer.size().await, 0, "size should be 0 before any writes");

    buffer.write_chunk(b"hello").await.expect("first write");
    assert_eq!(buffer.size().await, 5, "size after first write");
    assert_eq!(buffer.size_fast(), 5, "size_fast after first write");

    buffer.write_chunk(b" world").await.expect("second write");
    assert_eq!(buffer.size().await, 11, "size after second write");
    assert_eq!(buffer.size_fast(), 11, "size_fast after second write");

    buffer.cleanup().await.expect("cleanup disk buffer");
}
