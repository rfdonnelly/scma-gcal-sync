use crate::model::{
    Attendee,
    Comment,
    Event,
};

use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{
    And,
    Attr,
    Class,
    Name,
};
use tap::prelude::*;
use tracing::info;

use std::collections::HashSet;
use std::convert::TryFrom;

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

    pub async fn read(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let client = create_client()?;
        login(&client, self.username, self.password).await?;

        info!("Fetching event list page {}", EVENTS_URL);
        let events_page = Page::from_url(&client, EVENTS_URL).await?;
        let event_urls = EventUrls::try_from(events_page)?;
        info!("Parsed event URLs {:?}", event_urls);

        let events = stream::iter(event_urls)
            .map(|event_url| {
                let client = &client;
                async move {
                    info!("Fetching event from {}", event_url);
                    let event_page = Page::from_url(&client, &event_url).await?;
                    let event = Event::try_from(event_page)?;
                    info!("Parsed {:?}", event);
                    Ok::<Event, Box<dyn std::error::Error>>(event)
                }
            })
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>().await?
            .tap_mut(|events| events.sort_by_key(|event| event.start_date));

        Ok(events)
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
struct Page(String);

/// Accessor for the Page's content
impl AsRef<str> for Page {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl Page {
    async fn from_url(client: &reqwest::Client, url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let rsp = client.get(url).send().await?;
        let text = rsp.text().await?;

        Ok(Self(text))
    }
}

type Url = String;
#[derive(Debug)]
struct EventUrls(Vec<Url>);

impl TryFrom<Page> for EventUrls {
    type Error = Box<dyn std::error::Error>;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        let document = Document::from(page.as_ref());

        let node = document.find(Class("ohanah")).next().unwrap();
        let urls = node
            .find(Name("a"))
            .fold(HashSet::new(), |mut urls, elem| {
                let href = elem.attr("href").unwrap();
                urls.insert(format!("{BASE_URL}{href}"));
                urls
            });

        Ok(EventUrls(Vec::from_iter(urls)))
    }
}

impl IntoIterator for EventUrls {
    type Item = Url;
    type IntoIter = std::vec::IntoIter<Url>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl TryFrom<Page> for Event {
    type Error = Box<dyn std::error::Error>;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        let document = Document::from(page.as_ref());

        let title = document
            .find(And(Name("meta"), Attr("property", "og:title")))
            .next()
            .unwrap()
            .attr("content")
            .unwrap()
            .to_string();

        let url = document
            .find(Name("base"))
            .next()
            .unwrap()
            .attr("href")
            .unwrap()
            .to_string();

        let start_date = document
            .find(And(Name("span"), Attr("itemprop", "startDate")))
            .next()
            .unwrap()
            .attr("content")
            .unwrap()
            .parse()?;

        let end_date = document
            .find(And(Name("span"), Attr("itemprop", "endDate")))
            .next()
            .unwrap()
            .attr("content")
            .unwrap()
            .parse()?;

        let location = document
            .find(And(Name("h3"), Attr("itemprop", "location")))
            .next()
            .unwrap()
            .text()
            .trim()
            .to_string();

        let description = document
            .find(And(Name("div"), Attr("itemprop", "description")))
            .next()
            .unwrap()
            .find(Name("div"))
            .map(|div| div.text())
            .collect::<Vec<String>>()
            .join("\n");

        let comments = document
            .find(Class("kmt-wrap"))
            .map(|node| {
                let author = node
                    .find(Class("kmt-author"))
                    .next()
                    .unwrap()
                    .text()
                    .trim()
                    .to_string();

                let date = node
                    .find(And(Name("time"), Attr("itemprop", "dateCreated")))
                    .next()
                    .unwrap()
                    .attr("datetime")
                    .unwrap()
                    .parse()
                    .unwrap();

                let text = node
                    .find(Class("kmt-body"))
                    .next()
                    .unwrap()
                    .text()
                    .trim()
                    .to_string();

                Comment {
                    author,
                    date,
                    text,
                }
            })
            .collect();

        let attendee_names = document
            .find(Class("attendee_name"))
            .map(|node| node.text());
        let attendee_comments = document
            .find(Class("number_of_tickets"))
            .map(|node| node.text());
        let attendees = attendee_names.zip(attendee_comments)
            .map(|(name, comment)| {
                let count = comment
                    .split_once(" ")
                    .unwrap()
                    .0[1..]
                    .parse()
                    .unwrap();

                let comment = comment
                    .split_once(")")
                    .unwrap()
                    .1
                    .trim()
                    .to_string();

                Attendee {
                    name,
                    count,
                    comment,
                }
            })
            .collect();

        let event = Event {
            title,
            url,
            start_date,
            end_date,
            location,
            description,
            comments,
            attendees,
        };

        Ok(event)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::path::{Path, PathBuf};

    impl Page {
        fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
            let text = std::fs::read_to_string(path)?;

            Ok(Self(text))
        }
    }

    #[test]
    fn parse_event_list_page() {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "test", "inputs", "events-list.html"].iter().collect();
        let page = Page::from_file(path).unwrap();
        let urls = {
            let mut urls = EventUrls::try_from(page).unwrap();
            urls.0.sort();
            urls
        };
        insta::assert_yaml_snapshot!(urls.0);
    }

    #[test]
    fn parse_event() {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "test", "inputs", "event-0.html"].iter().collect();
        let page = Page::from_file(path).unwrap();
        let event = Event::try_from(page).unwrap();
        insta::assert_yaml_snapshot!(event);
    }
}
