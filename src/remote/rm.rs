use crate::remote::ls::find_file_id_by_name;
use anyhow::Result;
use reqwest::Client;
use serde_json::json;

pub async fn remove_file(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    filename: &str,
) -> Result<()> {
    let file_id = find_file_id_by_name(token, drive_id, parent_file_id, filename).await?;

    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/delete";
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
    if res.status().is_success() {
        println!("🗑️  Deleted '{}'", filename);
        Ok(())
    } else {
        Err(anyhow::anyhow!("Failed to delete: {}", res.text().await?))
    }
}
