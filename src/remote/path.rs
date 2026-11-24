use super::{Drive, Entry, ROOT_ID};
use anyhow::{bail, Context, Result};

#[derive(Clone, Default)]
pub struct Directory {
    folders: Vec<(String, String)>,
}

impl Directory {
    pub fn display(&self) -> String {
        let mut path = "/root".to_owned();
        for (name, _) in &self.folders {
            path.push('/');
            path.push_str(&crate::terminal_text(name));
        }
        path
    }

    pub fn id(&self) -> &str {
        self.folders.last().map_or(ROOT_ID, |(_, id)| id)
    }

    pub fn includes(&self, id: &str) -> bool {
        self.folders.iter().any(|(_, folder_id)| folder_id == id)
    }

    pub async fn resolve(&self, drive: &Drive<'_>, path: &str) -> Result<Self> {
        let mut result = if path.starts_with('/') {
            Self::default()
        } else {
            self.clone()
        };
        let mut components = path.split('/').filter(|part| !part.is_empty()).peekable();
        if path.starts_with('/') && components.peek() == Some(&"root") {
            components.next();
        }
        for name in components {
            match name {
                "." => {}
                ".." => {
                    result.folders.pop();
                }
                _ => {
                    let folder = drive.find(result.id(), name).await?.with_context(|| {
                        format!("Remote directory component {name:?} was not found in {path:?}")
                    })?;
                    if !folder.is_dir() {
                        bail!("Remote path component {name:?} is a file");
                    }
                    result.folders.push((folder.name, folder.file_id));
                }
            }
        }
        Ok(result)
    }

    pub async fn entry(&self, drive: &Drive<'_>, path: &str) -> Result<(Self, Entry)> {
        let (parent, name) = self
            .parent_and_name(drive, path.trim_end_matches('/'))
            .await?;
        let item = drive
            .find(parent.id(), name)
            .await?
            .with_context(|| format!("Remote path {path:?} was not found"))?;
        if path.ends_with('/') && !item.is_dir() {
            bail!("Remote path {path:?} is a file");
        }
        Ok((parent, item))
    }

    pub async fn parent_and_name<'a>(
        &self,
        drive: &Drive<'_>,
        path: &'a str,
    ) -> Result<(Self, &'a str)> {
        let (parent_path, name) = path.rfind('/').map_or(("", path), |i| path.split_at(i + 1));
        if matches!(name, "" | "." | "..") || is_root_alias(path) {
            bail!("Expected a remote file or directory name in {path:?}");
        }
        Ok((self.resolve(drive, parent_path).await?, name))
    }

    pub async fn destination<'a>(
        &self,
        drive: &Drive<'_>,
        path: &'a str,
        default_name: &'a str,
    ) -> Result<(Self, &'a str)> {
        if path.is_empty() {
            bail!("Destination path is empty");
        }
        let last = path.rsplit('/').next().unwrap_or_default();
        if matches!(last, "" | "." | "..") || is_root_alias(path) {
            return Ok((self.resolve(drive, path).await?, default_name));
        }
        let (mut parent, name) = self.parent_and_name(drive, path).await?;
        if let Some(item) = drive.find(parent.id(), name).await? {
            if item.is_dir() {
                parent.folders.push((item.name, item.file_id));
                return Ok((parent, default_name));
            }
        }
        Ok((parent, name))
    }
}

fn is_root_alias(path: &str) -> bool {
    path.starts_with('/') && path.trim_matches('/') == "root"
}
