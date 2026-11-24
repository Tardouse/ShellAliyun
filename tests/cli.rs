use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

const CLI: &str = env!("CARGO_BIN_EXE_shellaliyun");

#[test]
fn parses_and_rejects_options_before_accessing_the_drive() {
    let help = Command::new(CLI).args(["help", "search"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--return-total"));
    for args in [
        &["ls", "--limit", "0"][..],
        &["search", "abc", "--query", "name = 'abc'"][..],
        &["search", "abc", "--global", "--in", "/"][..],
    ] {
        let invalid = Command::new(CLI).args(args).output().unwrap();
        assert_eq!(invalid.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&invalid.stderr).contains("Not logged in"));
    }
}

#[test]
fn shell_keeps_local_working_directory_and_parses_quoted_paths() {
    let workspace = tempfile::tempdir().unwrap();
    let directory = workspace.path().join("two words");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("result.txt"), "").unwrap();
    let mut shell = Command::new(CLI)
        .current_dir(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    shell
        .stdin
        .take()
        .unwrap()
        .write_all(b"lcd 'two words'\nlls\nlpwd\nput 'unfinished\npwd\nquit\n")
        .unwrap();
    let result = shell.wait_with_output().unwrap();
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "result.txt");
    assert!(lines[1].ends_with("two words"));
    assert_eq!(lines[2], "/root");
    assert!(String::from_utf8_lossy(&result.stderr).contains("unterminated quote"));
}
