use anyhow::{anyhow, Result};
use colored::Colorize;
use reqwest::Client;
use serde::Deserialize;

#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    pub limit: Option<u32>,
    pub marker: Option<String>,
    pub order_by: Option<String>,
    pub order_direction: Option<String>,
    pub fetch_all: bool,
}

#[derive(Deserialize, Debug)]
struct FileListResponse {
    items: Vec<FileItem>,
    next_marker: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FileItem {
    pub name: String,
    pub file_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: Option<u64>,
    #[allow(dead_code)]
    pub updated_at: Option<String>,
}

/// Issue the OpenAPI request and return the full response body.
/// 请求阿里云盘 OpenAPI 并返回完整的响应体。
async fn request_file_list(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    limit: Option<u32>,
    marker: Option<String>,
    order_by: Option<&str>,
    order_direction: Option<&str>,
) -> Result<FileListResponse> {
    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/list";

    let mut body = serde_json::Map::new();
    body.insert(
        "drive_id".to_string(),
        serde_json::Value::String(drive_id.to_string()),
    );
    body.insert(
        "parent_file_id".to_string(),
        serde_json::Value::String(parent_file_id.to_string()),
    );
    body.insert(
        "limit".to_string(),
        serde_json::Value::Number(serde_json::Number::from(limit.unwrap_or(100))),
    );
    body.insert(
        "order_by".to_string(),
        serde_json::Value::String(order_by.unwrap_or("name_enhanced").to_string()),
    );
    body.insert(
        "order_direction".to_string(),
        serde_json::Value::String(order_direction.unwrap_or("ASC").to_string()),
    );

    if let Some(marker) = marker.filter(|m| !m.is_empty()) {
        body.insert("marker".to_string(), serde_json::Value::String(marker));
    }

    let client = Client::new();
    let res = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;

    if !res.status().is_success() {
        let text = res.text().await?;
        anyhow::bail!("Failed to list files: {}", text);
    }

    Ok(res.json().await?)
}

/// 获取远程文件列表并打印结果（带中英提示）。
/// List remote files and print them with bilingual hints.
pub async fn list_remote_files(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    options: &ListOptions,
) -> Result<()> {
    let mut marker = options.marker.clone();
    let mut first_page = true;

    loop {
        let resp = request_file_list(
            token,
            drive_id,
            parent_file_id,
            options.limit,
            marker.clone(),
            options.order_by.as_deref(),
            options.order_direction.as_deref(),
        )
        .await?;

        if resp.items.is_empty() {
            if first_page {
                println!("{}", "(empty)".dimmed());
            }
        } else {
            for item in &resp.items {
                if item.kind == "folder" {
                    println!("{}/", item.name.blue());
                } else {
                    let size = item.size.unwrap_or(0);
                    println!("{:<40} {:>10} bytes", item.name, size);
                }
            }
        }

        first_page = false;

        if options.fetch_all {
            match resp.next_marker.filter(|m| !m.is_empty()) {
                Some(next) => {
                    marker = Some(next);
                    continue;
                }
                None => break,
            }
        } else {
            if let Some(marker) = resp.next_marker.filter(|m| !m.is_empty()) {
                println!("Next marker: {}", marker.dimmed());
            }
            break;
        }
    }

    Ok(())
}

/// 根据文件夹名查找 file_id。
/// Find a subfolder id by its name within the given parent.
pub async fn get_subfolder_id(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    folder_name: &str,
) -> Result<Option<String>> {
    let resp = request_file_list(token, drive_id, parent_file_id, None, None, None, None).await?;
    for item in resp.items {
        if item.kind == "folder" && item.name == folder_name {
            return Ok(Some(item.file_id));
        }
    }

    Ok(None)
}

/// 根据文件名查找 file_id。
/// Find a file id by its display name inside the current directory.
pub async fn find_file_id_by_name(
    token: &str,
    drive_id: &str,
    parent_file_id: &str,
    filename: &str,
) -> Result<String> {
    let resp = request_file_list(token, drive_id, parent_file_id, None, None, None, None).await?;
    for item in resp.items {
        if item.name == filename {
            return Ok(item.file_id);
        }
    }

    Err(anyhow!(
        "File '{}' not found in current directory",
        filename
    ))
}

/// Resolve a nested path into its final folder id (relative to parent).
/// 解析相对路径（或空路径）为最终的文件夹 ID。
pub async fn resolve_path_to_id(
    token: &str,
    drive_id: &str,
    root_parent_id: &str,
    path: &str,
) -> Result<String> {
    if path.is_empty() {
        return Ok(root_parent_id.to_string());
    }

    let mut current_id = root_parent_id.to_string();
    for name in path.split('/') {
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            anyhow::bail!("'..' is not supported in resolve_path_to_id");
        }
        if let Some(id) = get_subfolder_id(token, drive_id, &current_id, name).await? {
            current_id = id;
        } else {
            return Err(anyhow!("路径 '{}' 不存在", path));
        }
    }
    Ok(current_id)
}
