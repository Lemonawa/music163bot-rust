/// Shared test infrastructure for mock HTTP servers used across integration tests.
///
/// This module is compiled only in test builds (`#[cfg(test)]`).
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Write a minimal HTTP/1.1 200 JSON response and close the connection.
pub async fn write_json_response(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Find the first occurrence of `needle` in `haystack`, returning its start index.
pub fn find_byte_sequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse the `Content-Length` header value from a raw HTTP header block.
pub fn parse_content_length(headers: &str) -> usize {
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
