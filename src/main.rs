mod input;
mod model;
mod output;

use input::Web;
use output::GCal;

use chrono::Local;
use clap::{AppSettings, ArgEnum, Parser};
use tracing::info;
use tracing_subscriber;

const BASE_URL: &str = "https://www.rockclimbing.org";

#[derive(Copy, Clone, PartialEq, Eq, ArgEnum)]
enum InputType {
    Web,
    YAML,
}

#[derive(Copy, Clone, PartialEq, Eq, ArgEnum)]
enum OutputType {
    #[clap(name = "gcal")]
    GCal,
    YAML,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PipeFile {
    Pipe,
    File(String),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ParsePipeFileError;

impl From<&str> for PipeFile {
    fn from(s: &str) -> Self {
        match s {
            "-" => PipeFile::Pipe,
            _ => PipeFile::File(s.to_string()),
        }
    }
}

#[derive(Parser)]
#[clap(about, version, author)]
#[clap(global_setting(AppSettings::DeriveDisplayOrder))]
struct Args {
    /// Include past events. Without this option, only present (active) and future events will be
    /// sync'd.  With this option, all events (past, present, and future) will be sync'd.  Only
    /// applicable to the web input.
    #[clap(long)]
    all: bool,

    #[clap(arg_enum, short, long, default_value = "web")]
    input: InputType,
    /// The name of the input file to use for the yaml input.
    #[clap(parse(from_str), long = "ifile", default_value = "-")]
    input_file: PipeFile,

    #[clap(arg_enum, short, long, default_value = "gcal")]
    output: OutputType,
    /// The name of the output file to use for the yaml output.
    #[clap(parse(from_str), long = "ofile", default_value = "-")]
    output_file: PipeFile,

    /// Username for the SCMA website (https://rockclimbing.org).
    #[clap(short, long, default_value = "", env = "SCMA_USERNAME")]
    username: String,
    /// Password for the SCMA website (https://rockclimbing.org).
    #[clap(short, long, default_value = "", env = "SCMA_PASSWORD")]
    password: String,

    /// The name of the Google Calendar to sync to.
    #[clap(short, long, default_value = "SCMA Test")]
    calendar: String,
    /// The client secret JSON is downloaded by the user from the Google API console
    /// (https://console.developers.google.com).
    ///
    /// This file contains JSON like '{"installed":{"client_id": ... }}'.
    #[clap(
        long,
        default_value = "client_secret.json",
        env = "GCAL_CLIENT_SECRET_PATH"
    )]
    client_secret_json_path: String,
    /// The token JSON file is created, written, and read by the application to persist the
    /// authentication token.
    #[clap(long, default_value = "token.json", env = "GCAL_OAUTH_TOKEN_JSON_PATH")]
    oauth_token_json_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let min_date = if args.all {
        None
    } else {
        Some(Local::today().naive_local())
    };

    let events = match args.input {
        InputType::Web => {
            Web::new(&args.username, &args.password, BASE_URL, min_date)
                .read()
                .await?
        }
        InputType::YAML => {
            info!(input=?args.input_file, "Reading events");
            let events_yaml = match args.input_file {
                PipeFile::Pipe => todo!(),
                PipeFile::File(path) => std::fs::read_to_string(&path)?,
            };
            serde_yaml::from_str(&events_yaml)?
        }
    };

    match args.output {
        OutputType::GCal => {
            GCal::new(
                &args.calendar,
                &args.client_secret_json_path,
                &args.oauth_token_json_path,
            )
            .write(&events)
            .await?;
        }
        OutputType::YAML => {
            info!(output=?args.output_file, "Writing events");
            match args.output_file {
                PipeFile::Pipe => println!("{}", serde_yaml::to_string(&events)?),
                PipeFile::File(_) => todo!(),
            }
        }
    }

    Ok(())
}
