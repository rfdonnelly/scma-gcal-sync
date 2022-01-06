use crate::model::Event;

use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{
    Class,
    Name,
};
use tracing::info;

use std::collections::HashSet;
use std::path::Path;
use std::marker::PhantomData;

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
        let events_page = Page::from_url(&client, EVENTS_URL).await?;
        let event_urls = Page::<Vec<Url>>::parse(events_page)?;
        info!("Parsed event URLs {:?}", event_urls);

        let events: Vec<Event> = stream::iter(event_urls)
            .map(|event_url| {
                let client = &client;
                async move {
                    info!("Fetching event from {}", event_url);
                    let event_page = Page::from_url(&client, &event_url).await?;
                    let event = Page::<Event>::parse(event_page)?;
                    info!("Parsed {:?}", event);
                    Ok::<Event, Box<dyn std::error::Error>>(event)
                }
            })
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect().await?;

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

#[derive(Debug)]
struct Page<T> {
    text: String,
    phantom: PhantomData<T>,
}

impl<T> Page<T> {
    async fn from_url(client: &reqwest::Client, url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let rsp = client.get(url).send().await?;
        let text = rsp.text().await?;

        Ok(Self { text, phantom: PhantomData })
    }

    fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;

        Ok(Self { text, phantom: PhantomData })
    }
}

type Url = String;

impl Page<Vec<Url>> {
    fn parse(self) -> Result<Vec<Url>, Box<dyn std::error::Error>> {
        let document = Document::from(self.text.as_str());

        let node = document.find(Class("ohanah")).next().unwrap();
        let urls = node
            .find(Name("a"))
            .fold(HashSet::new(), |mut urls, elem| {
                let href = elem.attr("href").unwrap();
                urls.insert(format!("{BASE_URL}{href}"));
                urls
            });

        Ok(Vec::from_iter(urls))
    }
}

impl Page<Event> {
    fn parse(self) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Default::default())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::path::PathBuf;

    #[test]
    fn parse_event_list_page() {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "test", "inputs", "events-list.html"].iter().collect();
        let page = Page::from_file(path).unwrap();
        let urls = {
            let mut urls = Page::<Vec<Url>>::parse(page).unwrap();
            urls.sort();
            urls
        };
        insta::assert_yaml_snapshot!(urls);
    }
}
