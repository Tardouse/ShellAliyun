use anyhow::Result;
use reqwest::Client;
use serde_json::json;

/// 创建文件夹（mkdir 命令）
pub async fn mkdir(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    folder_name: &str,
) -> Result<()> {
    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/create";

    let body = json!({
        "drive_id": drive_id,
        "parent_file_id": parent_file_id,
        "name": folder_name,
        "check_name_mode": "refuse", // 同名文件夹拒绝创建
        "type": "folder"
    });

    let client = Client::new();
    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await?;

    if status.is_success() {
        println!("📁 文件夹 '{}' 已创建成功", folder_name);
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "❌ 创建文件夹 '{}' 失败: {}",
            folder_name,
            text
        ))
    }
}
