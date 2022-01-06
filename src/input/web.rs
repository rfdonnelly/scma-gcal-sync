use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{
    Class,
    Name,
};
use tracing::info;

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

const BASE_URL: &str = "https://www.rockclimbing.org";
const LOGIN_URL: &str = "https://www.rockclimbing.org/index.php/component/comprofiler/login";
const EVENTS_URL: &str = "https://www.rockclimbing.org/index.php/event-list/events-list";
const CONCURRENT_REQUESTS: usize = 3;

pub struct Web<'a> {
    username: &'a str,
    password: &'a str,
}

impl<'a> Web<'a> {
    pub fn new(username: &'a str, password: &'a str) -> Self {
        Self {
            username: username,
            password: password,
        }
    }

    pub async fn read(&self) -> Result<(), Box<dyn std::error::Error>> {
        let client = create_client()?;
        login(&client, self.username, self.password).await?;

        info!("Fetching event list page {}", EVENTS_URL);
        let rsp = client.get(EVENTS_URL).send().await?;
        let text = rsp.text().await?;
        let events_page = EventListPage::from_str(&text)?;
        info!("Parsed events\n{}", events_page);
        let urls = events_page.event_links;
        let events: Result<Vec<Event>, Box<dyn std::error::Error>> = stream::iter(urls)
            .map(|url| {
                let client = &client;
                async move {
                    info!("Fetching event from {}", url);
                    let rsp = client.get(&url).send().await?;
                    let text = rsp.text().await?;
                    info!("Fetched event from {}", url);
                    Event::from_str(&text, url)
                }
            })
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect().await;

        info!("Parsed events {:?}", events);

        Ok(())
    }
}

fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    Ok(
        reqwest::Client::builder()
            .cookie_store(true)
            .user_agent("Mozilla/5.0")
            .build()?
    )
}

async fn login<S>(client: &reqwest::Client, username: S, password: S) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsRef<str>
{
    info!("Logging into {}", LOGIN_URL);

    let login_params = [("username", username.as_ref()), ("passwd", password.as_ref())];
    let rsp = client.post(LOGIN_URL).form(&login_params).send().await?;

    if !rsp.status().is_success() {
        Err("login failed".into())
    } else if rsp.url().path() != "/" {
        Err("bad username or password".into())
    } else {
        Ok(())
    }
}

struct EventListPage {
    event_links: HashSet<String>,
}

impl fmt::Display for EventListPage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for link in &self.event_links {
            writeln!(f, "{link}")?;
        }

        Ok(())
    }
}

impl FromStr for EventListPage {
    type Err = Box<dyn std::error::Error>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let document = Document::from(s);

        let node = document.find(Class("ohanah")).next().unwrap();
        let mut event_links: HashSet<String> = HashSet::new();
        for link in node.find(Name("a")) {
            let href = link.attr("href").unwrap();
            event_links.insert(format!("{BASE_URL}{href}"));
        }

        Ok(EventListPage { event_links })
    }
}

#[derive(Debug, Default)]
struct Event {
    name: String,
    link: String,
}

impl Event {
    fn from_str(s: &str, link: String) -> Result<Self, Box<dyn std::error::Error>> {
        let name = link.clone();

        Ok(Event { name, link })
    }
}
