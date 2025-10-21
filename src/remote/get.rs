use crate::remote::ls::find_file_id_by_name;
use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// 下载文件（使用文件名）
pub async fn get_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    filename: &str,
    local_path: &Path,
) -> Result<()> {
    let file_id = find_file_id_by_name(token, drive_id, parent_file_id, filename).await?;

    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/getDownloadUrl";
    let body = json!({
        "drive_id": drive_id,
        "file_id": file_id
    });

    let client = Client::new();
    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let v: serde_json::Value = res.json().await?;
    let dl_url = v["url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No URL in response"))?;

    let bytes = client.get(dl_url).send().await?.bytes().await?;
    let mut out = File::create(local_path)?;
    out.write_all(&bytes)?;

    println!("✅ 下载完成: {} -> {}", filename, local_path.display());
    Ok(())
}
