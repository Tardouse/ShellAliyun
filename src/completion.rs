use crate::{cli::Cli, local, remote::Entry, terminal_text};
use clap::CommandFactory;
use rustyline::{
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
    Context, Helper,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::OnceCell;

pub struct ShellCompleter {
    pub local: PathBuf,
    pub remote: Arc<OnceCell<Vec<Entry>>>,
    commands: clap::Command,
}

impl ShellCompleter {
    pub fn new(local: PathBuf) -> Self {
        let mut commands = Cli::command();
        commands.build();
        Self {
            local,
            remote: Arc::default(),
            commands,
        }
    }

    fn candidates(&self, words: &[String], prefix: &str) -> Vec<String> {
        let Some(command) = words.first() else {
            return self
                .commands
                .get_subcommands()
                .flat_map(|cmd| std::iter::once(cmd.get_name()).chain(cmd.get_visible_aliases()))
                .filter(|name| name.starts_with(prefix))
                .map(str::to_owned)
                .collect();
        };

        let Some(definition) = self.commands.find_subcommand(command) else {
            return Vec::new();
        };
        if prefix.starts_with('-') && !words.iter().any(|word| word == "--") {
            return definition
                .get_arguments()
                .filter_map(|arg| arg.get_long())
                .map(|option| format!("--{option}"))
                .filter(|option| option.starts_with(prefix))
                .collect();
        }

        let index = words
            .iter()
            .skip(1)
            .filter(|word| !word.starts_with('-'))
            .count();
        match (definition.get_name(), index) {
            ("put" | "lls", 0) | ("get", 1) => self.local_paths(prefix, false),
            ("lcd", 0) => self.local_paths(prefix, true),
            ("ls" | "cd", 0) | ("put" | "cp" | "mv", 1) => self.remote_paths(prefix, Some(true)),
            ("get", 0) => self.remote_paths(prefix, Some(false)),
            ("rm" | "cp" | "mv", 0) => self.remote_paths(prefix, None),
            _ => Vec::new(),
        }
    }

    fn local_paths(&self, prefix: &str, directories_only: bool) -> Vec<String> {
        let prefix = match prefix {
            "~" => "~/",
            "$HOME" => "$HOME/",
            _ => prefix,
        };
        let (base, name) = prefix
            .rfind(local::is_separator)
            .map_or(("", prefix), |i| prefix.split_at(i + 1));
        let path = local::resolve(&self.local, Path::new(base));
        let Ok(entries) = fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let is_dir = entry.path().is_dir();
                let filename = entry.file_name().into_string().ok()?;
                (filename.starts_with(name) && (!directories_only || is_dir))
                    .then(|| path_candidate(base, &filename, is_dir))
            })
            .collect()
    }

    fn remote_paths(&self, prefix: &str, directories_only: Option<bool>) -> Vec<String> {
        let (base, name) = if let Some(name) = prefix.strip_prefix("./") {
            ("./", name)
        } else {
            ("", prefix)
        };
        if name.contains('/') {
            return Vec::new();
        }
        let mut paths = Vec::new();
        if directories_only != Some(false) && base.is_empty() {
            paths.extend(
                ["./", "../"]
                    .into_iter()
                    .filter(|path| path.starts_with(name))
                    .map(str::to_owned),
            );
        }
        if let Some(entries) = self.remote.get() {
            paths.extend(entries.iter().filter_map(|entry| {
                let is_dir = entry.is_dir();
                (entry.name.starts_with(name)
                    && directories_only.is_none_or(|wanted| wanted == is_dir))
                .then(|| path_candidate(base, &entry.name, is_dir))
            }));
        }
        paths
    }
}

impl Completer for ShellCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let input = &line[..pos];
        let (start, prefix) = partial_word(input);
        let words = shlex::split(&input[..start]).unwrap_or_default();
        let mut candidates = self.candidates(&words, &prefix);
        candidates.sort_unstable();
        candidates.dedup();
        Ok((
            start,
            candidates
                .into_iter()
                .filter_map(|candidate| {
                    let replacement = shlex::try_quote(&candidate).ok()?.into_owned();
                    Some(Pair {
                        display: terminal_text(&candidate),
                        replacement,
                    })
                })
                .collect(),
        ))
    }
}

impl Helper for ShellCompleter {}
impl Hinter for ShellCompleter {
    type Hint = String;
}
impl Highlighter for ShellCompleter {}
impl Validator for ShellCompleter {}

fn path_candidate(base: &str, name: &str, is_dir: bool) -> String {
    let safe_base = if base.is_empty() && name.starts_with('-') {
        "./"
    } else {
        base
    };
    let suffix = if is_dir { "/" } else { "" };
    format!("{safe_base}{name}{suffix}")
}

fn partial_word(input: &str) -> (usize, String) {
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if matches!(ch, ' ' | '\t' | '\r' | '\n') {
            start = i + ch.len_utf8();
        }
    }
    let mut fragment = input[start..].to_owned();
    if escaped {
        fragment.pop();
    }
    if let Some(quote) = quote {
        fragment.push(quote);
    }
    let prefix = shlex::split(&fragment)
        .and_then(|words| words.into_iter().next())
        .unwrap_or(fragment);
    (start, prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_quoted_and_escaped_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("two words.txt"), "").unwrap();
        let completer = ShellCompleter::new(dir.path().into());
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        for line in ["put 'two wo", "put two\\ wo"] {
            let (start, candidates) = completer.complete(line, line.len(), &ctx).unwrap();
            assert_eq!(start, 4);
            assert_eq!(candidates.len(), 1);
            assert_eq!(
                shlex::split(&candidates[0].replacement).unwrap(),
                ["two words.txt"]
            );
        }
    }
}
