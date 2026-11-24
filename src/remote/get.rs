use super::{progress, retry_delay, retry_status, Drive, Entry, ATTEMPTS};
use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt, TryStreamExt};
use reqwest::{
    header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE},
    Client, Response, StatusCode, Url,
};
use serde::Deserialize;
use serde_json::json;
use sha1::{Digest, Sha1};
use std::{io, path::Path};
use tempfile::NamedTempFile;
use tokio::{fs::File, io::AsyncWriteExt};

const CHUNK_SIZE: u64 = 8 * 1024 * 1024;
const CONCURRENCY: usize = 3;

#[derive(Deserialize)]
struct FileDetails {
    size: u64,
    content_hash: Option<String>,
    content_hash_name: Option<String>,
}

#[derive(Deserialize)]
struct DownloadLink {
    url: Url,
}

impl Drive<'_> {
    pub async fn download(&self, entry: &Entry, destination: &Path, force: bool) -> Result<()> {
        if entry.is_dir() {
            bail!("Cannot download a directory with get");
        }
        match destination.symlink_metadata() {
            Ok(_) if !force => bail!(
                "Local destination already exists: {}. Use --force to replace it.",
                destination.display()
            ),
            Ok(metadata) if metadata.is_dir() => {
                bail!(
                    "Local destination is a directory: {}",
                    destination.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("inspect local download destination"),
        }

        let parent = destination
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("create a temporary download in {}", parent.display()))?;
        let details: FileDetails = self
            .call(
                "get",
                json!({"drive_id": self.id, "file_id": entry.file_id}),
            )
            .await?;
        let mut file = File::from_std(temporary.reopen()?);
        let mut hash = Sha1::new();
        if details.size > 0 {
            let link: DownloadLink = self
                .call(
                    "getDownloadUrl",
                    json!({
                        "drive_id": self.id, "file_id": entry.file_id, "expire_sec": 14400
                    }),
                )
                .await?;
            transfer(
                &self.api.http,
                &link.url,
                details.size,
                &mut file,
                &mut hash,
            )
            .await?;
        }
        if let Some(expected) = details.content_hash.as_deref().filter(|s| !s.is_empty()) {
            let is_sha1 = match details.content_hash_name.as_deref() {
                Some(name) => name.eq_ignore_ascii_case("sha1"),
                None => expected.len() == 40 && expected.bytes().all(|c| c.is_ascii_hexdigit()),
            };
            if is_sha1 {
                let actual = format!("{:x}", hash.finalize());
                if !actual.eq_ignore_ascii_case(expected) {
                    bail!("Download SHA-1 mismatch for {:?}", entry.name);
                }
            }
        }

        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let result = if force {
            temporary.persist(destination)
        } else {
            temporary.persist_noclobber(destination)
        };
        result
            .map_err(|err| err.error)
            .with_context(|| format!("save download to {}", destination.display()))?;
        Ok(())
    }
}

async fn transfer(
    http: &Client,
    url: &Url,
    total: u64,
    file: &mut File,
    hash: &mut Sha1,
) -> Result<()> {
    let progress = progress(total)?;
    let first_end = CHUNK_SIZE.min(total) - 1;
    let first = request_range(http, url, 0, first_end).await?;
    if first.status() == StatusCode::OK {
        // Some storage servers ignore Range and return the full file.
        let mut first = first;
        let mut received = 0u64;
        while let Some(bytes) = first.chunk().await.map_err(reqwest::Error::without_url)? {
            received = received.saturating_add(bytes.len() as u64);
            if received > total {
                bail!("Download body exceeds the expected size");
            }
            file.write_all(&bytes).await?;
            hash.update(&bytes);
            progress.set_position(received);
        }
        if received != total {
            bail!("Download ended after {received} bytes, expected {total}");
        }
    } else {
        let first = match read_range(first, 0, first_end, total).await {
            Ok(bytes) => bytes,
            Err(err) if err.downcast_ref::<reqwest::Error>().is_some() => {
                download_range(http, url, 0, first_end, total).await?
            }
            Err(err) => return Err(err),
        };
        file.write_all(&first).await?;
        hash.update(&first);
        progress.inc(first.len() as u64);
        let ranges = ((first_end + 1)..total)
            .step_by(CHUNK_SIZE as usize)
            .map(|start| (start, start.saturating_add(CHUNK_SIZE - 1).min(total - 1)));
        let mut parts = stream::iter(ranges)
            .map(|(start, end)| download_range(http, url, start, end, total))
            .buffered(CONCURRENCY);
        while let Some(bytes) = parts.try_next().await? {
            file.write_all(&bytes).await?;
            hash.update(&bytes);
            progress.inc(bytes.len() as u64);
        }
    }
    progress.finish_and_clear();
    Ok(())
}

async fn request_range(http: &Client, url: &Url, start: u64, end: u64) -> Result<Response> {
    for attempt in 0..ATTEMPTS {
        let response = http
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) if attempt + 1 < ATTEMPTS && retry_status(response.status()) => {}
            Ok(response) => bail!("Download range {start}-{end}: HTTP {}", response.status()),
            Err(_) if attempt + 1 < ATTEMPTS => {}
            Err(err) => return Err(err.without_url()).context("request download range"),
        }
        retry_delay(attempt).await;
    }
    unreachable!("the final attempt returns a result");
}

async fn download_range(
    http: &Client,
    url: &Url,
    start: u64,
    end: u64,
    total: u64,
) -> Result<Vec<u8>> {
    for attempt in 0..ATTEMPTS {
        let response = request_range(http, url, start, end).await?;
        match read_range(response, start, end, total).await {
            Err(err)
                if attempt + 1 < ATTEMPTS && err.downcast_ref::<reqwest::Error>().is_some() =>
            {
                retry_delay(attempt).await;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt returns a result");
}

async fn read_range(mut response: Response, start: u64, end: u64, total: u64) -> Result<Vec<u8>> {
    let expected = format!("bytes {start}-{end}/{total}");
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok());
    if response.status() != StatusCode::PARTIAL_CONTENT || content_range != Some(&expected) {
        bail!("Invalid download response for range {start}-{end}, expected {expected}");
    }
    let length = (end - start + 1) as usize;
    let mut bytes = Vec::with_capacity(length);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        if chunk.len() > length - bytes.len() {
            bail!("Download range {start}-{end} exceeds the expected length");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != length {
        bail!(
            "Download range {start}-{end} ended after {} bytes",
            bytes.len()
        );
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::Api,
        test_server::{json_reply, request_json, TestServer},
    };
    use http_body_util::Full;
    use hyper::{body::Bytes, Response};
    use std::fs;

    #[tokio::test]
    async fn validates_ranges_and_keeps_existing_destination_on_failure() {
        let full = CHUNK_SIZE + 4;
        let server = TestServer::start(move |request, origin| match request.uri().path() {
            "/adrive/v1.0/openFile/get" => json_reply(json!({ "size": full })),
            "/adrive/v1.0/openFile/getDownloadUrl" => {
                let body = request_json(&request);
                json_reply(
                    json!({"url": format!("{origin}/{}", body["file_id"].as_str().unwrap())}),
                )
            }
            "/ok" | "/bad" => {
                assert!(!request.headers().contains_key("authorization"));
                let first = request.headers()["range"] == format!("bytes=0-{}", CHUNK_SIZE - 1);
                let (start, end, bytes) = if first {
                    (
                        0,
                        CHUNK_SIZE - 1,
                        Bytes::from(vec![42; CHUNK_SIZE as usize]),
                    )
                } else {
                    (CHUNK_SIZE, full - 1, Bytes::from_static(b"tail"))
                };
                let end = if !first && request.uri().path() == "/bad" {
                    end + 1
                } else {
                    end
                };
                Response::builder()
                    .status(206)
                    .header("content-range", format!("bytes {start}-{end}/{full}"))
                    .body(Full::new(bytes))
                    .unwrap()
            }
            path => panic!("unexpected request: {path}"),
        })
        .await;
        let api = Api::for_test(&server.origin);
        let drive = Drive::new(&api, "token", "drive");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("saved");
        let mut entry = Entry {
            file_id: "bad".into(),
            name: "file".into(),
            kind: "file".into(),
            size: Some(full),
        };

        fs::write(&path, b"old data").unwrap();
        assert!(drive.download(&entry, &path, false).await.is_err());
        assert!(drive
            .download(&entry, &path, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("range"));
        assert_eq!(fs::read(&path).unwrap(), b"old data");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);

        entry.file_id = "ok".into();
        drive.download(&entry, &path, true).await.unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, full);
        assert!(bytes[..CHUNK_SIZE as usize].iter().all(|byte| *byte == 42));
        assert_eq!(&bytes[CHUNK_SIZE as usize..], b"tail");
    }

    #[tokio::test]
    async fn verifies_full_body_and_sha1_before_publishing() {
        let server = TestServer::start(|request, origin| match request.uri().path() {
            "/adrive/v1.0/openFile/get" => {
                let body = request_json(&request);
                let hash = if body["file_id"] == "ok" {
                    "A9993E364706816ABA3E25717850C26C9CD0D89D"
                } else {
                    "wrong"
                };
                json_reply(json!({ "size": 3, "content_hash_name": "sha1", "content_hash": hash }))
            }
            "/adrive/v1.0/openFile/getDownloadUrl" => {
                json_reply(json!({"url": format!("{origin}/body")}))
            }
            "/body" => Response::new(Full::new(Bytes::from_static(b"abc"))),
            path => panic!("unexpected request: {path}"),
        })
        .await;
        let api = Api::for_test(&server.origin);
        let drive = Drive::new(&api, "token", "drive");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("saved");
        let mut entry = Entry {
            file_id: "bad".into(),
            name: "file".into(),
            kind: "file".into(),
            size: Some(3),
        };
        assert!(drive
            .download(&entry, &path, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("SHA-1"));
        assert!(!path.exists());
        entry.file_id = "ok".into();
        drive.download(&entry, &path, false).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
    }
}
