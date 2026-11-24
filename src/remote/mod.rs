mod get;
mod path;
mod put;

pub use path::Directory;

use crate::{
    api::Api,
    cli::{Direction, ListOrder, PageArgs, SearchOrder},
};
use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

pub const ROOT_ID: &str = "root";
const PREFIX: &str = "/adrive/v1.0/openFile/";

pub struct Drive<'a> {
    api: &'a Api,
    token: &'a str,
    id: &'a str,
}

impl<'a> Drive<'a> {
    pub fn new(api: &'a Api, token: &'a str, id: &'a str) -> Self {
        Self { api, token, id }
    }

    pub async fn default_id(api: &Api, token: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Info {
            default_drive_id: String,
        }
        let info: Info = api
            .post("/adrive/v1.0/user/getDriveInfo", token, &json!({}))
            .await?;
        if info.default_drive_id.is_empty() {
            bail!("Aliyun Drive returned an empty default drive ID");
        }
        Ok(info.default_drive_id)
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, action: &str, body: Value) -> Result<T> {
        self.api
            .post(&format!("{PREFIX}{action}"), self.token, &body)
            .await
    }

    pub async fn list(
        &self,
        parent: &str,
        options: &PageArgs,
        order: Option<ListOrder>,
    ) -> Result<()> {
        let mut body = self.list_body(parent);
        body["order_by"] = json!(order.unwrap_or(ListOrder::NameEnhanced));
        body["order_direction"] = json!(options.order_direction.unwrap_or(Direction::Asc));
        self.print_pages("list", body, options, false, "(empty)")
            .await
    }

    pub async fn search(
        &self,
        query: &str,
        options: &PageArgs,
        order: Option<SearchOrder>,
        return_total: bool,
    ) -> Result<()> {
        let mut body = json!({"drive_id": self.id, "query": query});
        if let Some(order) = order {
            let direction = options.order_direction.unwrap_or(Direction::Asc);
            body["order_by"] = json!(format!("{} {}", order.as_api(), direction.as_api()));
        } else if options.order_direction.is_some() {
            bail!("For search, --order-direction requires --order-by");
        }
        if return_total {
            body["return_total_count"] = json!(true);
        }
        self.print_pages("search", body, options, return_total, "(no results)")
            .await
    }

    fn list_body(&self, parent: &str) -> Value {
        json!({"drive_id": self.id, "parent_file_id": parent, "limit": 100})
    }

    async fn print_pages(
        &self,
        action: &str,
        mut body: Value,
        options: &PageArgs,
        return_total: bool,
        empty_message: &str,
    ) -> Result<()> {
        body["limit"] = json!(options.limit);
        let mut pages = Pages::new(options.marker.as_deref());
        let mut has_items = false;
        while let Some(page) = pages.next(self, action, &body).await? {
            for item in page.items {
                has_items = true;
                let name = crate::terminal_text(&item.name);
                if item.is_dir() {
                    println!("{name}/");
                } else if let Some(size) = item.size {
                    println!("{name}\t{size} bytes");
                } else {
                    println!("{name}");
                }
            }
            if return_total {
                if let Some(total) = page.total_count {
                    eprintln!("Total count: {total}");
                }
            }
            if !options.all {
                if let Some(marker) = pages.marker.as_deref() {
                    eprintln!("Next marker: {}", crate::terminal_text(marker));
                }
                break;
            }
            body.as_object_mut().unwrap().remove("return_total_count");
        }
        if !has_items {
            eprintln!("{empty_message}");
        }
        Ok(())
    }

    pub async fn entries(&self, parent: &str) -> Result<Vec<Entry>> {
        let mut pages = Pages::new(None);
        let body = self.list_body(parent);
        let mut entries = Vec::new();
        while let Some(page) = pages.next(self, "list", &body).await? {
            entries.extend(page.items);
        }
        Ok(entries)
    }

    pub async fn find(&self, parent: &str, name: &str) -> Result<Option<Entry>> {
        let mut pages = Pages::new(None);
        let body = self.list_body(parent);
        let mut found = None;
        while let Some(page) = pages.next(self, "list", &body).await? {
            for item in page.items.into_iter().filter(|item| item.name == name) {
                if found.is_some() {
                    bail!("Multiple remote entries are named {name:?} in the same directory");
                }
                found = Some(item);
            }
        }
        Ok(found)
    }

    pub async fn mkdir(&self, parent: &str, name: &str) -> Result<()> {
        self.call::<Value>(
            "create",
            json!({
                "drive_id": self.id, "parent_file_id": parent, "name": name,
                "type": "folder", "check_name_mode": "refuse",
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn remove(&self, id: &str, permanent: bool) -> Result<()> {
        let action = if permanent {
            "delete"
        } else {
            "recyclebin/trash"
        };
        self.call::<Value>(action, json!({"drive_id": self.id, "file_id": id}))
            .await?;
        Ok(())
    }

    pub async fn move_entry(
        &self,
        entry: &Entry,
        source_parent: &str,
        parent: &str,
        name: &str,
    ) -> Result<()> {
        if source_parent == parent && entry.name == name {
            return Ok(());
        }
        self.require_free_name(parent, name).await?;
        if source_parent == parent {
            self.rename(&entry.file_id, name).await?;
        } else {
            let response: Value = self
                .call(
                    "move",
                    json!({
                        "drive_id": self.id, "file_id": entry.file_id,
                        "to_parent_file_id": parent, "new_name": name, "check_name_mode": "refuse",
                    }),
                )
                .await?;
            check_name_conflict(&response, name)?;
        }
        Ok(())
    }

    pub async fn copy_entry(&self, entry: &Entry, parent: &str, name: &str) -> Result<()> {
        self.require_free_name(parent, name).await?;
        let rename = name != entry.name;
        let copy: Value = self
            .call(
                "copy",
                json!({
                    "drive_id": self.id, "file_id": entry.file_id, "to_parent_file_id": parent,
                    "auto_rename": rename,
                }),
            )
            .await?;
        check_name_conflict(&copy, name)?;
        if rename {
            let id = copy["file_id"]
                .as_str()
                .filter(|id| !id.is_empty() && *id != entry.file_id)
                .context(
                    "Copy accepted but Aliyun Drive did not return a new file ID for renaming",
                )?;
            self.rename(id, name).await.with_context(|| {
                format!("Copied entry {id:?}, but could not rename it to {name:?}")
            })?;
        }
        Ok(())
    }

    async fn require_free_name(&self, parent: &str, name: &str) -> Result<()> {
        if self.find(parent, name).await?.is_some() {
            bail!("A remote entry named {name:?} already exists at the destination");
        }
        Ok(())
    }

    async fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.call::<Value>(
            "update",
            json!({
                "drive_id": self.id, "file_id": id, "name": name, "check_name_mode": "refuse",
            }),
        )
        .await?;
        Ok(())
    }
}

fn check_name_conflict(response: &Value, name: &str) -> Result<()> {
    if response["exist"].as_bool() == Some(true) {
        bail!("Aliyun Drive reported an existing entry for {name:?}");
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Entry {
    pub name: String,
    pub file_id: String,
    #[serde(rename = "type")]
    kind: String,
    size: Option<u64>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == "folder"
    }
}

#[derive(Deserialize)]
struct Page {
    items: Vec<Entry>,
    next_marker: Option<String>,
    total_count: Option<u64>,
}

struct Pages {
    marker: Option<String>,
    seen: HashSet<String>,
    done: bool,
}

impl Pages {
    fn new(marker: Option<&str>) -> Self {
        let marker = marker.filter(|m| !m.is_empty()).map(str::to_owned);
        Self {
            seen: marker.iter().cloned().collect(),
            marker,
            done: false,
        }
    }

    async fn next(
        &mut self,
        drive: &Drive<'_>,
        action: &str,
        body: &Value,
    ) -> Result<Option<Page>> {
        if self.done {
            return Ok(None);
        }
        let mut body = body.clone();
        if let Some(marker) = &self.marker {
            body["marker"] = json!(marker);
        }
        let page: Page = drive.call(action, body).await?;
        self.marker = page
            .next_marker
            .as_deref()
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        if let Some(next) = &self.marker {
            if !self.seen.insert(next.clone()) {
                bail!("Aliyun Drive repeated a pagination marker");
            }
        } else {
            self.done = true;
        }
        Ok(Some(page))
    }
}

fn progress(size: u64) -> Result<ProgressBar> {
    let progress = ProgressBar::new(size);
    progress.set_style(
        ProgressStyle::with_template(
            "{bar:36.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
        )?
        .progress_chars("=>-"),
    );
    Ok(progress)
}

const ATTEMPTS: usize = 4;

fn retry_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
        )
}

async fn retry_delay(attempt: usize) {
    tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{json_reply, request_json, TestServer};

    #[tokio::test]
    async fn resolves_later_pages_and_remote_path_components() {
        let server = TestServer::start(|request, _| {
            assert_eq!(request.headers()["authorization"], "Bearer test-token");
            let body = request_json(&request);
            let reply = match (body["parent_file_id"].as_str(), body["marker"].as_str()) {
                (Some("root"), None) => json!({"items": [], "next_marker": "next"}),
                (Some("root"), Some("next")) => json!({"items": [
                    {"file_id": "a", "name": "rooted", "type": "folder"},
                    {"file_id": "b", "name": "root", "type": "folder"}
                ]}),
                (Some("a"), None) => json!({"items": [
                    {"file_id": "c", "name": "照片.jpg", "type": "file"}
                ]}),
                _ => panic!("unexpected list request: {body}"),
            };
            json_reply(reply)
        })
        .await;
        let api = Api::for_test(&server.origin);
        let drive = Drive::new(&api, "test-token", "test-drive");
        let root = Directory::default();
        let dir = root.resolve(&drive, "/root/rooted").await.unwrap();
        assert_eq!(dir.id(), "a");
        let file = dir.entry(&drive, "../rooted/照片.jpg").await.unwrap().1;
        assert_eq!(file.file_id, "c");
        assert!(dir.resolve(&drive, "照片.jpg").await.is_err());
        assert_eq!(dir.resolve(&drive, "/root").await.unwrap().id(), ROOT_ID);
        assert_eq!(dir.resolve(&drive, "/root/root").await.unwrap().id(), "b");
        let (parent, name) = dir
            .destination(&drive, "/照片.jpg", "ignored")
            .await
            .unwrap();
        assert_eq!(parent.id(), ROOT_ID);
        assert_eq!(name, "照片.jpg");
    }

    #[tokio::test]
    async fn repeated_marker_and_ambiguous_names_are_errors() {
        let server = TestServer::start(|request, _| {
            let body = request_json(&request);
            if body["parent_file_id"] == "duplicate" {
                json_reply(json!({"items": [
                    {"file_id": "a", "name": "same", "type": "file"},
                    {"file_id": "b", "name": "same", "type": "folder"}
                ]}))
            } else {
                json_reply(json!({"items": [], "next_marker": "loop"}))
            }
        })
        .await;
        let api = Api::for_test(&server.origin);
        let drive = Drive::new(&api, "test", "drive");
        let loop_error = drive.entries(ROOT_ID).await.err().unwrap();
        assert!(loop_error.to_string().contains("repeated"));
        let ambiguous = drive.find("duplicate", "same").await.err().unwrap();
        assert!(ambiguous.to_string().contains("Multiple"));
    }
}
