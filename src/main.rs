use scma_gcal_sync::{DateSelect, Event, GAuth, GCal, GPpl, Web};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use futures::{stream, StreamExt, TryStreamExt};
use tracing::info;
use tracing_subscriber::EnvFilter;

use std::collections::HashMap;

const BASE_URL: &str = "https://www.rockclimbing.org";
const CONCURRENT_REQUESTS: usize = 3;

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum DataType {
    Events,
    Users,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum InputType {
    Web,
    Yaml,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum OutputType {
    #[clap(name = "gcal")]
    GCal,
    #[clap(name = "gppl")]
    GPpl,
    Yaml,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum AuthType {
    #[clap(name = "oauth")]
    OAuth,
    ServiceAccount,
    Infer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PipeFile {
    Pipe,
    File(String),
}

impl From<&str> for PipeFile {
    fn from(s: &str) -> Self {
        match s {
            "-" => PipeFile::Pipe,
            _ => PipeFile::File(s.to_string()),
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Boolean {
    True,
    False,
}

impl From<Boolean> for bool {
    fn from(b: Boolean) -> Self {
        match b {
            Boolean::True => true,
            Boolean::False => false,
        }
    }
}

#[derive(Parser)]
#[command(about, version, author)]
struct Cli {
    /// Disables Google API methods that create, modify, or delete.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// The data type to operate on.
    #[arg(value_enum, default_value = "events")]
    data_type: DataType,

    #[arg(value_enum, short, long, default_value = "web")]
    input: InputType,
    /// The name of the input file to use for the yaml input.
    #[arg(long = "ifile", default_value = "-")]
    input_file: PipeFile,

    #[arg(value_enum, short, long, default_value = "gcal")]
    output: OutputType,
    /// The name of the output file to use for the yaml output.
    #[arg(long = "ofile", default_value = "-")]
    output_file: PipeFile,

    /// Username for the SCMA website (https://rockclimbing.org).
    #[arg(help_heading = "Web Input Options")]
    #[arg(short, long, default_value = "", env = "SCMA_USERNAME")]
    username: String,
    /// Password for the SCMA website (https://rockclimbing.org).
    #[arg(help_heading = "Web Input Options")]
    #[arg(short, long, default_value = "", env = "SCMA_PASSWORD")]
    password: String,
    /// Includes past events.
    ///
    /// Without this option, only in-progress and future events will be sync'd.  With this option,
    /// all events (past, in-progress, and future) will be sync'd.
    #[arg(help_heading = "Web Input Options")]
    #[arg(long)]
    all: bool,

    /// The authentication type to use for the Google APIs.
    ///
    /// The Google Calendar output infers `--auth-type service-account`.  The Google People output
    /// infers `--auth-type oauth`.
    #[arg(help_heading = "Google Authentication Options")]
    #[arg(value_enum, long, default_value = "infer")]
    auth_type: AuthType,

    /// Path to the JSON file that contains the client secret.
    ///
    /// This file is downloaded by the user from the Google API console
    /// (https://console.developers.google.com).
    ///
    /// This is used for both the `--auth-type oauth` and `--auth-type service-account`.
    ///
    /// The `--auth-type oauth` JSON looks like: `{"installed":{"client_id": ... }}`.
    ///
    /// The `--auth-type service-account` JSON looks like: `{"type": "service_account", "project_id": ...}`.
    #[arg(help_heading = "Google Authentication Options")]
    #[arg(
        long = "secret-file",
        default_value = "secret.json",
        env = "GOOGLE_CLIENT_SECRET_PATH"
    )]
    client_secret_json_path: String,

    /// Path to the JSON file used to persist the OAuth tokens.
    ///
    /// This file is fully managed (created, written, and read) by the application.
    ///
    /// This is used for the oauth --auth-type only.
    #[arg(help_heading = "Google Authentication Options")]
    #[arg(
        long = "token-file",
        default_value = "token.json",
        env = "GOOGLE_OAUTH_TOKEN_PATH"
    )]
    oauth_token_json_path: String,

    /// The name of the Google Calendar to sync to.
    #[arg(help_heading = "Google Calendar Options")]
    #[arg(short, long, default_value = "SCMA")]
    calendar: String,

    /// Add a user (by email address) as a co-owner of the calendar.
    ///
    /// Use multiple times to specify multiple owners. Useful when using service account
    /// authentication to allow a non-service account to administer the calendar.
    ///
    /// Example: --calendar-owner owner1@example.com --calendar-owner owner2@example.com
    #[arg(help_heading = "Google Calendar Options")]
    #[arg(long = "calendar-owner")]
    calendar_owners: Vec<String>,

    /// A map of email aliases to account for email aliases resolution done by Goolge Calendar.
    ///
    /// A YAML file containing a map of SCMA email addresses to Google email aliases.
    ///
    /// Google Calendar resolves email address aliases.  For example, we might do an acl.insert for
    /// the email user-alias@example.com.  Behind the scenes, Google Calendar resolves the alias
    /// and replaces the email with user@example.com.  This leads to a insert+delete pair on the
    /// next sync.  To prevent this, the email alias resolution must be done before acl.insert.  If
    /// an SCMA email exists as a key in this map, the corresponding value will be used in its
    /// place.
    ///
    /// Example contents:
    ///
    ///  { "user-alias@example.com": "user@example.com", "scma-member-email-address@example.com":
    ///  "google-resolved-email-address@example.com" }
    #[arg(help_heading = "Google Calendar Options")]
    #[arg(long)]
    email_aliases_file: Option<String>,

    /// Disables sending an email notification on ACL insert
    #[arg(help_heading = "Google Calendar Options")]
    #[arg(value_enum, long, default_value = "false")]
    notify_acl_insert: Boolean,

    /// The name of the Google People ContactGroup to sync to.
    #[arg(help_heading = "Google People Options")]
    #[arg(long, default_value = "SCMA")]
    group: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::from_default_env().add_directive("info".parse()?);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    let args = Cli::parse();

    match args.data_type {
        DataType::Events => process_events(args).await,
        DataType::Users => process_users(args).await,
    }
}

async fn auth_from_args(args: &Cli, infer_type: AuthType) -> anyhow::Result<GAuth> {
    let auth_type = match args.auth_type {
        AuthType::Infer => infer_type,
        AuthType::OAuth | AuthType::ServiceAccount => args.auth_type,
    };

    match auth_type {
        AuthType::OAuth => {
            GAuth::with_oauth(&args.client_secret_json_path, &args.oauth_token_json_path).await
        }
        AuthType::ServiceAccount => {
            GAuth::with_service_account(&args.client_secret_json_path).await
        }
        AuthType::Infer => unreachable!("Due to match above"),
    }
}

async fn process_events(args: Cli) -> Result<(), Box<dyn std::error::Error>> {
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
            let auth = auth_from_args(&args, AuthType::ServiceAccount).await?;

            let ((web, events), gcal) = tokio::try_join!(
                web_events(&args.username, &args.password, dates),
                GCal::new(
                    &args.calendar,
                    &args.calendar_owners,
                    auth,
                    args.dry_run,
                    args.notify_acl_insert.into()
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
                        PipeFile::File(ref path) => std::fs::read_to_string(path)?,
                    };
                    serde_yaml::from_str(&events_yaml)?
                }
            };

            match args.output {
                OutputType::GCal => {
                    let auth = auth_from_args(&args, AuthType::ServiceAccount).await?;
                    GCal::new(
                        &args.calendar,
                        &args.calendar_owners,
                        auth,
                        args.dry_run,
                        args.notify_acl_insert.into(),
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
                OutputType::GPpl => unimplemented!(),
            }
        }
    }

    Ok(())
}

async fn process_users(args: Cli) -> Result<(), Box<dyn std::error::Error>> {
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
                PipeFile::File(ref path) => std::fs::read_to_string(path)?,
            };
            serde_yaml::from_str(&users_yaml)?
        }
    };

    match args.output {
        OutputType::GCal => {
            let email_aliases: HashMap<String, String> = match args.email_aliases_file {
                None => HashMap::new(),
                Some(ref path) => {
                    let email_aliases = std::fs::read_to_string(path)
                        .context(format!("unable to read email aliases file `{}`", path))?;
                    serde_yaml::from_str(&email_aliases)
                        .context(format!("unable to parse email aliases file `{}`", path))?
                }
            };

            info!(?email_aliases, "Applying email aliases");
            let emails: Vec<&str> = users
                .iter()
                .map(|user| user.email.as_str())
                .map(|email| email_aliases.get(email).map(AsRef::as_ref).unwrap_or(email))
                .collect();

            let auth = auth_from_args(&args, AuthType::ServiceAccount).await?;
            GCal::new(
                &args.calendar,
                &args.calendar_owners,
                auth,
                args.dry_run,
                args.notify_acl_insert.into(),
            )
            .await?
            .acl_sync(&emails, &args.calendar_owners)
            .await?;
        }
        OutputType::Yaml => {
            info!(output=?args.output_file, "Writing users");
            match args.output_file {
                PipeFile::Pipe => println!("{}", serde_yaml::to_string(&users)?),
                PipeFile::File(_) => todo!(),
            }
        }
        OutputType::GPpl => {
            let auth = auth_from_args(&args, AuthType::OAuth).await?;
            GPpl::new(&args.group, auth, args.dry_run)
                .await?
                .people_sync(users)
                .await?;
        }
    }

    Ok(())
}

async fn scma_to_gcal(
    event: Event,
    web: &Web<'_>,
    gcal: &GCal,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = web.fetch_event_details(event).await?;
    gcal.events_patch_or_insert(&event).await
}

async fn web_events<'a>(
    username: &str,
    password: &str,
    dates: DateSelect,
) -> Result<(Web<'a>, Vec<Event>), Box<dyn std::error::Error>> {
    let web = Web::new(username, password, BASE_URL, dates).await?;
    let events = web.fetch_events().await?;
    Ok((web, events))
}
