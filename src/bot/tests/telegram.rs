use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use teloxide::update_listeners::AsUpdateStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[test]
fn redact_bot_token_in_error_message_masks_bot_path_segment() {
    let raw = "error sending request for url (http://127.0.0.1:8081/bot123456789:fake_test_token/sendAudio)";

    let redacted = super::redact_bot_token_in_error_message(raw);

    assert!(!redacted.contains("123456789:fake_test_token"));
    assert!(redacted.contains("/bot<redacted>/sendAudio"));
}

#[test]
fn parse_telegram_api_response_returns_error_when_http_200_ok_false() {
    let body = r#"{"ok": false, "description": "chat not found"}"#;
    let err = super::parse_telegram_api_response(body, reqwest::StatusCode::OK, "sendAudio")
        .expect_err("ok=false should be treated as Telegram API error");
    let err_msg = err.to_string();

    assert!(err_msg.contains("chat not found"));
    assert!(err_msg.contains("HTTP 200"));
}

#[test]
fn parse_telegram_api_response_returns_error_when_http_500_for_any_ok_flag() {
    let cases = [
        (r#"{"ok": true, "result": {}}"#, "unknown error"),
        (
            r#"{"ok": false, "description": "server failed"}"#,
            "server failed",
        ),
    ];

    for (body, expected_desc) in cases {
        let err = super::parse_telegram_api_response(
            body,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "sendAudio",
        )
        .expect_err("non-2xx status should always be treated as error");
        let err_msg = err.to_string();

        assert!(err_msg.contains(expected_desc));
        assert!(err_msg.contains("HTTP 500"));
    }
}

#[test]
fn parse_telegram_api_response_returns_parse_error_for_non_json_body() {
    let err = super::parse_telegram_api_response(
        "<html>502 bad gateway</html>",
        reqwest::StatusCode::OK,
        "sendAudio",
    )
    .expect_err("non-JSON body should fail response parsing");
    let err_msg = err.to_string();

    // BotError::Serialization wraps serde_json::Error directly; check it's a parse error
    assert!(
        err_msg.contains("expected") || err_msg.contains("invalid") || err_msg.contains("EOF"),
        "unexpected error message: {err_msg}"
    );
}

#[test]
fn parse_telegram_api_response_uses_unknown_error_when_description_missing() {
    let err = super::parse_telegram_api_response(
        r#"{"ok": false}"#,
        reqwest::StatusCode::OK,
        "sendAudio",
    )
    .expect_err("missing description should still return a Telegram API error");
    let err_msg = err.to_string();

    assert!(err_msg.contains("unknown error"));
    assert!(err_msg.contains("HTTP 200"));
}

#[test]
fn parse_telegram_api_response_preserves_retry_after_hint_for_429() {
    let body = r#"{"ok": false, "description": "Too Many Requests: retry after 26", "parameters": {"retry_after": 26}}"#;
    let err = super::parse_telegram_api_response(
        body,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "sendAudio",
    )
    .expect_err("429 should be treated as Telegram API error");
    let err_msg = err.to_string();

    assert!(err_msg.contains("HTTP 429"));
    assert_eq!(
        crate::utils::extract_retry_after_seconds(&err_msg),
        Some(26)
    );
}

#[test]
fn parse_telegram_api_response_redacts_sensitive_description_text() {
    let body = r#"{"ok": false, "description": "proxy said http://127.0.0.1:8081/bot123456789:fake_test_token/sendAudio failed"}"#;
    let err =
        super::parse_telegram_api_response(body, reqwest::StatusCode::BAD_GATEWAY, "sendAudio")
            .expect_err("sensitive description should still return an error");
    let err_msg = err.to_string();

    assert!(!err_msg.contains("123456789:fake_test_token"));
    assert!(err_msg.contains("/bot<redacted>/sendAudio"));
}

#[test]
fn extract_file_id_reads_audio_field() {
    let payload = serde_json::json!({
        "ok": true,
        "result": {
            "audio": {
                "file_id": "audio_file_123"
            }
        }
    });

    assert_eq!(
        super::extract_file_id_from_response(&payload),
        Some("audio_file_123".to_string())
    );
}

#[test]
fn inflight_registry_first_is_leader() {
    let inflight = Arc::new(super::InflightDownloads::default());
    let claim = inflight.begin(42);
    assert!(matches!(claim, super::InflightClaim::Leader(_)));
}

#[tokio::test]
async fn inflight_registry_second_waits() {
    let inflight = Arc::new(super::InflightDownloads::default());
    let leader = match inflight.begin(99) {
        super::InflightClaim::Leader(guard) => guard,
        super::InflightClaim::Follower(_) => panic!("first claim should be leader"),
    };

    let follower_entry = match inflight.begin(99) {
        super::InflightClaim::Leader(_) => panic!("second claim should be follower"),
        super::InflightClaim::Follower(entry) => entry,
    };

    let pending = tokio::time::timeout(Duration::from_millis(20), follower_entry.wait()).await;
    assert!(pending.is_err(), "follower should wait while leader active");

    drop(leader);

    tokio::time::timeout(Duration::from_secs(1), follower_entry.wait())
        .await
        .expect("follower should be released after leader finishes");
}

#[tokio::test]
async fn singleflight_claim_helper_waits_for_existing_leader() {
    let inflight = Arc::new(super::InflightDownloads::default());
    let leader = super::acquire_download_leader(&inflight, 7)
        .await
        .expect("first claim should be leader");

    let inflight_for_follower = Arc::clone(&inflight);
    let follower = tokio::spawn(async move {
        super::acquire_download_leader(&inflight_for_follower, 7)
            .await
            .is_none()
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!follower.is_finished(), "follower should still be waiting");

    drop(leader);

    let waited = tokio::time::timeout(Duration::from_secs(1), follower)
        .await
        .expect("follower task should complete")
        .expect("follower task join should succeed");
    assert!(waited, "follower claim should resolve as waiting follower");
}

#[tokio::test]
async fn tagging_wrapper_returns_buffer_for_mp3_without_artwork() {
    let buffer = crate::audio_buffer::AudioBuffer::Memory {
        data: vec![1, 2, 3],
        filename: "sample.mp3".to_string(),
    };
    let detail = crate::music_api::SongDetail {
        id: 1,
        name: "Song".to_string(),
        dt: Some(1_000),
        ar: Some(vec![]),
        al: None,
    };

    let tagged = super::apply_tags_in_blocking(
        buffer,
        super::AudioFormat::Mp3,
        Arc::new(detail),
        None,
        false,
    )
    .await
    .expect("mp3 format should keep buffer");

    assert!(tagged.size().await >= 3);
}

#[tokio::test]
async fn tagging_wrapper_adds_mp3_id3_header() {
    let buffer = crate::audio_buffer::AudioBuffer::Memory {
        data: vec![0xFF, 0xFB, 0x90, 0x64],
        filename: "sample.mp3".to_string(),
    };
    let detail = crate::music_api::SongDetail {
        id: 2,
        name: "Song".to_string(),
        dt: Some(120_000),
        ar: Some(vec![crate::music_api::Artist {
            id: 1,
            name: "Artist".to_string(),
        }]),
        al: Some(crate::music_api::Album {
            id: 1,
            name: "Album".to_string(),
            pic_url: None,
        }),
    };

    let tagged = super::apply_tags_in_blocking(
        buffer,
        super::AudioFormat::Mp3,
        Arc::new(detail),
        None,
        false,
    )
    .await
    .expect("mp3 tagging should succeed");
    let data = tagged.get_data().await.expect("read tagged data");
    assert!(data.starts_with(b"ID3"));
}

#[tokio::test]
async fn startup_update_listener_skips_pending_updates() {
    let server = MockTelegramPollingServer::start().await;
    let api_url = reqwest::Url::parse(&server.base_url()).expect("valid mock api url");
    let bot = Bot::new("123456:TEST").set_api_url(api_url);

    let mut listener = super::entry::build_startup_update_listener(bot).await;
    let stream = listener.as_stream();
    tokio::pin!(stream);
    let update = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("listener should yield an update")
        .expect("stream should produce an item")
        .expect("listener request should succeed");

    assert_eq!(update.id.0, 43);
    assert_eq!(server.get_updates_offsets(), vec![-1, 43]);
}

#[derive(Debug, Default)]
struct MockTelegramPollingServerState {
    get_updates_offsets: Vec<i64>,
}

struct MockTelegramPollingServer {
    base_url: String,
    state: Arc<Mutex<MockTelegramPollingServerState>>,
    accept_loop_task: JoinHandle<()>,
}

impl MockTelegramPollingServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock telegram polling server");
        let address = listener
            .local_addr()
            .expect("mock telegram polling server local addr");
        let state = Arc::new(Mutex::new(MockTelegramPollingServerState::default()));
        let shared_state = Arc::clone(&state);

        let accept_loop_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection_state = Arc::clone(&shared_state);
                tokio::spawn(async move {
                    let _ = handle_mock_telegram_polling_connection(stream, connection_state).await;
                });
            }
        });

        Self {
            base_url: format!("http://{address}/"),
            state,
            accept_loop_task,
        }
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn get_updates_offsets(&self) -> Vec<i64> {
        self.state
            .lock()
            .expect("lock mock telegram polling server state")
            .get_updates_offsets
            .clone()
    }
}

impl Drop for MockTelegramPollingServer {
    fn drop(&mut self) {
        self.accept_loop_task.abort();
    }
}

async fn handle_mock_telegram_polling_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<MockTelegramPollingServerState>>,
) -> std::io::Result<()> {
    let Some((path, request_body)) = read_http_request(&mut stream).await? else {
        return Ok(());
    };

    let method = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = match method.as_str() {
        "getwebhookinfo" => mock_get_webhook_info_response_json(),
        "getupdates" => mock_get_updates_response_json(&state, &path, &request_body),
        _ => serde_json::json!({
            "ok": false,
            "description": format!("unsupported method: {method}")
        })
        .to_string(),
    };

    write_json_response(&mut stream, &body).await
}

fn mock_get_webhook_info_response_json() -> String {
    serde_json::json!({
        "ok": true,
        "result": {
            "url": "",
            "has_custom_certificate": false,
            "pending_update_count": 2,
            "allowed_updates": ["message"]
        }
    })
    .to_string()
}

fn mock_get_updates_response_json(
    state: &Arc<Mutex<MockTelegramPollingServerState>>,
    path: &str,
    request_body: &str,
) -> String {
    let offset = parse_request_field_as_i64(path, request_body, "offset").unwrap_or(0);
    let mut guard = state
        .lock()
        .expect("lock mock telegram polling server state");
    guard.get_updates_offsets.push(offset);
    drop(guard);

    let update_ids: &[u32] = match offset {
        -1 => &[42],
        43 => &[43],
        0 => &[41, 42, 43],
        _ => &[],
    };

    let updates = update_ids
        .iter()
        .map(|&update_id| mock_message_update_json(update_id))
        .collect::<Vec<_>>();

    serde_json::json!({
        "ok": true,
        "result": updates,
    })
    .to_string()
}

fn mock_message_update_json(update_id: u32) -> serde_json::Value {
    serde_json::json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "date": 0,
            "chat": {
                "id": 123456,
                "type": "private"
            },
            "from": {
                "id": 123456,
                "is_bot": false,
                "first_name": "tester"
            },
            "text": format!("/music {update_id}")
        }
    })
}

async fn write_json_response(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Option<(String, String)>> {
    let mut request_buffer = Vec::new();
    let mut chunk = [0u8; 1024];

    let header_end = loop {
        let read_size = stream.read(&mut chunk).await?;
        if read_size == 0 {
            return Ok(None);
        }
        request_buffer.extend_from_slice(&chunk[..read_size]);
        if let Some(pos) = find_byte_sequence(&request_buffer, b"\r\n\r\n") {
            break pos;
        }
    };

    let headers = String::from_utf8_lossy(&request_buffer[..header_end]);
    let request_line = headers.lines().next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .map_or_else(|| "/".to_string(), ToString::to_string);

    let content_length = parse_content_length(&headers);
    let body_start = header_end + 4;
    let mut body = if body_start < request_buffer.len() {
        request_buffer[body_start..].to_vec()
    } else {
        Vec::new()
    };

    while body.len() < content_length {
        let read_size = stream.read(&mut chunk).await?;
        if read_size == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read_size]);
    }
    body.truncate(content_length);

    Ok(Some((path, String::from_utf8_lossy(&body).into_owned())))
}

fn parse_request_field_as_i64(path: &str, body: &str, field: &str) -> Option<i64> {
    parse_query_field_as_i64(path, field)
        .or_else(|| {
            url::form_urlencoded::parse(body.as_bytes()).find_map(|(key, value)| {
                if key == field {
                    value.parse().ok()
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
            value.get(field)?.as_i64()
        })
}

fn parse_query_field_as_i64(path: &str, field: &str) -> Option<i64> {
    let query = path.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
        if key == field {
            value.parse().ok()
        } else {
            None
        }
    })
}

fn parse_content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn find_byte_sequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

use super::*;
