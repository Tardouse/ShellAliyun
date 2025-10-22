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
use futures::Stream;

/// Each part size (500 MB, up to 5 GB allowed by Aliyun)
const PART_SIZE: usize = 500 * 1024 * 1024;

/// 支持进度追踪的字节流
struct ProgressStream {
    data: Vec<u8>,
    position: usize,
    progress: ProgressBar,
    chunk_size: usize,
}

impl ProgressStream {
    fn new(data: Vec<u8>, progress: ProgressBar) -> Self {
        Self {
            data,
            position: 0,
            progress,
            chunk_size: 8192,
        }
    }
}

impl Stream for ProgressStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.position >= self.data.len() {
            return Poll::Ready(None);
        }

        let end = (self.position + self.chunk_size).min(self.data.len());
        let chunk = self.data[self.position..end].to_vec();
        self.position = end;
        
        // 更新进度条
        self.progress.inc(chunk.len() as u64);

        Poll::Ready(Some(Ok(chunk)))
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

    println!("📦 FileID: {}, UploadID: {}", file_id, upload_id);

    // 2️⃣ 创建全局进度条 - 显示整体上传进度
    let pb = ProgressBar::new(file_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%) | {bytes_per_sec} | ETA: {eta}")
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

        // 读取分片数据
        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; chunk_size];
        file.read_exact(&mut buf)?;

        let mut retry_count = 0;
        loop {
            // 创建进度追踪流
            let stream = ProgressStream::new(buf.clone(), pb.clone());
            let body = Body::wrap_stream(stream);

            let put_res = client
                .put(upload_url)
                .header("Content-Length", chunk_size.to_string())
                .body(body)
                .send()
                .await;

            match put_res {
                Ok(r) if r.status().is_success() => {
                    break;
                }
                Ok(r) if r.status().as_u16() == 403 && retry_count < 3 => {
                    // 回退进度条（因为这次上传失败了）
                    pb.set_position(pb.position().saturating_sub(chunk_size as u64));
                    eprintln!("⚠️ Concurrency limit hit, retrying in 3 s...");
                    retry_count += 1;
                    sleep(Duration::from_secs(3)).await;
                }
                Ok(r) => {
                    pb.set_position(pb.position().saturating_sub(chunk_size as u64));
                    return Err(anyhow!("Part {} upload failed: {}", part_number, r.text().await?));
                }
                Err(e) if retry_count < 3 => {
                    pb.set_position(pb.position().saturating_sub(chunk_size as u64));
                    eprintln!("⚠️ Network error: {}, retrying in 3 s...", e);
                    retry_count += 1;
                    sleep(Duration::from_secs(3)).await;
                }
                Err(e) => {
                    pb.set_position(pb.position().saturating_sub(chunk_size as u64));
                    return Err(anyhow!("Upload failed: {}", e));
                }
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
        println!("🎉 File uploaded successfully!");
        println!("✅ Upload success!");
    } else {
        return Err(anyhow!("Upload completion failed: {}", text));
    }

    Ok(())
}
