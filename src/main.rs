use clap::Parser;

use std::fmt;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long)]
    user: String,
    #[clap(short, long)]
    pass: String,
}

#[derive(Debug)]
struct LoginError;

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "login failed")
    }
}

impl std::error::Error for LoginError {}

const LOGIN_URL: &str = "https://www.rockclimbing.org/index.php/component/comprofiler/login";
const EVENTS_URL: &str = "https://www.rockclimbing.org/index.php/event-list/events-list";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .user_agent("Mozilla/5.0")
        .build()?;
    let login_params = [("username", args.user), ("passwd", args.pass)];
    let rsp = client.post(LOGIN_URL).form(&login_params).send().await?;
    if !rsp.status().is_success() {
        return Err("login failed".into());
    }
    if rsp.url().path() != "/" {
        return Err("bad username or password".into());
    }

    let rsp = client.get(EVENTS_URL).send().await?;
    let body = rsp.text().await?;
    println!("{:#?}", body);

    Ok(())
}
