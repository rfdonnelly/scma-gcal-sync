use crate::model::{Attendee, Comment, DateSelect, Event, User};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{And, Attr, Class, Name};
use tap::prelude::*;
use tracing::info;

use std::convert::TryFrom;

const SITE_URL: &str = "https://www.rockclimbing.org";
const LOGIN_URL: &str = "https://www.rockclimbing.org/index.php/component/comprofiler/login";
const EVENTS_URL: &str =
    "https://www.rockclimbing.org/index.php/event-list/events-list?format=json";
const USERS_URL: &str = "https://www.rockclimbing.org/index.php?option=com_jsondumper";
const CONCURRENT_REQUESTS: usize = 3;

pub struct Web {
    dates: DateSelect,
    client: reqwest::Client,
}

impl Web {
    pub async fn new(
        username: &str,
        password: &str,
        dates: DateSelect,
    ) -> Result<Web, Box<dyn std::error::Error>> {
        let client = Self::create_client()?;

        let web = Self { dates, client };

        web.login(username, password).await?;

        Ok(web)
    }

    pub async fn read(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events = self.fetch_events().await?;
        let events = self.fetch_events_details(events).await?;
        Ok(events)
    }

    pub async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events_url = match self.dates {
            DateSelect::All => EVENTS_URL.to_string(),
            DateSelect::NotPast => [EVENTS_URL, "&filterEvents=notpast"].join(""),
        };

        info!(url=%events_url, "Fetching event list page");
        let events_page = Page::from_url(&self.client, &events_url).await?;
        let events = EventList::try_from(events_page)?.into_inner();

        Ok(events)
    }

    fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
        Ok(reqwest::Client::builder()
            .cookie_store(true)
            .user_agent(format!(
                "{} {} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_REPOSITORY")
            ))
            .build()?)
    }

    async fn login(&self, username: &str, password: &str) -> anyhow::Result<()> {
        let url = LOGIN_URL;

        info!(%url, "Logging in");

        let login_params = [("username", username), ("passwd", password)];
        let rsp = self
            .client
            .post(url)
            .form(&login_params)
            .send()
            .await
            .with_context(|| format!("unable to login to {} due to bad request", SITE_URL))?;

        if !rsp.status().is_success() {
            Err(anyhow!(
                "unable to login to {} due to bad response",
                SITE_URL
            ))
        } else if rsp.url().path() != "/" {
            Err(anyhow!(
                "unable to login to {} due to bad username or password",
                SITE_URL
            ))
        } else {
            Ok(())
        }
    }

    async fn fetch_events_details(
        &self,
        events: Vec<Event>,
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
        let timestamp = Utc::now();
        let event = Event::try_from((event, event_page, timestamp))?;
        Ok(event)
    }

    pub async fn fetch_users(&self) -> Result<Vec<User>, Box<dyn std::error::Error>> {
        let url = USERS_URL;

        info!(url=%url, "Fetching users");
        let page = Page::from_url(&self.client, url).await?;
        let users = Users::try_from(page)?;

        Ok(users.0)
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

impl TryFrom<(Event, Page, DateTime<Utc>)> for Event {
    type Error = Box<dyn std::error::Error>;

    fn try_from(event_page_timestamp: (Event, Page, DateTime<Utc>)) -> Result<Self, Self::Error> {
        let (event_item, page, timestamp) = event_page_timestamp;

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

        let timestamp = Some(timestamp);

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
            timestamp,
        };

        Ok(event)
    }
}

use serde::Serialize;
#[derive(Serialize)]
pub struct EventList(Vec<Event>);

impl EventList {
    fn into_inner(self) -> Vec<Event> {
        self.0
    }
}

impl TryFrom<Page> for EventList {
    type Error = Box<dyn std::error::Error>;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        let events = serde_json::from_str::<Vec<Event>>(page.as_ref())?.tap_mut(|events| {
            events
                .iter_mut()
                .for_each(|event| event.url = [SITE_URL, &event.url].join(""))
        });

        Ok(Self(events))
    }
}

impl FromIterator<Event> for EventList {
    fn from_iter<I: IntoIterator<Item = Event>>(iter: I) -> Self {
        Self(Vec::from_iter(iter))
    }
}

#[derive(Serialize)]
pub struct Users(Vec<User>);

impl TryFrom<Page> for Users {
    type Error = Box<dyn std::error::Error>;

    fn try_from(page: Page) -> Result<Self, Self::Error> {
        use serde::Deserialize;
        #[derive(Deserialize)]
        struct Data {
            users: Vec<User>,
        }

        let mut data: Data = serde_json::from_str::<Data>(page.as_ref())?;
        data.users.iter_mut().for_each(|user| {
            user.phone = user.phone.as_ref().map(normalize_phone_number);
            user.email = normalize_email(&user.email);
            user.timestamp = Some(Utc::now());
        });
        Ok(Users(data.users))
    }
}

fn normalize_phone_number<S>(phone_number: S) -> String
where
    S: AsRef<str>,
{
    let prefix = "+1".chars();
    let suffix = phone_number.as_ref().chars().filter(char::is_ascii_digit);
    prefix.chain(suffix).collect()
}

fn normalize_email<S>(email: S) -> String
where
    S: AsRef<str>,
{
    email.as_ref().to_lowercase()
}

#[cfg(test)]
mod test {
    use super::*;

    use chrono::TimeZone;

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
            timestamp: None,
        };
        let timestamp = Utc.timestamp_opt(0, 0).unwrap();
        let event = Event::try_from((event_item, page, timestamp)).unwrap();
        insta::assert_yaml_snapshot!(event);
    }

    #[test]
    fn parse_event_list_json() {
        let path = path_to_input("events-list.json");
        let page = Page::from_file(path).unwrap();
        let events = EventList::try_from(page).unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[test]
    fn parse_users() {
        let path = path_to_input("users.json");
        let page = Page::from_file(path).unwrap();
        let users = Users::try_from(page).unwrap().tap_mut(|users| {
            users.0.iter_mut().for_each(|user| user.timestamp = None);
        });
        insta::assert_yaml_snapshot!(users);
    }

    #[test]
    fn normalize_phone_number() {
        let phone_numbers = vec![
            "(555) 555-5555",
            "5555555555",
            "555-555-5555",
            "555 555 5555",
        ];
        let actual: Vec<String> = phone_numbers
            .into_iter()
            .map(super::normalize_phone_number)
            .collect();
        insta::assert_yaml_snapshot!(actual);
    }
}
