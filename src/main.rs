mod input;
mod model;
mod output;

use input::Web;
use output::GCal;

use clap::{
    ArgEnum,
    Parser,
};
use tracing::info;
use tracing_subscriber;

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

#[derive(Parser)]
#[clap(about, version, author)]
struct Args {
    #[clap(arg_enum, short, long, default_value="web")]
    input: InputType,
    #[clap(arg_enum, short, long, default_value="gcal")]
    output: OutputType,

    #[clap(short, long, default_value="", env="SCMA_USERNAME")]
    username: String,
    #[clap(short, long, default_value="", env="SCMA_PASSWORD")]
    password: String,

    #[clap(short, long, default_value="-")]
    file: String,

    #[clap(short, long, default_value="SCMA Test")]
    calendar: String,
    #[clap(long, default_value="client_secret.json", env="GCAL_CLIENT_SECRET_PATH")]
    client_secret_json_path: String,
    #[clap(long, default_value="token.json", env="GCAL_OAUTH_TOKEN_JSON_PATH")]
    oauth_token_json_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let events = match args.input {
        InputType::Web => {
            Web::new(&args.username, &args.password)
                .read()
                .await?
        }
        InputType::YAML => {
            info!(%args.file, "Loading events from file");
            let events_yaml = std::fs::read_to_string(&args.file)?;
            serde_yaml::from_str(&events_yaml)?
        }
    };

    match args.output {
        OutputType::GCal => {
            info!("Querying Google Calendar");
            GCal::new(
                &args.calendar,
                &args.client_secret_json_path,
                &args.oauth_token_json_path,
            ).write(&events).await?;
        }
        OutputType::YAML => {
            println!("{}", serde_yaml::to_string(&events)?);
        }
    }

    Ok(())
}
