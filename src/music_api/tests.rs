use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::Duration;

#[cfg(test)]
use super::resize_image_with_padding;
use super::{Album, Artist, MusicApi, SongDetail, SongUrl};
use super::{
    CachePruneStats, SONG_DETAIL_CACHE_TTL, SONG_LYRIC_CACHE_TTL, SONG_URL_CACHE_TTL,
    TimedCacheEntry, cache_entry_is_fresh, format_artists, requests, resize_album_art_to_thumbnail,
    rewrite_media_url, shared, song_url_cache_key,
};
use crate::config::Config;
use crate::error::BotError;
use crate::utils::build_http_client;
use image::{DynamicImage, GenericImageView};

#[derive(Clone, Debug)]
enum MockSongUrlReply {
    OkWithUrl(&'static str),
    OkEmptyUrl,
    ApiCode(i32),
}

#[derive(Debug)]
struct MockMusicApiServerState {
    song_id: u64,
    song_url_sequences: HashMap<u64, VecDeque<MockSongUrlReply>>,
    calls_by_bitrate: HashMap<u64, usize>,
    byradio_referer_seen: bool,
}

impl MockMusicApiServerState {
    fn new(song_id: u64, responses: HashMap<u64, Vec<MockSongUrlReply>>) -> Self {
        let song_url_sequences = responses
            .into_iter()
            .map(|(bitrate, items)| (bitrate, VecDeque::from(items)))
            .collect();
        Self {
            song_id,
            song_url_sequences,
            calls_by_bitrate: HashMap::new(),
            byradio_referer_seen: false,
        }
    }
}

struct MockMusicApiServer {
    base_url: String,
    state: Arc<Mutex<MockMusicApiServerState>>,
    accept_loop_task: JoinHandle<()>,
}

impl MockMusicApiServer {
    async fn start(song_id: u64, responses: HashMap<u64, Vec<MockSongUrlReply>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let address = listener.local_addr().expect("mock server local addr");
        let state = Arc::new(Mutex::new(MockMusicApiServerState::new(song_id, responses)));
        let shared_state = Arc::clone(&state);

        let accept_loop_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection_state = Arc::clone(&shared_state);
                tokio::spawn(async move {
                    let _ = handle_mock_music_api_connection(stream, connection_state).await;
                });
            }
        });

        Self {
            base_url: format!("http://{address}"),
            state,
            accept_loop_task,
        }
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn calls_for_bitrate(&self, bitrate: u64) -> usize {
        let state = self.state.lock().expect("lock mock server state");
        *state.calls_by_bitrate.get(&bitrate).unwrap_or(&0)
    }

    fn saw_byradio_referer(&self) -> bool {
        let state = self.state.lock().expect("lock mock server state");
        state.byradio_referer_seen
    }
}

impl Drop for MockMusicApiServer {
    fn drop(&mut self) {
        self.accept_loop_task.abort();
    }
}

async fn handle_mock_music_api_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<MockMusicApiServerState>>,
) -> std::io::Result<()> {
    let Some((path, headers, request_body)) = read_http_request(&mut stream).await? else {
        return Ok(());
    };

    let body = if path == "/api/song/detail" {
        let song_id = state.lock().expect("lock mock server state").song_id;
        mock_song_detail_response_json(song_id)
    } else if path == "/api/song/enhance/player/url" {
        mock_song_url_response_json(&state, &request_body)
    } else if path == "/api/v6/playlist/detail" {
        mock_playlist_detail_response_json(&state)
    } else if path.starts_with("/api/v1/album/") {
        mock_album_song_response_json(&state)
    } else if path.starts_with("/api/dj/program/detail") {
        let song_id = state.lock().expect("lock mock server state").song_id;
        let program_id = parse_query_field_as_u64(&path, "id").unwrap_or(3_714_760_479);
        mock_program_detail_response_json(song_id, program_id)
    } else if path.starts_with("/api/dj/program/byradio") {
        if parse_header_value(&headers, "referer")
            .is_some_and(|value| value.trim() == "https://music.163.com/")
        {
            let mut guard = state.lock().expect("lock mock server state");
            guard.byradio_referer_seen = true;
        }

        let song_id = state.lock().expect("lock mock server state").song_id;
        let limit =
            parse_query_field_as_u64(&path, "limit").map_or(3, |value| value.max(1) as usize);
        mock_djradio_program_response_json(song_id, limit)
    } else {
        r#"{"code":404}"#.to_string()
    };

    write_json_response(&mut stream, &body).await
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

async fn read_http_request(
    stream: &mut TcpStream,
) -> std::io::Result<Option<(String, String, String)>> {
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

    let body = String::from_utf8_lossy(&body).into_owned();
    Ok(Some((path, headers.into_owned(), body)))
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

fn parse_form_field_as_u64(body: &str, field: &str) -> Option<u64> {
    url::form_urlencoded::parse(body.as_bytes()).find_map(|(k, v)| {
        if k == field {
            v.parse::<u64>().ok()
        } else {
            None
        }
    })
}

fn parse_query_field_as_u64(path: &str, field: &str) -> Option<u64> {
    let query = path.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes()).find_map(|(k, v)| {
        if k == field {
            v.parse::<u64>().ok()
        } else {
            None
        }
    })
}

fn parse_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        if header_name.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn mock_song_detail_response_json(song_id: u64) -> String {
    format!(
        r#"{{"code":200,"songs":[{{"id":{song_id},"name":"Mock Song {song_id}","dt":240000,"ar":[{{"id":1,"name":"Mock Artist"}}],"al":{{"id":2,"name":"Mock Album","picUrl":null}}}}]}}"#
    )
}

fn mock_song_url_response_json(
    state: &Arc<Mutex<MockMusicApiServerState>>,
    request_body: &str,
) -> String {
    let bitrate = parse_form_field_as_u64(request_body, "br").unwrap_or_default();
    let (song_id, reply) = {
        let mut guard = state.lock().expect("lock mock server state");
        let calls = guard.calls_by_bitrate.entry(bitrate).or_insert(0);
        *calls += 1;
        let reply = guard
            .song_url_sequences
            .get_mut(&bitrate)
            .and_then(VecDeque::pop_front)
            .unwrap_or(MockSongUrlReply::ApiCode(598));
        (guard.song_id, reply)
    };

    match reply {
        MockSongUrlReply::OkWithUrl(url) => format!(
            r#"{{"code":200,"data":[{{"id":{song_id},"url":"{url}","br":{bitrate},"size":12345,"md5":"abc","type":"mp3"}}]}}"#
        ),
        MockSongUrlReply::OkEmptyUrl => format!(
            r#"{{"code":200,"data":[{{"id":{song_id},"url":null,"br":{bitrate},"size":12345,"md5":null,"type":null}}]}}"#
        ),
        MockSongUrlReply::ApiCode(code) => {
            format!(r#"{{"code":{code},"data":[]}}"#)
        }
    }
}

fn mock_playlist_detail_response_json(state: &Arc<Mutex<MockMusicApiServerState>>) -> String {
    let song_id = state.lock().expect("lock mock server state").song_id;
    format!(r#"{{"code":200,"playlist":{{"trackIds":[{{"id":{song_id}}}]}}}}"#)
}

fn mock_album_song_response_json(state: &Arc<Mutex<MockMusicApiServerState>>) -> String {
    let song_id = state.lock().expect("lock mock server state").song_id;
    format!(r#"{{"code":200,"songs":[{{"id":{song_id}}}]}}"#)
}

fn mock_program_detail_response_json(song_id: u64, program_id: u64) -> String {
    format!(
        r#"{{"code":200,"program":{{"id":{program_id},"name":"Mock Program {program_id}","mainTrackId":{song_id},"coverUrl":"https://mock.example/program-cover.jpg","dj":{{"nickname":"Mock DJ"}},"radio":{{"name":"Mock Radio"}}}}}}"#
    )
}

fn mock_djradio_program_response_json(song_id: u64, limit: usize) -> String {
    let mut programs = Vec::with_capacity(limit);
    for idx in 0..limit {
        let program_id = 3_714_760_479 + idx as u64;
        let main_track_id = song_id + idx as u64;
        programs.push(format!(
            r#"{{"id":{program_id},"name":"Mock Program {program_id}","mainTrackId":{main_track_id},"coverUrl":"https://mock.example/program-{program_id}.jpg","dj":{{"nickname":"Mock DJ"}},"radio":{{"name":"Mock Radio"}}}}"#
        ));
    }
    let programs_json = programs.join(",");
    format!(r#"{{"code":200,"count":5,"programs":[{programs_json}]}}"#)
}

fn sample_song_detail(song_id: u64) -> SongDetail {
    SongDetail {
        id: song_id,
        name: format!("Sample Song {song_id}"),
        dt: Some(180_000),
        ar: Some(vec![Artist {
            id: 7,
            name: "Sample Artist".to_string(),
        }]),
        al: Some(Album {
            id: 8,
            name: "Sample Album".to_string(),
            pic_url: None,
        }),
    }
}

fn sample_song_url(song_id: u64, bitrate: u64, url: &str) -> SongUrl {
    SongUrl {
        id: song_id,
        url: url.to_string(),
        br: bitrate,
        size: 1_024,
        md5: "md5".to_string(),
        format: "mp3".to_string(),
    }
}

mod cache;
mod core;
mod fallback;
mod formatting;
