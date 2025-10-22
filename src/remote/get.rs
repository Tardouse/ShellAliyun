use crate::remote::ls::find_file_id_by_name;
use anyhow::{anyhow, Result};
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{header, Client};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use tokio::time::{sleep, Duration};

const CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 每块8MB
const MAX_CONCURRENCY: usize = 3; // 普通应用限制

/// 从阿里云盘下载文件（带进度条、分段下载、断点续传、403重试）
pub async fn get_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    filename: &str,
    local_path: &Path,
) -> Result<()> {
    let client = Client::new();

    // 1️⃣ 根据文件名查找 file_id
    let file_id = find_file_id_by_name(token, drive_id, parent_file_id, filename).await?;

    // 2️⃣ 获取文件详情（确保知道文件大小）
    let detail_url = "https://openapi.alipan.com/adrive/v1.0/openFile/get";
    let detail_body = json!({
        "drive_id": drive_id,
        "file_id": file_id
    });
    let detail_res = client
        .post(detail_url)
        .bearer_auth(token)
        .json(&detail_body)
        .send()
        .await?;
    let detail_json: serde_json::Value = detail_res.json().await?;
    let total_size = detail_json["size"]
        .as_u64()
        .ok_or_else(|| anyhow!("Can not get file size"))?;

    // 3️⃣ 获取下载链接
    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/getDownloadUrl";
    let body = json!({ "drive_id": drive_id, "file_id": file_id });
    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let v: serde_json::Value = res.json().await?;
    let dl_url = v["url"]
        .as_str()
        .ok_or_else(|| anyhow!("No URL in response"))?
        .to_string();

    // 4️⃣ 进度条
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    // 5️⃣ 打开/创建目标文件（断点续传）
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(local_path)?;
    let downloaded = file.metadata()?.len();
    if downloaded > 0 && downloaded < total_size {
        file.seek(SeekFrom::Start(downloaded))?;
        pb.set_position(downloaded);
    }

    // 6️⃣ 构建下载分片
    let chunks: Vec<(u64, u64)> = (downloaded..total_size)
        .step_by(CHUNK_SIZE as usize)
        .map(|start| {
            let end = std::cmp::min(start + CHUNK_SIZE - 1, total_size - 1);
            (start, end)
        })
        .collect();

    // 7️⃣ 并发分段下载
    let client_ref = &client;
    let pb_ref = &pb;
    let path_ref = local_path.to_path_buf();

    stream::iter(chunks)
        .map(|(start, end)| {
            let client = client_ref.clone();
            let dl_url = dl_url.clone();
            let path = path_ref.clone();
            let pb = pb_ref.clone();

            tokio::spawn(async move {
                let range_header = format!("bytes={}-{}", start, end);
                let mut retry_count = 0;

                loop {
                    let resp = client
                        .get(&dl_url)
                        .header(header::RANGE, &range_header)
                        .send()
                        .await;

                    match resp {
                        Ok(r) if r.status().is_success() || r.status() == 206 => {
                            let bytes = r.bytes().await?;
                            let mut f = OpenOptions::new().write(true).open(&path)?;
                            f.seek(SeekFrom::Start(start))?;
                            f.write_all(&bytes)?;
                            pb.inc(bytes.len() as u64);
                            break;
                        }
                        Ok(r) if r.status().as_u16() == 403 && retry_count < 3 => {
                            eprintln!("403 Concurrent limit, retry after 3 seconds...");
                            retry_count += 1;
                            sleep(Duration::from_secs(3)).await;
                            continue;
                        }
                        Ok(r) => return Err(anyhow!("Download Error: {}", r.status())),
                        Err(e) if retry_count < 3 => {
                            eprintln!("Network Error: {}，retry after 3 seconds...", e);
                            retry_count += 1;
                            sleep(Duration::from_secs(3)).await;
                            continue;
                        }
                        Err(e) => return Err(anyhow!("Download failed: {}", e)),
                    }
                }
                Ok::<(), anyhow::Error>(())
            })
        })
        .buffer_unordered(MAX_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    pb.finish_with_message("✅ Download Complete");
    println!("✅ File Saved to: {}", local_path.display());
    Ok(())
}
