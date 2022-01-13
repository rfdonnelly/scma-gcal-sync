use crate::model::{Attendee, Comment, Event};

use chrono::NaiveDate;
use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{And, Attr, Class, Name};
use tap::prelude::*;
use tracing::info;

use std::convert::TryFrom;

const LOGIN_PATH: &str = "/index.php/component/comprofiler/login";
const EVENTS_PATH: &str = "/index.php/event-list/events-list?format=json";
const CONCURRENT_REQUESTS: usize = 3;

pub struct Web<'a> {
    base_url: &'a str,
    min_date: Option<NaiveDate>,
    client: reqwest::Client,
}

impl<'a> Web<'a> {
    pub async fn new(
        username: &str,
        password: &str,
        base_url: &'a str,
        min_date: Option<NaiveDate>,
    ) -> Result<Web<'a>, Box<dyn std::error::Error>> {
        let client = Self::create_client()?;

        let web = Self {
            base_url,
            min_date,
            client,
        };

        web.login(username, password).await?;

        Ok(web)
    }

    pub async fn read(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events = self.fetch_events().await?;
        let events = self.fetch_events_details(events).await?;
        Ok(events)
    }

    pub async fn fetch_events(&self) -> Result<EventList, Box<dyn std::error::Error>> {
        let events_url = [self.base_url, EVENTS_PATH].join("");
        info!(url=%events_url, "Fetching event list page");
        let events_page = Page::from_url(&self.client, &events_url).await?;
        let events = EventList::try_from((self.base_url, events_page))?;

        let events = match self.min_date {
            None => events,
            Some(min_date) => events
                .into_iter()
                .filter(|event| event.end_date > min_date)
                .collect(),
        };

        Ok(events)
    }

    fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
        Ok(reqwest::Client::builder()
            .cookie_store(true)
            .user_agent("Mozilla/5.0")
            .build()?)
    }

    async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let url = [self.base_url, LOGIN_PATH].join("");

        info!(%url, "Logging in");

        let login_params = [("username", username), ("passwd", password)];
        let rsp = self.client.post(url).form(&login_params).send().await?;

        if !rsp.status().is_success() {
            Err("login failed".into())
        } else if rsp.url().path() != "/" {
            Err("bad username or password".into())
        } else {
            Ok(())
        }
    }

    async fn fetch_events_details(
        &self,
        events: EventList,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events = stream::iter(events)
            .map(|event| self.fetch_event_details(event))
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>()
            .await?
            .tap_mut(|events| events.sort_by_key(|event| event.start_date));

        Ok(events)
    }

    pub async fn fetch_event_details(
        &self,
        event: Event,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        info!(%event.id, %event, url=%event.url, "Fetching event");
        let event_page = Page::from_url(&self.client, &event.url).await?;
        let event = Event::try_from((event, event_page))?;
        Ok(event)
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
    async fn from_url(
        client: &reqwest::Client,
        url: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let rsp = client.get(url).send().await?;
        let text = rsp.text().await?;

        Ok(Self(text))
    }
}

impl TryFrom<(Event, Page)> for Event {
    type Error = Box<dyn std::error::Error>;

    fn try_from(event_page_pair: (Event, Page)) -> Result<Self, Self::Error> {
        let (event_item, page) = event_page_pair;

        let id = event_item.id;
        let title = event_item.title;
        let url = event_item.url;
        let start_date = event_item.start_date;
        let end_date = event_item.end_date;
        let location = event_item.location;
        let description = event_item.description;

        let document = Document::from(page.as_ref());

        let comments: Vec<Comment> = document
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

                Comment { author, date, text }
            })
            .collect();
        let comments = if comments.is_empty() {
            None
        } else {
            Some(comments)
        };

        let attendee_names = document
            .find(Class("attendee_name"))
            .map(|node| node.text());
        let attendee_comments = document
            .find(Class("number_of_tickets"))
            .map(|node| node.text());
        let attendees: Vec<Attendee> = attendee_names
            .zip(attendee_comments)
            .map(|(name, comment)| {
                let count = comment.split_once(' ').unwrap().0[1..].parse().unwrap();

                let comment = comment.split_once(')').unwrap().1.trim().to_string();

                Attendee {
                    name,
                    count,
                    comment,
                }
            })
            .collect();
        let attendees = if attendees.is_empty() {
            None
        } else {
            Some(attendees)
        };

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

use serde::Serialize;
#[derive(Serialize)]
pub struct EventList(Vec<Event>);

impl TryFrom<(&str, Page)> for EventList {
    type Error = Box<dyn std::error::Error>;

    fn try_from(url_page: (&str, Page)) -> Result<Self, Self::Error> {
        let (base_url, page) = url_page;

        let events = serde_json::from_str::<Vec<Event>>(page.as_ref())?.tap_mut(|events| {
            events
                .iter_mut()
                .for_each(|event| event.url = [base_url, &event.url].join(""))
        });

        Ok(Self(events))
    }
}

impl IntoIterator for EventList {
    type Item = Event;
    type IntoIter = std::vec::IntoIter<Event>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<Event> for EventList {
    fn from_iter<I: IntoIterator<Item = Event>>(iter: I) -> Self {
        Self(Vec::from_iter(iter))
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
        let path = path_to_input("event-527.html");
        let page = Page::from_file(path).unwrap();
        let event_item = Event {
            id: "527".into(),
            title: "a title".into(),
            url: "a url".into(),
            start_date: "2022-01-14".parse().unwrap(),
            end_date: "2022-01-17".parse().unwrap(),
            location: "a location".into(),
            description: "a description".into(),
            comments: None,
            attendees: None,
        };
        let event = Event::try_from((event_item, page)).unwrap();
        insta::assert_yaml_snapshot!(event);
    }

    #[test]
    fn parse_event_list_json() {
        let path = path_to_input("events-list.json");
        let page = Page::from_file(path).unwrap();
        let base_url = "https://www.rockclimbing.org";
        let events = EventList::try_from((base_url, page)).unwrap();
        insta::assert_yaml_snapshot!(events);
    }
}
