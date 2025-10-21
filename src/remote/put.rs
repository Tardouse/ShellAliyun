use anyhow::{anyhow, Result};
use reqwest::{Body, Client};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 每个分片的大小（5 GB 上限，但一般我们用 100 MB）
const PART_SIZE: usize = 100 * 1024 * 1024;

/// 上传文件到阿里云盘（自动分片）
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

    println!(
        "🟢 开始上传文件: {} ({} MB)",
        filename,
        file_size / 1024 / 1024
    );

    // 构造 part_info_list
    let part_count = ((file_size as f64) / (PART_SIZE as f64)).ceil() as usize;
    let part_info_list: Vec<Value> = (1..=part_count)
        .map(|i| json!({ "part_number": i }))
        .collect();

    // Step 1️⃣: 创建上传会话
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
    if let Some(msg) = v.get("code") {
        return Err(anyhow!("创建文件失败: {}", msg));
    }

    let file_id = v["file_id"].as_str().unwrap_or_default().to_string();
    let upload_id = v["upload_id"].as_str().unwrap_or_default().to_string();
    let parts_value = v["part_info_list"].clone();
    let parts = parts_value.as_array().cloned().unwrap_or_default();

    // ✅ 秒传
    if v["rapid_upload"].as_bool().unwrap_or(false) {
        println!("⚡ 文件已秒传，无需上传。");
        return Ok(());
    }

    println!(
        "📦 文件ID: {}, UploadID: {}, 分片数量: {}",
        file_id,
        upload_id,
        parts.len()
    );

    // Step 2️⃣: 上传每个分片
    let mut buf = vec![0u8; PART_SIZE];
    for (i, part) in parts.iter().enumerate() {
        let upload_url = part["upload_url"].as_str().unwrap();
        let part_number = part["part_number"].as_u64().unwrap_or(0);
        let start = (i * PART_SIZE) as u64;
        let end = ((i + 1) * PART_SIZE).min(file_size as usize) as u64;
        let chunk_size = (end - start) as usize;

        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf[..chunk_size])?;

        println!(
            "🚀 上传分片 {}/{} ({} MB)...",
            i + 1,
            parts.len(),
            chunk_size / 1024 / 1024
        );

        let put_res = client
            .put(upload_url)
            .body(Body::from(buf[..chunk_size].to_vec()))
            .send()
            .await?;

        if !put_res.status().is_success() {
            return Err(anyhow!(
                "分片 {} 上传失败: {}",
                part_number,
                put_res.text().await?
            ));
        }
    }

    // Step 3️⃣: 标记上传完成
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

    let status = res2.status(); // ✅ 提前取状态
    let text = res2.text().await?;

    if status.is_success() {
        println!("✅ 上传完成: {}", text);
    } else {
        return Err(anyhow!("上传完成失败: {}", text));
    }

    Ok(())
}
