use anyhow::Result;
use colored::Colorize;
use rustyline::completion::{Completer, Pair};
use rustyline::Editor;
use rustyline::{Context, Helper};
use shlex::Shlex;
use std::{fs, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

mod login;
mod remote;

use login::{check_login, oauth_login};
use remote::{
    drive::get_drive_id,
    ls::{get_subfolder_id, list_remote_files},
};

#[tokio::main]
async fn main() -> Result<()> {
    println!("{}", "AliyunDrive CLI".bold());
    println!("Type 'help' for commands.");

    // Init Shell
    let mut shell = Shell::new()?;
    shell.run().await
}

struct Shell {
    rl: Editor<AliyunCompleter, rustyline::history::DefaultHistory>,
    local_cwd: PathBuf,
    remote_cwd: String,
    remote_path: String,
    remote_stack: Vec<(String, String)>,
    drive_id_cache: Option<String>,
    completer_remote_cwd: Arc<Mutex<String>>,
}

fn expand_local_path(input: &str) -> PathBuf {
    if input.is_empty() {
        return PathBuf::from(input);
    }

    let mut expanded = input.to_string();
    if let Ok(home) = std::env::var("HOME") {
        if expanded == "~" {
            expanded = home;
        } else if expanded.starts_with("~/") {
            expanded = format!("{}/{}", home, &expanded[2..]);
        } else if expanded.starts_with("$HOME") {
            let suffix = expanded.trim_start_matches("$HOME");
            expanded = format!("{}{}", home, suffix);
        }
    }

    PathBuf::from(expanded)
}

impl Shell {
    fn new() -> Result<Self> {
        let completer_remote_cwd = Arc::new(Mutex::new("root".to_string()));
        let completer = AliyunCompleter::new(Arc::clone(&completer_remote_cwd));
        let mut rl = Editor::<AliyunCompleter, _>::new()?;
        rl.set_helper(Some(completer));

        Ok(Self {
            rl,
            local_cwd: std::env::current_dir()?,
            remote_cwd: "root".to_string(),
            remote_path: "/root".to_string(),
            remote_stack: vec![("/root".to_string(), "root".to_string())],
            drive_id_cache: None,
            completer_remote_cwd,
        })
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            let prompt = format!("{} ", format!("aliyun:{}> ", self.remote_path).blue());
            let line = self.rl.readline(&prompt);
            match line {
                Ok(line) => {
                    let line = line.trim();
                    if !line.is_empty() {
                        self.rl.add_history_entry(line)?;
                    }
                    if let Err(e) = self.dispatch(line).await {
                        eprintln!("{} {}", "Error:".red(), e);
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => continue,
                Err(rustyline::error::ReadlineError::Eof) => {
                    println!("bye");
                    break;
                }
                Err(err) => {
                    eprintln!("Readline error: {}", err);
                    break;
                }
            }
        }
        Ok(())
    }

    async fn dispatch(&mut self, line: &str) -> Result<()> {
        let mut parts = Shlex::new(line).collect::<Vec<_>>();
        if parts.is_empty() {
            return Ok(());
        }

        let cmd = parts.remove(0);
        match cmd.as_str() {
            "help" => self.cmd_help(),
            "exit" | "quit" => {
                println!("Bye.");
                std::process::exit(0);
            }
            "login" => {
                oauth_login().await?;
            }
            "pwd" => println!("{}", self.remote_path),
            "ls" => self.cmd_ls(parts).await?,
            "mkdir" => self.cmd_mkdir(parts).await?,
            "cd" => self.cmd_cd(parts).await?,
            "lls" => self.cmd_lls(parts)?,
            "lcd" => self.cmd_lcd(parts)?,
            "lpwd" => println!("{}", self.local_cwd.display()),
            "put" => self.cmd_put(parts).await?,
            "get" => self.cmd_get(parts).await?,
            "cp" => self.cmd_cp(parts).await?,
            "mv" => self.cmd_mv(parts).await?,
            "rm" => self.cmd_rm(parts).await?,
            _ => println!("Unknown command: {}", cmd),
        }
        Ok(())
    }

    async fn ensure_drive_id(&mut self, token: &str) -> Result<String> {
        if let Some(id) = &self.drive_id_cache {
            Ok(id.clone())
        } else {
            let id = get_drive_id(token).await?;
            self.drive_id_cache = Some(id.clone());
            Ok(id)
        }
    }

    /// Fetch and cache the authenticated token/drive_id pair.
    /// 获取并缓存鉴权所需的 token 与 drive_id。
    async fn ensure_auth(&mut self) -> Result<(String, String)> {
        let token = check_login()?;
        let drive_id = self.ensure_drive_id(&token).await?;
        Ok((token, drive_id))
    }

    /// Keep the auto-completer aware of the current remote folder id.
    /// 同步当前远程目录 ID，供自动补全使用。
    async fn sync_completer_remote_cwd(&self) {
        let mut guard = self.completer_remote_cwd.lock().await;
        *guard = self.remote_cwd.clone();
    }

    /// Handle `ls` command with optional path argument (relative or absolute).
    /// 处理 `ls` 命令，支持可选路径（相对或绝对）。
    async fn cmd_ls(&mut self, args: Vec<String>) -> Result<()> {
        let (token, drive_id) = self.ensure_auth().await?;
        let target = args.get(0).map(|s| s.as_str()).unwrap_or(".");
        let parent_id = if target == "." {
            self.remote_cwd.clone()
        } else {
            self.resolve_remote_parent(&token, &drive_id, target)
                .await?
        };
        list_remote_files(&token, &drive_id, &parent_id).await
    }

    async fn cmd_cd(&mut self, parts: Vec<String>) -> Result<()> {
        if parts.is_empty() {
            return Err(anyhow::anyhow!("Usage: cd <folder>"));
        }
        let target = &parts[0];
        let (token, drive_id) = self.ensure_auth().await?;
        self.navigate_remote_path(&token, &drive_id, target).await?;
        self.sync_completer_remote_cwd().await;
        Ok(())
    }

    fn cmd_help(&self) {
        println!("{}", "Available commands:".blue());
        println!("  login              OAuth2 login");
        println!("  ls [path]         Remote listing (支持相对/绝对路径)");
        println!("  cd <path>         Remote navigation (支持..与绝对路径)");
        println!("  pwd               Show remote cwd");
        println!("  put <file>         Upload file");
        println!("  get <name> [path]  Download file");
        println!("  cp <name> <to>     Copy remote file");
        println!("  mv <name> <to>     Move/rename remote file");
        println!("  rm <name>          Delete remote file");
        println!("  lls / lcd / lpwd   Local file ops");
        println!("  exit / quit        Exit");
    }

    fn cmd_lls(&self, args: Vec<String>) -> Result<()> {
        let path = if args.is_empty() {
            self.local_cwd.clone()
        } else {
            expand_local_path(&args[0])
        };
        for e in fs::read_dir(&path)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if e.file_type()?.is_dir() {
                println!("{}/", name.blue());
            } else {
                println!("{}", name);
            }
        }
        Ok(())
    }

    fn cmd_lcd(&mut self, args: Vec<String>) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("Usage: lcd <dir>"));
        }
        let path = expand_local_path(&args[0]);
        std::env::set_current_dir(&path)?;
        self.local_cwd = std::env::current_dir()?;
        Ok(())
    }

    async fn cmd_put(&mut self, args: Vec<String>) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("Usage: put <local_file>"));
        }
        let (token, drive_id) = self.ensure_auth().await?;
        let local_path = expand_local_path(&args[0]);
        let path_str = local_path.to_string_lossy().to_string();
        remote::put::put_file(&token, &drive_id, &self.remote_cwd, &path_str).await
    }

    async fn cmd_get(&mut self, args: Vec<String>) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("Usage: get <filename> [local_path]"));
        }
        let remote_name = &args[0];
        let local_path = if args.len() >= 2 {
            let provided = expand_local_path(&args[1]);
            if provided.is_dir() {
                provided.join(remote_name)
            } else {
                provided
            }
        } else {
            self.local_cwd.join(remote_name)
        };

        if local_path.exists() {
            return Err(anyhow::anyhow!(
                "Local file already exists: {}",
                local_path.display()
            ));
        }

        if let Some(parent) = local_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(anyhow::anyhow!(
                    "Local directory does not exist: {}",
                    parent.display()
                ));
            }
        }

        let (token, drive_id) = self.ensure_auth().await?;
        remote::get::get_file(
            &token,
            &drive_id,
            &self.remote_cwd,
            remote_name,
            &local_path,
        )
        .await
    }

    async fn cmd_cp(&mut self, args: Vec<String>) -> Result<()> {
        if args.len() < 2 {
            return Err(anyhow::anyhow!("Usage: cp <filename> <target>"));
        }
        let (token, drive_id) = self.ensure_auth().await?;
        let (to_parent, new_name) = self
            .resolve_remote_destination(&token, &drive_id, &args[1], &args[0])
            .await?;
        remote::cp::copy_file(
            &token,
            &drive_id,
            &self.remote_cwd,
            &args[0],
            &to_parent,
            &new_name,
        )
        .await
    }

    async fn cmd_mv(&mut self, args: Vec<String>) -> Result<()> {
        if args.len() < 2 {
            return Err(anyhow::anyhow!("Usage: mv <filename> <target>"));
        }
        let (token, drive_id) = self.ensure_auth().await?;
        let (to_parent, new_name) = self
            .resolve_remote_destination(&token, &drive_id, &args[1], &args[0])
            .await?;
        remote::mv::move_file(
            &token,
            &drive_id,
            &self.remote_cwd,
            &args[0],
            &to_parent,
            &new_name,
        )
        .await
    }

    async fn cmd_rm(&mut self, args: Vec<String>) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("Usage: rm <filename>"));
        }
        let (token, drive_id) = self.ensure_auth().await?;
        remote::rm::remove_file(&token, &drive_id, &self.remote_cwd, &args[0]).await
    }

    async fn cmd_mkdir(&mut self, args: Vec<String>) -> Result<()> {
        if args.is_empty() {
            return Err(anyhow::anyhow!("Usage: mkdir <folder_name>"));
        }
        let folder_name = &args[0];
        let (token, drive_id) = self.ensure_auth().await?;
        remote::mkdir::mkdir(&token, &drive_id, &self.remote_cwd, folder_name).await
    }

    /// Resolve target path to a parent folder id without changing state.
    /// 解析目标路径对应的父级目录 ID（不改变当前状态）。
    async fn resolve_remote_parent(
        &self,
        token: &str,
        drive_id: &str,
        target: &str,
    ) -> Result<String> {
        if target == "." {
            return Ok(self.remote_cwd.clone());
        }
        if target == ".." {
            return Ok(self
                .remote_stack
                .iter()
                .rev()
                .nth(1)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| "root".to_string()));
        }

        if target.starts_with('/') {
            let relative = target.trim_start_matches('/');
            let trimmed = relative
                .trim_start_matches("root/")
                .trim_start_matches("root");
            return remote::ls::resolve_path_to_id(token, drive_id, "root", trimmed).await;
        }

        remote::ls::resolve_path_to_id(token, drive_id, &self.remote_cwd, target).await
    }

    /// Navigate to the target remote folder (relative/absolute, supports `..`).
    /// 导航至目标远程目录（支持相对/绝对路径以及 `..`）。
    async fn navigate_remote_path(
        &mut self,
        token: &str,
        drive_id: &str,
        target: &str,
    ) -> Result<()> {
        let mut new_stack = if target.starts_with('/') {
            vec![("/root".to_string(), "root".to_string())]
        } else {
            self.remote_stack.clone()
        };

        for comp in target.split('/').filter(|c| !c.is_empty()) {
            match comp {
                "." => continue,
                ".." => {
                    if new_stack.len() > 1 {
                        new_stack.pop();
                    } else {
                        println!("{}", "Already at root.".yellow());
                    }
                }
                name => {
                    let (_, current_id) = new_stack
                        .last()
                        .cloned()
                        .unwrap_or_else(|| ("/root".to_string(), "root".to_string()));
                    if let Some(fid) = get_subfolder_id(token, drive_id, &current_id, name).await? {
                        let new_path =
                            if new_stack.last().map(|(p, _)| p == "/root").unwrap_or(true) {
                                format!("/root/{}", name)
                            } else {
                                format!("{}/{}", new_stack.last().unwrap().0, name)
                            };
                        new_stack.push((new_path, fid));
                    } else {
                        anyhow::bail!("Folder not found: {}", name);
                    }
                }
            }
        }

        if let Some((path, id)) = new_stack.last().cloned() {
            self.remote_stack = new_stack;
            self.remote_path = path;
            self.remote_cwd = id;
        }
        Ok(())
    }

    async fn resolve_remote_folder_from_current(
        &self,
        token: &str,
        drive_id: &str,
        path: &str,
    ) -> Result<String> {
        if path.is_empty() {
            return Ok(self.remote_cwd.clone());
        }

        if path == "." {
            return Ok(self.remote_cwd.clone());
        }

        if path == ".." {
            return Ok(self
                .remote_stack
                .iter()
                .rev()
                .nth(1)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| "root".to_string()));
        }

        if path.starts_with('/') {
            let trimmed = path
                .trim_start_matches('/')
                .trim_start_matches("root/")
                .trim_start_matches("root");
            if trimmed.is_empty() {
                return Ok("root".to_string());
            }
            return remote::ls::resolve_path_to_id(token, drive_id, "root", trimmed).await;
        }

        let mut stack: Vec<String> = self.remote_stack.iter().map(|(_, id)| id.clone()).collect();

        for comp in path.split('/') {
            if comp.is_empty() || comp == "." {
                continue;
            }
            if comp == ".." {
                if stack.len() > 1 {
                    stack.pop();
                }
                continue;
            }

            let current_id = stack.last().cloned().unwrap_or_else(|| "root".to_string());
            if let Some(next_id) = get_subfolder_id(token, drive_id, &current_id, comp).await? {
                stack.push(next_id);
            } else {
                anyhow::bail!("Folder not found: {}", comp);
            }
        }

        stack
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Failed to resolve remote path"))
    }

    async fn resolve_remote_destination(
        &self,
        token: &str,
        drive_id: &str,
        target: &str,
        default_name: &str,
    ) -> Result<(String, String)> {
        let trimmed = target.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == "./" {
            return Ok((self.remote_cwd.clone(), default_name.to_string()));
        }

        if trimmed == ".." || trimmed == "../" {
            let parent_id = self
                .remote_stack
                .iter()
                .rev()
                .nth(1)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| "root".to_string());
            return Ok((parent_id, default_name.to_string()));
        }

        if trimmed == "/" || trimmed == "/root" || trimmed == "/root/" {
            return Ok(("root".to_string(), default_name.to_string()));
        }

        let is_dir = trimmed.ends_with('/');
        let path_part = if is_dir {
            trimmed.trim_end_matches('/')
        } else {
            trimmed
        };

        if path_part.is_empty() {
            return Ok((self.remote_cwd.clone(), default_name.to_string()));
        }

        if !is_dir {
            if !path_part.contains('/') {
                return Ok((self.remote_cwd.clone(), path_part.to_string()));
            }
            let idx = path_part
                .rfind('/')
                .ok_or_else(|| anyhow::anyhow!("Invalid target: {}", target))?;
            let parent_path = &path_part[..idx];
            let file_name = &path_part[idx + 1..];
            let parent_id = self
                .resolve_remote_folder_from_current(token, drive_id, parent_path)
                .await?;
            return Ok((parent_id, file_name.to_string()));
        }

        let parent_id = self
            .resolve_remote_folder_from_current(token, drive_id, path_part)
            .await?;
        Ok((parent_id, default_name.to_string()))
    }
}

#[derive(Clone)]
struct AliyunCompleter {
    remote_cwd: Arc<Mutex<String>>,
}

impl AliyunCompleter {
    fn new(remote_cwd: Arc<Mutex<String>>) -> Self {
        Self { remote_cwd }
    }
}

impl Completer for AliyunCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let cmds = vec![
            "ls", "cd", "put", "get", "cp", "mv", "rm", "lls", "lcd", "help", "exit",
        ];
        let input = &line[..pos];
        let mut parts: Vec<&str> = input.split_whitespace().collect();
        let trailing_ws = input
            .chars()
            .last()
            .map(|c| c.is_whitespace())
            .unwrap_or(false);
        if trailing_ws {
            parts.push("");
        }

        if parts.is_empty() {
            return Ok((0, command_pairs("", &cmds)));
        }

        if parts.len() == 1 {
            return Ok((0, command_pairs(parts[0], &cmds)));
        }

        let cmd = parts[0];
        let arg_index = parts.len() - 1;
        let current = *parts.last().unwrap_or(&"");
        let start = input
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        let mut remote_cache: Option<Vec<RemoteEntry>> = None;
        let mut ensure_remote = || {
            if remote_cache.is_none() {
                // 克隆 Arc 以便在新线程中使用
                let remote_cwd_clone = Arc::clone(&self.remote_cwd);
                
                // 在新线程中执行所有操作，避免阻塞主运行时
                if let Ok(result) = std::thread::spawn(move || {
                    // 在新线程中可以安全使用 blocking_lock
                    let parent_id = remote_cwd_clone.blocking_lock().clone();
                    
                    // 创建新的运行时
                    let rt = tokio::runtime::Runtime::new().ok()?;
                    rt.block_on(fetch_remote_entries(parent_id)).ok()
                }).join() {
                    if let Some(entries) = result {
                        remote_cache = Some(entries);
                    }
                }
            }
        };

        let pairs = match cmd {
            "rm" => {
                ensure_remote();
                remote_cache
                    .as_ref()
                    .map(|entries| remote_name_pairs(entries, current, true, true))
                    .unwrap_or_default()
            }
            "get" => {
                if arg_index == 1 {
                    ensure_remote();
                    remote_cache
                        .as_ref()
                        .map(|entries| remote_name_pairs(entries, current, false, true))
                        .unwrap_or_default()
                } else {
                    collect_local_pairs(current)
                }
            }
            "cp" | "mv" => {
                if arg_index == 1 {
                    ensure_remote();
                    remote_cache
                        .as_ref()
                        .map(|entries| remote_name_pairs(entries, current, true, true))
                        .unwrap_or_default()
                } else {
                    ensure_remote();
                    let mut result = remote_cache
                        .as_ref()
                        .map(|entries| remote_name_pairs(entries, current, true, false))
                        .unwrap_or_default();
                    result.extend(special_remote_targets(current));
                    result
                }
            }
            "ls" | "cd" | "mkdir" => {
                ensure_remote();
                let mut result = remote_cache
                    .as_ref()
                    .map(|entries| remote_name_pairs(entries, current, true, true))
                    .unwrap_or_default();
                result.extend(special_remote_targets(current));
                result
            }
            "put" | "lls" | "lcd" => collect_local_pairs(current),
            _ => vec![],
        };

        Ok((start, pairs))
    }
}

use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

impl Helper for AliyunCompleter {}

impl Hinter for AliyunCompleter {
    type Hint = String;
}

impl Highlighter for AliyunCompleter {}

impl Validator for AliyunCompleter {}

#[derive(Clone)]
struct RemoteEntry {
    name: String,
    is_dir: bool,
}

/// Get remote entries in the provided directory for autocompletion.
/// 获取指定远程目录下的所有条目（含文件/文件夹），用于命令自动补全。
async fn fetch_remote_entries(parent_file_id: String) -> Result<Vec<RemoteEntry>> {
    if let Ok(token) = check_login() {
        let drive_id = remote::drive::get_drive_id(&token).await?;
        let url = "https://openapi.alipan.com/adrive/v1.0/openFile/list";
        let body = serde_json::json!({
            "drive_id": drive_id,
            "parent_file_id": parent_file_id
        });

        let client = reqwest::Client::new();
        let res = client
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;
        let v: serde_json::Value = res.json().await?;
        if let Some(items) = v["items"].as_array() {
            let entries = items
                .iter()
                .filter_map(|i| {
                    let name = i["name"].as_str()?;
                    let kind = i["type"].as_str().unwrap_or("");
                    Some(RemoteEntry {
                        name: name.to_string(),
                        is_dir: kind == "folder",
                    })
                })
                .collect::<Vec<_>>();
            return Ok(entries);
        }
    }
    Ok(vec![])
}

fn command_pairs(prefix: &str, commands: &[&str]) -> Vec<Pair> {
    commands
        .iter()
        .filter(|c| c.starts_with(prefix))
        .map(|c| Pair {
            display: c.to_string(),
            replacement: c.to_string(),
        })
        .collect()
}

fn remote_name_pairs(
    entries: &[RemoteEntry],
    prefix: &str,
    include_dirs: bool,
    include_files: bool,
) -> Vec<Pair> {
    if prefix.contains('/') {
        return vec![];
    }

    let mut pairs: Vec<Pair> = entries
        .iter()
        .filter(|entry| {
            (include_dirs || !entry.is_dir)
                && (include_files || entry.is_dir)
                && entry.name.starts_with(prefix)
        })
        .map(|entry| {
            let mut replacement = entry.name.clone();
            if entry.is_dir {
                replacement.push('/');
            }
            Pair {
                display: replacement.clone(),
                replacement,
            }
        })
        .collect();
    pairs.sort_by(|a, b| a.replacement.cmp(&b.replacement));
    pairs
}

fn collect_local_pairs(prefix: &str) -> Vec<Pair> {
    let mut prefix_owned = prefix.to_string();
    if prefix_owned == "~" {
        prefix_owned = "~/".to_string();
    } else if prefix_owned == "$HOME" {
        prefix_owned = "$HOME/".to_string();
    }

    let (dir_part, file_part) = if prefix_owned.ends_with('/') {
        (prefix_owned.clone(), String::new())
    } else if let Some(idx) = prefix_owned.rfind('/') {
        (
            prefix_owned[..idx + 1].to_string(),
            prefix_owned[idx + 1..].to_string(),
        )
    } else {
        (String::new(), prefix_owned.clone())
    };

    let mut dir_for_fs = if dir_part.is_empty() {
        ".".to_string()
    } else {
        dir_part.trim_end_matches('/').to_string()
    };
    if dir_for_fs.is_empty() && dir_part.starts_with('/') {
        dir_for_fs = "/".to_string();
    }

    let expanded_dir = expand_local_path(&dir_for_fs);
    let mut pairs = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&expanded_dir) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !file_part.is_empty() && !name.starts_with(&file_part) {
                continue;
            }
            let mut replacement = format!("{}{}", dir_part, name);
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                replacement.push('/');
            }
            pairs.push(Pair {
                display: replacement.clone(),
                replacement,
            });
        }
    }
    pairs.sort_by(|a, b| a.replacement.cmp(&b.replacement));
    pairs
}

fn special_remote_targets(prefix: &str) -> Vec<Pair> {
    let specials = ["..", "../", ".", "./"];
    specials
        .iter()
        .filter(|s| s.starts_with(prefix))
        .map(|s| Pair {
            display: s.to_string(),
            replacement: s.to_string(),
        })
        .collect()
}
