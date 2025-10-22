use anyhow::Result;
use colored::Colorize;
use reqwest::Client;
use serde::Deserialize;

use super::ls::FileItem;

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub limit: Option<u32>,
    pub marker: Option<String>,
    pub order_by: Option<String>,
    pub order_direction: Option<String>,
    pub return_total_count: bool,
    pub fetch_all: bool,
}

#[derive(Deserialize, Debug)]
struct SearchResponse {
    items: Vec<FileItem>,
    next_marker: Option<String>,
    total_count: Option<u64>,
}

async fn request_search(
    token: &str,
    drive_id: &str,
    query: &str,
    options: &SearchOptions,
    marker: Option<String>,
) -> Result<SearchResponse> {
    let url = "https://openapi.alipan.com/adrive/v1.0/openFile/search";
    let mut body = serde_json::Map::new();
    body.insert(
        "drive_id".to_string(),
        serde_json::Value::String(drive_id.to_string()),
    );
    body.insert(
        "query".to_string(),
        serde_json::Value::String(query.to_string()),
    );
    body.insert(
        "limit".to_string(),
        serde_json::Value::Number(serde_json::Number::from(options.limit.unwrap_or(100))),
    );
    if let Some(marker) = marker.filter(|m| !m.is_empty()) {
        body.insert("marker".to_string(), serde_json::Value::String(marker));
    }
    if let Some(order_by) = options.order_by.as_deref() {
        let mut clause = order_by.to_string();
        if let Some(direction) = options.order_direction.as_deref() {
            clause.push(' ');
            clause.push_str(direction);
        }
        body.insert("order_by".to_string(), serde_json::Value::String(clause));
    }
    if options.return_total_count {
        body.insert(
            "return_total_count".to_string(),
            serde_json::Value::Bool(true),
        );
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
        anyhow::bail!("Search request failed: {}", text);
    }

    Ok(res.json().await?)
}

pub async fn search_files(
    token: &str,
    drive_id: &str,
    query: &str,
    options: &SearchOptions,
) -> Result<()> {
    let mut marker = options.marker.clone();
    let mut first_page = true;

    loop {
        let resp = request_search(token, drive_id, query, options, marker.clone()).await?;

        if resp.items.is_empty() {
            if first_page {
                println!("{}", "(no results)".dimmed());
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

        if first_page && options.return_total_count {
            if let Some(total) = resp.total_count {
                println!("Total count: {}", total);
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
            if let Some(next) = resp.next_marker.filter(|m| !m.is_empty()) {
                println!("Next marker: {}", next.dimmed());
            }
            break;
        }
    }

    Ok(())
}
