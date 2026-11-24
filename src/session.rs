use crate::{
    api::Api,
    cli::{Cli, Command},
    completion::ShellCompleter,
    local,
    login::Login,
    remote::{Directory, Drive},
};
use anyhow::{bail, Context, Result};
use clap::{error::ErrorKind, Parser};
use rustyline::{error::ReadlineError, Editor};
use serde_json::json;
use std::{io::IsTerminal, path::PathBuf, sync::Arc};
use tokio::{sync::OnceCell, task::JoinHandle};

pub struct Session {
    api: Arc<Api>,
    login: Login,
    local: PathBuf,
    remote: Directory,
    connection: Option<Arc<Connection>>,
}

struct Connection {
    token: String,
    drive_id: OnceCell<String>,
}

impl Connection {
    async fn drive<'a>(&'a self, api: &'a Api) -> Result<Drive<'a>> {
        let id = self
            .drive_id
            .get_or_try_init(|| Drive::default_id(api, &self.token))
            .await?;
        Ok(Drive::new(api, &self.token, id))
    }
}

impl Session {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: Arc::new(Api::new()?),
            login: Login::default(),
            local: std::env::current_dir().context("read the local working directory")?,
            remote: Directory::default(),
            connection: None,
        })
    }

    pub async fn execute(&mut self, command: Command, in_shell: bool) -> Result<()> {
        match command {
            Command::Login {
                no_browser,
                session_only,
            } => {
                if session_only && !in_shell {
                    bail!("Run 'login --session-only' inside the interactive shell");
                }
                self.login
                    .authorize(&self.api, no_browser, session_only)
                    .await?;
                self.reset_remote();
                eprintln!("Login successful.");
            }
            Command::Logout => {
                self.login.logout()?;
                self.reset_remote();
                eprintln!("Logged out.");
            }
            Command::Pwd => println!("{}", self.remote.display()),
            Command::Lpwd => println!("{}", self.local.display()),
            Command::Lls { path } => {
                let path = path
                    .as_deref()
                    .map_or_else(|| self.local.clone(), |p| local::resolve(&self.local, p));
                local::list(&path)?;
            }
            Command::Lcd { path } => {
                if !in_shell {
                    bail!("'lcd' changes the working directory inside the interactive shell");
                }
                self.local = local::directory(&local::resolve(&self.local, &path))?;
            }
            Command::Cd { .. } if !in_shell => {
                bail!("'cd' changes the working directory inside the interactive shell");
            }
            Command::Exit => {}
            remote => self.execute_remote(remote).await?,
        }
        Ok(())
    }

    async fn execute_remote(&mut self, command: Command) -> Result<()> {
        let connection = self.connection()?;
        let drive = connection.drive(&self.api).await?;
        match command {
            Command::Cd { path } => {
                if path.is_empty() {
                    bail!("The remote directory path is empty");
                }
                self.remote = self.remote.resolve(&drive, &path).await?;
            }
            Command::Ls {
                path,
                page,
                order_by,
            } => {
                let dir = self
                    .remote
                    .resolve(&drive, path.as_deref().unwrap_or("."))
                    .await?;
                drive.list(dir.id(), &page, order_by).await?;
            }
            Command::Search {
                keyword,
                query,
                global,
                path,
                return_total,
                order_by,
                page,
            } => {
                let query = match query {
                    Some(query) => query,
                    None => {
                        let keyword = keyword.context("Supply a nonempty search keyword")?;
                        if keyword.trim().is_empty() {
                            bail!("Supply a nonempty search keyword");
                        }
                        let name = format!("name match {}", json!(keyword));
                        if global {
                            name
                        } else {
                            let dir = self
                                .remote
                                .resolve(&drive, path.as_deref().unwrap_or("."))
                                .await?;
                            let parent_id = dir.id().replace('\\', "\\\\").replace('\'', "\\'");
                            format!("parent_file_id = '{parent_id}' and {name}")
                        }
                    }
                };
                if query.trim().is_empty() {
                    bail!("Supply a nonempty search expression");
                }
                drive.search(&query, &page, order_by, return_total).await?;
            }
            Command::Mkdir { path } => {
                let (parent, name) = self
                    .remote
                    .parent_and_name(&drive, path.trim_end_matches('/'))
                    .await?;
                drive.mkdir(parent.id(), name).await?;
                eprintln!("Created {path:?}");
            }
            Command::Rm { path, permanent } => {
                let (_, entry) = self.remote.entry(&drive, &path).await?;
                drive.remove(&entry.file_id, permanent).await?;
                if self.remote.includes(&entry.file_id) {
                    self.remote = Directory::default();
                }
                let action = if permanent {
                    "Deleted"
                } else {
                    "Moved to recycle bin"
                };
                eprintln!("{action} {path:?}");
            }
            Command::Cp {
                source,
                destination,
            } => {
                let (_, entry) = self.remote.entry(&drive, &source).await?;
                let (dir, name) = self
                    .remote
                    .destination(&drive, &destination, &entry.name)
                    .await?;
                if entry.is_dir() && dir.includes(&entry.file_id) {
                    bail!("Cannot copy a directory into itself or a descendant");
                }
                drive.copy_entry(&entry, dir.id(), name).await?;
                eprintln!("Copied {source:?} to {}/{name:?}", dir.display());
            }
            Command::Mv {
                source,
                destination,
            } => {
                let (parent, entry) = self.remote.entry(&drive, &source).await?;
                let (dir, name) = self
                    .remote
                    .destination(&drive, &destination, &entry.name)
                    .await?;
                if entry.is_dir() && dir.includes(&entry.file_id) {
                    bail!("Cannot move a directory into itself or a descendant");
                }
                if parent.id() != dir.id() || name != entry.name {
                    drive
                        .move_entry(&entry, parent.id(), dir.id(), name)
                        .await?;
                    if self.remote.includes(&entry.file_id) {
                        self.remote = Directory::default();
                    }
                }
                eprintln!("Moved {source:?} to {}/{name:?}", dir.display());
            }
            Command::Put {
                local_file,
                destination,
            } => {
                let path = local::resolve(&self.local, &local_file);
                let (dir, name) = if let Some(destination) = &destination {
                    let fallback = path
                        .file_name()
                        .and_then(|part| part.to_str())
                        .unwrap_or("");
                    self.remote
                        .destination(&drive, destination, fallback)
                        .await?
                } else {
                    let name = path
                        .file_name()
                        .and_then(|part| part.to_str())
                        .context("Use an explicit remote filename for this local path")?;
                    (self.remote.clone(), name)
                };
                if name.is_empty() {
                    bail!("Use an explicit remote filename for this local path");
                }
                let uploaded = drive.upload(dir.id(), name, &path).await?;
                eprintln!("Uploaded to {}/{uploaded:?}", dir.display());
            }
            Command::Get {
                remote_file,
                local_path,
                force,
            } => {
                let (_, entry) = self.remote.entry(&drive, &remote_file).await?;
                let dest = local::download_path(&self.local, local_path.as_deref(), &entry.name)?;
                drive.download(&entry, &dest, force).await?;
                eprintln!("Downloaded to {}", dest.display());
            }
            _ => unreachable!("local commands are handled before connecting to the drive"),
        }
        Ok(())
    }

    fn reset_remote(&mut self) {
        self.remote = Directory::default();
        self.connection = None;
    }

    fn connection(&mut self) -> Result<Arc<Connection>> {
        let token = self.login.token()?;
        if let Some(previous) = &self.connection {
            if previous.token == token {
                return Ok(Arc::clone(previous));
            }
        }
        self.remote = Directory::default();
        let connection = Arc::new(Connection {
            token,
            drive_id: OnceCell::new(),
        });
        self.connection = Some(Arc::clone(&connection));
        Ok(connection)
    }

    pub async fn run_shell(&mut self) -> Result<()> {
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let mut editor = Editor::new()?;
        editor.set_helper(Some(ShellCompleter::new(self.local.clone())));
        let mut completion = None;
        if interactive {
            eprintln!("ShellAliyun  ·  Type 'help' for commands.");
            completion = self.refresh_completion(editor.helper_mut().unwrap());
        }

        loop {
            let prompt = if interactive {
                format!("aliyun:{}> ", self.remote.display())
            } else {
                String::new()
            };
            let line = match editor.readline(&prompt) {
                Ok(line) => line,
                Err(ReadlineError::Interrupted) => continue,
                Err(ReadlineError::Eof) => break,
                Err(err) => return Err(err).context("read shell command"),
            };
            let Some(words) = shlex::split(&line) else {
                eprintln!("Error: unterminated quote or escape");
                continue;
            };
            if words.is_empty() {
                continue;
            }
            editor.add_history_entry(line.as_str())?;
            let command =
                match Cli::try_parse_from(std::iter::once("shellaliyun".to_owned()).chain(words)) {
                    Ok(Cli {
                        command: Some(Command::Exit),
                    }) => break,
                    Ok(Cli {
                        command: Some(command),
                    }) => command,
                    Ok(Cli { command: None }) => continue,
                    Err(err) => {
                        if matches!(
                            err.kind(),
                            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                        ) {
                            err.print()?;
                        } else {
                            eprint!("{err}");
                        }
                        continue;
                    }
                };
            let refresh = matches!(
                &command,
                Command::Cd { .. }
                    | Command::Login { .. }
                    | Command::Logout
                    | Command::Mkdir { .. }
                    | Command::Put { .. }
                    | Command::Cp { .. }
                    | Command::Mv { .. }
                    | Command::Rm { .. }
            );
            tokio::select! {
                result = self.execute(command, true) => {
                    if let Err(err) = result {
                        eprintln!("Error: {err:#}");
                    }
                }
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    eprintln!("Interrupted");
                }
            }
            let helper = editor.helper_mut().unwrap();
            helper.local.clone_from(&self.local);
            if interactive && refresh {
                if let Some(task) = completion.take() {
                    task.abort();
                }
                completion = self.refresh_completion(helper);
            }
        }
        if let Some(task) = completion {
            task.abort();
        }
        Ok(())
    }

    fn refresh_completion(&mut self, completer: &mut ShellCompleter) -> Option<JoinHandle<()>> {
        completer.remote = Arc::default();
        let connection = self.connection().ok()?;
        let api = Arc::clone(&self.api);
        let entries = Arc::clone(&completer.remote);
        let parent = self.remote.id().to_owned();
        Some(tokio::spawn(async move {
            if let Ok(drive) = connection.drive(&api).await {
                if let Ok(list) = drive.entries(&parent).await {
                    let _ = entries.set(list);
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{json_reply, request_json, TestServer};
    use std::sync::Mutex;

    #[tokio::test]
    async fn cli_paths_search_and_mutations_use_expected_api_operations() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let server = TestServer::start(move |request, _| {
            assert_eq!(request.headers()["authorization"], "Bearer test-token");
            let body = request_json(&request);
            let path = request.uri().path();
            let action = path
                .split_once("/openFile/")
                .map_or(path, |(_, action)| action);
            recorded.lock().unwrap().push(action.to_owned());
            let response = match action {
                "/adrive/v1.0/user/getDriveInfo" => json!({"default_drive_id": "drive"}),
                "list" if body["parent_file_id"] == "root" => json!({"items": [
                    {"file_id": "dir", "name": "docs", "type": "folder"},
                    {"file_id": "source", "name": "source", "type": "file"}
                ]}),
                "list" if body["parent_file_id"] == "dir" => json!({"items": []}),
                "create" => {
                    assert_eq!(body["parent_file_id"], "dir");
                    assert_eq!(body["name"], "new folder");
                    assert_eq!(body["type"], "folder");
                    json!({"file_id": "new"})
                }
                "copy" => {
                    assert_eq!(body["file_id"], "source");
                    assert_eq!(body["to_parent_file_id"], "dir");
                    assert_eq!(body["auto_rename"], true);
                    json!({"file_id": "copied", "exist": false})
                }
                "update" => {
                    assert_eq!(body["file_id"], "copied");
                    assert_eq!(body["name"], "copy");
                    json!({"file_id": "copied"})
                }
                "move" => {
                    assert_eq!(body["file_id"], "source");
                    assert_eq!(body["new_name"], "source");
                    assert_eq!(body["to_parent_file_id"], "dir");
                    json!({"file_id": "source"})
                }
                "recyclebin/trash" | "delete" => {
                    assert_eq!(body["file_id"], "source");
                    json!(null)
                }
                "search" => {
                    assert_eq!(
                        body["query"],
                        r#"parent_file_id = 'dir' and name match "a\"b""#
                    );
                    assert_eq!(body["order_by"], "name DESC");
                    if body["marker"].is_null() {
                        assert_eq!(body["return_total_count"], true);
                        json!({"items": [], "total_count": 1, "next_marker": "next"})
                    } else {
                        assert_eq!(body["marker"], "next");
                        assert!(body.get("return_total_count").is_none());
                        json!({"items": [{"file_id": "file", "name": "a\"b", "type": "file"}]})
                    }
                }
                other => panic!("unexpected operation {other}"),
            };
            json_reply(response)
        })
        .await;
        let mut session = Session::new().unwrap();
        session.api = Arc::new(Api::for_test(&server.origin));
        session.login = Login::for_test();
        for args in [
            &["cd", "/root/docs"][..],
            &["mkdir", "./new folder"][..],
            &["cp", "/source", "./copy"][..],
            &["mv", "/source", "."][..],
            &[
                "search",
                "a\"b",
                "--return-total",
                "-a",
                "--order-by",
                "name",
                "--order-direction",
                "desc",
            ][..],
            &["rm", "/source"][..],
            &["rm", "/source", "--permanent"][..],
        ] {
            let cli =
                Cli::try_parse_from(std::iter::once("shellaliyun").chain(args.iter().copied()))
                    .unwrap();
            session.execute(cli.command.unwrap(), true).await.unwrap();
        }
        assert_eq!(session.remote.display(), "/root/docs");
        let calls = calls.lock().unwrap();
        let operations: Vec<_> = calls
            .iter()
            .map(String::as_str)
            .filter(|action| *action != "list")
            .collect();
        assert_eq!(
            operations,
            [
                "/adrive/v1.0/user/getDriveInfo",
                "create",
                "copy",
                "update",
                "move",
                "search",
                "search",
                "recyclebin/trash",
                "delete"
            ]
        );
    }
}
