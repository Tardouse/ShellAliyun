use crate::remote::ls::find_file_id_by_name;
use anyhow::Result;
use reqwest::Client;
use serde_json::json;

/// 复制文件到指定目录（支持重命名）。
/// Copy a file on Aliyun Drive into the target folder with an optional new name.
pub async fn copy_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    src_name: &str,
    to_parent_file_id: &str,
    new_name: &str,
) -> Result<()> {
    let src_file_id = find_file_id_by_name(token, drive_id, parent_file_id, src_name).await?;
    let client = Client::new();

    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/copy";
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
        println!("✅ 文件 '{}' 已复制为 '{}'", src_name, new_name);
        Ok(())
    } else {
        Err(anyhow::anyhow!("Failed to copy '{}': {}", src_name, text))
    }
}
