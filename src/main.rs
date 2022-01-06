mod input;
mod model;

use input::Web;

use clap::Parser;
use tracing_subscriber;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long, env="SCMA_USERNAME")]
    username: String,
    #[clap(short, long, env="SCMA_PASSWORD")]
    password: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let events = Web::new(&args.username, &args.password)
        .read()
        .await?;

    println!("{}", serde_yaml::to_string(&events)?);

    Ok(())
}
