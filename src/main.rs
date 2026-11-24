mod api;
mod cli;
mod completion;
mod local;
mod login;
mod remote;
mod session;

use anyhow::Result;
use clap::Parser;
use cli::Cli;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let mut session = session::Session::new()?;
    if let Some(command) = cli.command {
        tokio::select! {
            result = session.execute(command, false) => { result?; }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                eprintln!("Interrupted");
                return Ok(ExitCode::from(130));
            }
        }
    } else {
        session.run_shell().await?;
    }
    Ok(ExitCode::SUCCESS)
}

fn terminal_text(text: &str) -> String {
    let mut output = String::new();
    for ch in text.chars() {
        if ch.is_control() {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod test_server;
