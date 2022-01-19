mod input;
mod model;
mod output;

use input::{EventList, Web};
use model::DateSelect;
use output::GCal;

use clap::{AppSettings, ArgEnum, Parser};
use futures::{stream, StreamExt, TryStreamExt};
use tracing::info;

const BASE_URL: &str = "https://www.rockclimbing.org";
const CONCURRENT_REQUESTS: usize = 3;

#[derive(Copy, Clone, PartialEq, Eq, ArgEnum)]
enum DataType {
    Events,
    Users,
}

#[derive(Copy, Clone, PartialEq, Eq, ArgEnum)]
enum InputType {
    Web,
    Yaml,
}

#[derive(Copy, Clone, PartialEq, Eq, ArgEnum)]
enum OutputType {
    #[clap(name = "gcal")]
    GCal,
    Yaml,
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
    /// Include past events. Without this option, only present (in-progress) and future events will
    /// be sync'd.  With this option, all events (past, present, and future) will be sync'd.  Only
    /// applicable to the web input.
    #[clap(long)]
    all: bool,

    /// The data type to operate on.
    #[clap(arg_enum, short, long, default_value = "events")]
    data: DataType,

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
    #[clap(short, long, default_value = "SCMA")]
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

    match args.data {
        DataType::Events => process_events(args).await,
        DataType::Users => process_users(args).await,
    }
}

async fn process_events(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let dates = if args.all {
        DateSelect::All
    } else {
        DateSelect::NotPast
    };

    match (args.input, args.output) {
        (InputType::Web, OutputType::GCal) => {
            // Handle this case specially to maximize concurrency
            //
            // I've found it difficult to do this in a more general fashion.
            let ((web, events), gcal) = tokio::try_join!(
                web_events(&args.username, &args.password, dates),
                GCal::new(
                    &args.calendar,
                    &args.client_secret_json_path,
                    &args.oauth_token_json_path,
                ),
            )?;

            stream::iter(events)
                .map(|event| scma_to_gcal(event, &web, &gcal))
                .buffer_unordered(CONCURRENT_REQUESTS)
                .try_collect::<Vec<_>>()
                .await?;
        }
        _ => {
            let events = match args.input {
                InputType::Web => {
                    Web::new(&args.username, &args.password, BASE_URL, dates)
                        .await?
                        .read()
                        .await?
                }
                InputType::Yaml => {
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
                    .await?
                    .write(&events)
                    .await?;
                }
                OutputType::Yaml => {
                    info!(output=?args.output_file, "Writing events");
                    match args.output_file {
                        PipeFile::Pipe => println!("{}", serde_yaml::to_string(&events)?),
                        PipeFile::File(_) => todo!(),
                    }
                }
            }
        }
    }

    Ok(())
}

async fn process_users(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let users = match args.input {
        InputType::Web => {
            Web::new(
                &args.username,
                &args.password,
                BASE_URL,
                DateSelect::NotPast,
            )
            .await?
            .fetch_users()
            .await?
        }
        InputType::Yaml => {
            info!(input=?args.input_file, "Reading users");
            let users_yaml = match args.input_file {
                PipeFile::Pipe => todo!(),
                PipeFile::File(path) => std::fs::read_to_string(&path)?,
            };
            serde_yaml::from_str(&users_yaml)?
        }
    };

    match args.output {
        OutputType::GCal => todo!(),
        OutputType::Yaml => {
            info!(output=?args.output_file, "Writing users");
            match args.output_file {
                PipeFile::Pipe => println!("{}", serde_yaml::to_string(&users)?),
                PipeFile::File(_) => todo!(),
            }
        }
    }

    Ok(())
}

async fn scma_to_gcal(
    event: model::Event,
    web: &Web<'_>,
    gcal: &GCal,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = web.fetch_event_details(event).await?;
    gcal.patch_or_insert_event(&event).await
}

async fn web_events<'a>(
    username: &str,
    password: &str,
    dates: DateSelect,
) -> Result<(Web<'a>, EventList), Box<dyn std::error::Error>> {
    let web = Web::new(username, password, BASE_URL, dates).await?;
    let events = web.fetch_events().await?;
    Ok((web, events))
}
