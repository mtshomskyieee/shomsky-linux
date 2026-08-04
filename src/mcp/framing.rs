use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub async fn write_message<W: AsyncWriteExt + Unpin>(out: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    out.write_all(header.as_bytes()).await?;
    out.write_all(&body).await?;
    out.flush().await?;
    Ok(())
}

pub async fn read_message<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse().context("invalid Content-Length")?);
        }
    }
    let Some(len) = content_length else {
        bail!("MCP message missing Content-Length header");
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let v: Value = serde_json::from_slice(&buf).context("parsing MCP JSON body")?;
    Ok(Some(v))
}
