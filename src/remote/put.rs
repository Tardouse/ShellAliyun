use super::{progress, retry_delay, retry_status, Drive, ATTEMPTS};
use anyhow::{bail, Context, Result};
use futures::TryStreamExt;
use indicatif::ProgressBar;
use reqwest::{header::CONTENT_LENGTH, Body, Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::{io::SeekFrom, path::Path};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};
use tokio_util::io::ReaderStream;

const PART_SIZE: u64 = 500 * 1024 * 1024;
const MAX_PARTS: u64 = 10_000;

#[derive(Deserialize)]
struct Upload {
    file_id: String,
    name: Option<String>,
    file_name: Option<String>,
    upload_id: Option<String>,
    #[serde(default)]
    rapid_upload: bool,
    #[serde(default)]
    part_info_list: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    part_number: u64,
    upload_url: Url,
}

impl Drive<'_> {
    pub async fn upload(&self, parent: &str, name: &str, local: &Path) -> Result<String> {
        let metadata = tokio::fs::metadata(local)
            .await
            .with_context(|| format!("inspect {}", local.display()))?;
        if !metadata.is_file() {
            bail!("Local path is not a regular file: {}", local.display());
        }
        let mut file = File::open(local)
            .await
            .with_context(|| format!("open {}", local.display()))?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            bail!("Local path is not a regular file: {}", local.display());
        }
        let size = metadata.len();
        let count = size.div_ceil(PART_SIZE).max(1);
        if count > MAX_PARTS {
            bail!("File exceeds the upload limit of {MAX_PARTS} parts");
        }

        let mut upload: Upload = self.call("create", json!({
            "drive_id": self.id,
            "parent_file_id": parent,
            "name": name,
            "type": "file",
            "check_name_mode": "auto_rename",
            "size": size,
            "part_info_list": (1..=count).map(|n| json!({"part_number": n})).collect::<Vec<_>>(),
        })).await?;
        if upload.file_id.is_empty() {
            bail!("Create upload returned no file ID");
        }
        if upload.rapid_upload {
            return Ok(upload
                .file_name
                .or(upload.name)
                .unwrap_or_else(|| name.to_owned()));
        }
        let upload_id = upload
            .upload_id
            .filter(|id| !id.is_empty())
            .context("Missing upload ID")?;
        upload
            .part_info_list
            .sort_unstable_by_key(|part| part.part_number);
        let zero_parts = size == 0 && upload.part_info_list.is_empty();
        let expected_parts = upload.part_info_list.len() as u64 == count
            && upload
                .part_info_list
                .iter()
                .zip(1..)
                .all(|(part, n)| part.part_number == n);
        if !zero_parts && !expected_parts {
            bail!("Aliyun Drive returned an incomplete or invalid list of upload parts");
        }

        let progress = progress(size)?;
        for part in upload.part_info_list {
            let offset = (part.part_number - 1) * PART_SIZE;
            send_part(
                &self.api.http,
                &part,
                &mut file,
                offset,
                (size - offset).min(PART_SIZE),
                &progress,
            )
            .await?;
        }
        let after = file.metadata().await?;
        if after.len() != size || after.modified().ok() != metadata.modified().ok() {
            bail!("The local file changed during the upload");
        }
        let completed: serde_json::Value = self
            .call(
                "complete",
                json!({
                    "drive_id": self.id, "file_id": upload.file_id, "upload_id": upload_id,
                }),
            )
            .await?;
        progress.finish_and_clear();
        Ok(completed["name"]
            .as_str()
            .or_else(|| completed["file_name"].as_str())
            .map(str::to_owned)
            .or(upload.file_name)
            .or(upload.name)
            .unwrap_or_else(|| name.to_owned()))
    }
}

async fn send_part(
    http: &Client,
    part: &Part,
    file: &mut File,
    offset: u64,
    length: u64,
    progress: &ProgressBar,
) -> Result<()> {
    for attempt in 0..ATTEMPTS {
        progress.set_position(offset);
        let mut request = http
            .put(part.upload_url.clone())
            .header(CONTENT_LENGTH, length);
        if length > 0 {
            file.seek(SeekFrom::Start(offset)).await?;
            let stream_progress = progress.clone();
            let stream =
                ReaderStream::with_capacity(file.try_clone().await?.take(length), 64 * 1024)
                    .inspect_ok(move |bytes| stream_progress.inc(bytes.len() as u64));
            request = request.body(Body::wrap_stream(stream));
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                if progress.position() != offset + length {
                    bail!("Part {} ended before all bytes were sent", part.part_number);
                }
                return Ok(());
            }
            Ok(response) if attempt + 1 < ATTEMPTS && retry_status(response.status()) => {}
            Ok(response) => bail!(
                "Upload part {}: HTTP {}",
                part.part_number,
                response.status()
            ),
            Err(_) if attempt + 1 < ATTEMPTS => {}
            Err(err) => {
                return Err(err.without_url())
                    .with_context(|| format!("upload part {}", part.part_number))
            }
        }
        retry_delay(attempt).await;
    }
    unreachable!("the final attempt returns a result");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::Api,
        test_server::{json_reply, request_json, TestServer},
    };
    use std::{
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    #[tokio::test]
    async fn retries_stream_and_finalizes_including_empty_files() {
        let sent = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&sent);
        let server = TestServer::start(move |request, origin| {
            match request.uri().path() {
                "/adrive/v1.0/openFile/create" => {
                    let body = request_json(&request);
                    assert_eq!(body["part_info_list"], json!([{"part_number": 1}]));
                    assert_eq!(body["size"], if body["name"] == "empty" { 0 } else { 7 });
                    json_reply(json!({
                        "file_id": body["name"], "upload_id": "upload", "name": "saved",
                        "part_info_list": [{"part_number": 1, "upload_url": format!("{origin}/{}", body["name"].as_str().unwrap())}]
                    }))
                }
                "/file" | "/empty" => {
                    assert_eq!(request.method(), "PUT");
                    assert!(!request.headers().contains_key("authorization"));
                    if request.uri().path() == "/file" {
                        assert_eq!(request.body().as_ref(), b"content");
                        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                            return hyper::Response::builder()
                                .status(503)
                                .body(http_body_util::Full::default())
                                .unwrap();
                        }
                    } else {
                        assert!(request.body().is_empty());
                    }
                    json_reply(json!(null))
                }
                "/adrive/v1.0/openFile/complete" => {
                    assert_eq!(request_json(&request)["upload_id"], "upload");
                    json_reply(json!({"name": "saved"}))
                }
                path => panic!("unexpected request: {path}"),
            }
        }).await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        fs::write(&path, b"content").unwrap();
        let api = Api::for_test(&server.origin);
        let drive = Drive::new(&api, "token", "drive");
        assert_eq!(drive.upload("root", "file", &path).await.unwrap(), "saved");
        assert_eq!(sent.load(Ordering::SeqCst), 2);
        fs::write(&path, []).unwrap();
        assert_eq!(drive.upload("root", "empty", &path).await.unwrap(), "saved");
    }
}
