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

use std::convert::TryFrom;

const BASE_URL: &str = "https://www.rockclimbing.org";
const LOGIN_URL: &str = "https://www.rockclimbing.org/index.php/component/comprofiler/login";
const EVENTS_URL: &str = "https://www.rockclimbing.org/index.php/event-list/events-list?format=json";
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
        let client = Self::create_client()?;
        Self::login(&client, self.username, self.password).await?;

        info!("Fetching event list page {}", EVENTS_URL);
        let events_page = Page::from_url(&client, EVENTS_URL).await?;
        let events = EventList::try_from(events_page)?;

        let events = Self::fetch_events(&client, events).await?;

        Ok(events)
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

    async fn fetch_events(client: &reqwest::Client, event_items: EventList) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events = stream::iter(event_items)
            .map(|event_item| {
                let client = &client;
                async move {
                    let event_url = [BASE_URL, &event_item.url].join("");
                    info!("Fetching event from {}", event_url);
                    let event_page = Page::from_url(&client, &event_url).await?;
                    let event = Event::try_from((event_item, event_page))?;
                    Ok::<Event, Box<dyn std::error::Error>>(event)
                }
            })
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>().await?
            .tap_mut(|events| events.sort_by_key(|event| event.start_date));

        Ok(events)
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

impl TryFrom<(EventListItem, Page)> for Event {
    type Error = Box<dyn std::error::Error>;

    fn try_from(event_page_pair: (EventListItem, Page)) -> Result<Self, Self::Error> {
        let (event_item, page) = event_page_pair;

        let id = event_item.id;
        let title = event_item.title;
        let url = event_item.url;
        let start_date = event_item.start_date;
        let end_date = event_item.end_date;
        let location = event_item.location;
        let description = Document::from(event_item.description.as_ref())
            .find(Name("div"))
            .next()
            .unwrap()
            .find(Name("div"))
            .map(|div| div.text())
            .collect::<Vec<String>>()
            .join("\n");

        let document = Document::from(page.as_ref());

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
            id,
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

use serde::{Serialize, Deserialize};
use chrono::NaiveDate;
#[derive(Serialize, Deserialize)]
struct EventListItem {
    id: String,
    title: String,
    url: String,
    #[serde(rename(deserialize = "date"))]
    start_date: NaiveDate,
    end_date: NaiveDate,
    #[serde(rename(deserialize = "venue"))]
    location: String,
    description: String,
}

#[derive(Serialize)]
struct EventList(Vec<EventListItem>);

impl TryFrom<Page> for EventList {
    type Error = Box<dyn std::error::Error>;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        let events: Vec<EventListItem> = serde_json::from_str(page.as_ref())?;

        Ok(Self(events))
    }
}

impl IntoIterator for EventList {
    type Item = EventListItem;
    type IntoIter = std::vec::IntoIter<EventListItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
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

    fn path_to_input(filename: &str) -> PathBuf {
        [env!("CARGO_MANIFEST_DIR"), "test", "inputs", filename]
            .iter()
            .collect()
    }

    #[test]
    fn parse_event() {
        let path = path_to_input("event-0.html");
        let page = Page::from_file(path).unwrap();
        let event_item = EventListItem {
            id: "an id".into(),
            title: "a title".into(),
            url: "a url".into(),
            start_date: "2022-01-14".parse().unwrap(),
            end_date: "2022-01-17".parse().unwrap(),
            location: "a location".into(),
            // FIXME: Capture the first span
            //
            // The first span is ignored because we are only looking at the contents of the divs.
            // Make description parsing more intelligent so that it includes the divless span AND
            // separates the divs with newlines.
            description: "<font face=\"Arial, Verdana\"><span style=\"font-size: 13.3333px;\">Camping Fri and Sat nights at Joshua Tree, Ryan Campground.</span></font><div><font face=\"Arial, Verdana\"><div style=\"font-size: 13.3333px;\">Fri and Sat nights : Four campsites:</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#3 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#4 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#6 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space: pre;\">\t</span>#7 (2 parking spaces)</div><div style=\"style\"><span style=\"font-size: 13.3333px;\">Trip Leader: Rob Donnelly</span></div></font></div>".into(),
        };
        let event = Event::try_from((event_item, page)).unwrap();
        insta::assert_yaml_snapshot!(event);
    }

    #[test]
    fn parse_event_list_json() {
        let path = path_to_input("events-list.json");
        let page = Page::from_file(path).unwrap();
        let events = EventList::try_from(page).unwrap();
        insta::assert_yaml_snapshot!(events);
    }
}
