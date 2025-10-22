use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;

#[derive(Deserialize)]
struct DriveInfo {
    default_drive_id: String,
}

/// 获取当前用户主盘的 drive_id
pub async fn get_drive_id(token: &str) -> Result<String> {
    let url = "https://openapi.alipan.com/adrive/v1.0/user/getDriveInfo";
    let client = Client::new();
    // ✅ 改为 POST 请求
    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&serde_json::json!({})) // 空 body
        .send()
        .await?;

    if !res.status().is_success() {
        let text = res.text().await?;
        anyhow::bail!("Failed to get drive info: {}", text);
    }

    let info: DriveInfo = res.json().await?;
    Ok(info.default_drive_id)
}
