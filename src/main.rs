use clap::Parser;

mod input;

use input::Web;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long)]
    username: String,
    #[clap(short, long)]
    password: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let web = Web::new(&args.username, &args.password);
    web.read().await?;

    Ok(())
}
