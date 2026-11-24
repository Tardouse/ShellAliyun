use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = "Manage Aliyun Drive. Run without a command to enter the interactive shell."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Authorize access to Aliyun Drive
    Login {
        /// Print the authorization URL for use on another device
        #[arg(long)]
        no_browser: bool,
        /// Keep credentials in memory for this interactive session
        #[arg(long)]
        session_only: bool,
    },
    /// Remove the saved login
    Logout,
    /// List a remote directory
    Ls {
        /// Remote directory (defaults to the current directory)
        path: Option<String>,
        #[command(flatten)]
        page: PageArgs,
        /// Sort field
        #[arg(long, value_enum)]
        order_by: Option<ListOrder>,
    },
    /// Search by name or OpenAPI query
    Search {
        #[arg(required_unless_present = "query", conflicts_with = "query")]
        keyword: Option<String>,
        /// Use an OpenAPI search expression
        #[arg(long, conflicts_with_all = ["global", "path"])]
        query: Option<String>,
        /// Search across the drive
        #[arg(long, conflicts_with = "path")]
        global: bool,
        /// Search within this remote directory
        #[arg(long = "in", visible_alias = "path", value_name = "PATH")]
        path: Option<String>,
        /// Ask Aliyun Drive for the total number of matches
        #[arg(long)]
        return_total: bool,
        /// Sort field
        #[arg(long, value_enum)]
        order_by: Option<SearchOrder>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Change the remote working directory in the shell
    Cd { path: String },
    /// Print the remote working directory
    Pwd,
    /// Create a remote directory
    Mkdir { path: String },
    /// List a local directory
    Lls { path: Option<PathBuf> },
    /// Change the local working directory in the shell
    Lcd { path: PathBuf },
    /// Print the local working directory
    Lpwd,
    /// Upload a local file, automatically renaming on conflict
    Put {
        local_file: PathBuf,
        /// Remote directory or new file path
        destination: Option<String>,
    },
    /// Download a remote file to a local path
    Get {
        remote_file: String,
        local_path: Option<PathBuf>,
        /// Replace the destination after the download has been verified
        #[arg(short, long)]
        force: bool,
    },
    /// Copy a remote entry
    Cp { source: String, destination: String },
    /// Move or rename a remote entry
    Mv { source: String, destination: String },
    /// Move a remote entry to the recycle bin
    Rm {
        path: String,
        /// Delete permanently
        #[arg(long)]
        permanent: bool,
    },
    /// Exit the interactive shell
    #[command(visible_alias = "quit")]
    Exit,
}

#[derive(Args)]
pub struct PageArgs {
    /// Maximum items per page
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100), default_value_t = 100)]
    pub limit: u32,
    /// Start at this pagination marker
    #[arg(long)]
    pub marker: Option<String>,
    /// Fetch all remaining pages
    #[arg(short, long)]
    pub all: bool,
    /// Sort direction
    #[arg(long, value_enum, ignore_case = true)]
    pub order_direction: Option<Direction>,
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
pub enum Direction {
    #[serde(rename = "ASC")]
    Asc,
    #[serde(rename = "DESC")]
    Desc,
}

impl Direction {
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum ListOrder {
    CreatedAt,
    UpdatedAt,
    Name,
    Size,
    NameEnhanced,
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum SearchOrder {
    CreatedAt,
    UpdatedAt,
    Name,
    Size,
}

impl SearchOrder {
    pub fn as_api(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::UpdatedAt => "updated_at",
            Self::Name => "name",
            Self::Size => "size",
        }
    }
}
