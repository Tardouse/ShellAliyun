use anyhow::{bail, Context, Result};
use std::{
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

pub fn resolve(cwd: &Path, input: &Path) -> PathBuf {
    if let Some(text) = input.to_str() {
        for prefix in ["~", "$HOME"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                if rest.is_empty() || rest.starts_with(is_separator) {
                    if let Some(dir) = dirs::home_dir() {
                        return dir.join(rest.trim_start_matches(is_separator));
                    }
                }
            }
        }
    }
    cwd.join(input)
}

pub fn is_separator(ch: char) -> bool {
    std::path::is_separator(ch)
}

pub fn list(path: &Path) -> Result<()> {
    let entries = fs::read_dir(path).with_context(|| format!("list {}", path.display()))?;
    let mut entries = entries.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = crate::terminal_text(&entry.file_name().to_string_lossy());
        if entry.path().is_dir() {
            println!("{name}/");
        } else {
            println!("{name}");
        }
    }
    Ok(())
}

pub fn directory(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).with_context(|| format!("open {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("Local path is not a directory: {}", canonical.display());
    }
    Ok(canonical)
}

pub fn download_path(cwd: &Path, provided: Option<&Path>, remote_name: &str) -> Result<PathBuf> {
    let dest = provided.map_or_else(|| cwd.to_owned(), |name| resolve(cwd, name));
    if !dest.is_dir() {
        return Ok(dest);
    }

    let mut components = Path::new(remote_name).components();
    if !matches!(components.next(), Some(Component::Normal(name)) if name == OsStr::new(remote_name))
        || components.next().is_some()
        || (cfg!(windows) && remote_name.contains(':'))
    {
        bail!("Specify a local filename for remote entry {remote_name:?}");
    }
    Ok(dest.join(remote_name))
}
