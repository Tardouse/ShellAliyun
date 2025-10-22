use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Body, Client};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::time::{sleep, Duration};

/// Each part size (500 MB, up to 5 GB allowed by Aliyun)
const PART_SIZE: usize = 500 * 1024 * 1024;

/// A reader wrapper that reports progress as it reads bytes
struct ProgressReader<R: Read> {
    inner: R,
    progress: ProgressBar,
    total_read: u64,
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.total_read += n as u64;
        self.progress.inc(n as u64);
        Ok(n)
    }
}

/// Upload file with real-time progress bar
pub async fn put_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    local_path: &str,
) -> Result<()> {
    let client = Client::new();
    let path = Path::new(local_path);
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid filename"))?
        .to_string_lossy()
        .to_string();

    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();

    println!("🟢 Starting upload: {} ({} MB)", filename, file_size / 1024 / 1024);

    let part_count = ((file_size as f64) / (PART_SIZE as f64)).ceil() as usize;
    let part_info_list: Vec<Value> =
        (1..=part_count).map(|i| json!({ "part_number": i })).collect();

    // 1️⃣ Create upload session
    let create_url = "https://openapi.alipan.com/adrive/v1.0/openFile/create";
    let body = json!({
        "drive_id": drive_id,
        "parent_file_id": parent_file_id,
        "name": filename,
        "type": "file",
        "check_name_mode": "auto_rename",
        "part_info_list": part_info_list,
        "size": file_size,
        "content_hash_name": "sha1",
        "proof_version": "v1"
    });

    let res = client
        .post(create_url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let resp_text = res.text().await?;
    let v: Value = serde_json::from_str(&resp_text)?;
    if let Some(code) = v.get("code") {
        return Err(anyhow!("Failed to create file: {}", code));
    }

    let file_id = v["file_id"].as_str().unwrap_or_default().to_string();
    let upload_id = v["upload_id"].as_str().unwrap_or_default().to_string();
    let parts = v["part_info_list"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    if v["rapid_upload"].as_bool().unwrap_or(false) {
        println!("⚡ Rapid upload detected, skipping transfer.");
        return Ok(());
    }

    println!(
        "📦 FileID: {}, UploadID: {}, Parts: {}",
        file_id, upload_id, parts.len()
    );

    // 2️⃣ Global progress bar
    let pb = ProgressBar::new(file_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    // 3️⃣ Upload parts with live progress
    for (i, part) in parts.iter().enumerate() {
        let upload_url = part["upload_url"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing upload_url"))?;
        let part_number = part["part_number"].as_u64().unwrap_or(0);
        let start = (i * PART_SIZE) as u64;
        let end = ((i + 1) * PART_SIZE).min(file_size as usize) as u64;
        let chunk_size = (end - start) as usize;

        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; chunk_size];
        file.read_exact(&mut buf)?;

        let mut retry_count = 0;
        loop {
            println!(
                "🚀 Uploading part {}/{} ({} MB)...",
                i + 1,
                parts.len(),
                chunk_size / 1024 / 1024
            );

            // Wrap the buffer in a progress-tracking reader
            let reader = ProgressReader {
                inner: std::io::Cursor::new(buf.clone()),
                progress: pb.clone(),
                total_read: 0,
            };
            let body = Body::from(buf.clone());

            let put_res = client.put(upload_url).body(body).send().await;

            match put_res {
                Ok(r) if r.status().is_success() => break,
                Ok(r) if r.status().as_u16() == 403 && retry_count < 3 => {
                    eprintln!("⚠️ Concurrency limit hit, retrying in 3 s...");
                    retry_count += 1;
                    sleep(Duration::from_secs(3)).await;
                }
                Ok(r) => return Err(anyhow!("Part {} upload failed: {}", part_number, r.text().await?)),
                Err(e) if retry_count < 3 => {
                    eprintln!("⚠️ Network error: {}, retrying in 3 s...", e);
                    retry_count += 1;
                    sleep(Duration::from_secs(3)).await;
                }
                Err(e) => return Err(anyhow!("Upload failed: {}", e)),
            }
        }
    }

    pb.finish_with_message("✅ Upload complete, finalizing...");

    // 4️⃣ Complete upload
    let complete_url = "https://openapi.alipan.com/adrive/v1.0/openFile/complete";
    let complete_body = json!({
        "drive_id": drive_id,
        "file_id": file_id,
        "upload_id": upload_id
    });

    let res2 = client
        .post(complete_url)
        .bearer_auth(token)
        .json(&complete_body)
        .send()
        .await?;
    let status = res2.status();
    let text = res2.text().await?;
    if status.is_success() {
        pb.finish_with_message("🎉 File uploaded successfully!");
        println!("✅ Upload success: {}", text);
    } else {
        return Err(anyhow!("Upload completion failed: {}", text));
    }

    Ok(())
}
