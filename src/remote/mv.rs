use crate::remote::ls::find_file_id_by_name;
use anyhow::Result;
use reqwest::Client;
use serde_json::json;

/// 移动或重命名文件到指定目录。
/// Move or rename a file on Aliyun Drive into the destination folder.
pub async fn move_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    src_name: &str,
    to_parent_file_id: &str,
    new_name: &str,
) -> Result<()> {
    let src_file_id = find_file_id_by_name(token, drive_id, parent_file_id, src_name).await?;
    let client = Client::new();

    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/move";
    let body = json!({
        "drive_id": drive_id,
        "file_id": src_file_id,
        "to_parent_file_id": to_parent_file_id,
        "new_name": new_name
    });

    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await?;

    if status.is_success() {
        println!("✅ 文件 '{}' 已移动/重命名为 '{}'", src_name, new_name);
        Ok(())
    } else {
        Err(anyhow::anyhow!("Failed to move '{}': {}", src_name, text))
    }
}
