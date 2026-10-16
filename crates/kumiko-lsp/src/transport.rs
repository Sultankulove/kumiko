//! LSP base protocol: `Content-Length` headers, a blank line, a JSON body.

use std::io;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bodies above this are refused: a broken server must not exhaust memory.
const MAX_MESSAGE: usize = 256 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// The next message; `None` at the end of the stream.
pub async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return if length.is_none() {
                Ok(None)
            } else {
                Err(invalid("stream ended inside headers"))
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue; // tolerate stray blank lines between messages
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let parsed: usize = value
                .trim()
                .parse()
                .map_err(|_| invalid(format!("bad Content-Length `{}`", value.trim())))?;
            if parsed > MAX_MESSAGE {
                return Err(invalid(format!("message of {parsed} bytes")));
            }
            length = Some(parsed);
        }
        // Content-Type and unknown headers are ignored.
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| invalid(format!("bad JSON: {e}")))
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(|e| invalid(e.to_string()))?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn messages_round_trip() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &json!({"jsonrpc": "2.0", "method": "a"}))
            .await
            .unwrap();
        write_message(&mut buffer, &json!({"text": "привет"}))
            .await
            .unwrap();
        let mut reader = BufReader::new(buffer.as_slice());
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Some(json!({"jsonrpc": "2.0", "method": "a"}))
        );
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Some(json!({"text": "привет"}))
        );
        assert_eq!(read_message(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn other_headers_are_ignored_and_bad_ones_fail() {
        let input = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(&input[..]);
        assert_eq!(read_message(&mut reader).await.unwrap(), Some(json!({})));
        let mut reader = BufReader::new(&b"Content-Length: x\r\n\r\n"[..]);
        assert!(read_message(&mut reader).await.is_err());
        let mut reader = BufReader::new(&b"Content-Length: 10\r\n\r\n{}"[..]);
        assert!(read_message(&mut reader).await.is_err(), "truncated body");
    }
}
