use crate::model::{Attendee, Comment, DateSelect, Event, User};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::predicate::{And, Attr, Class, Name};
use tap::prelude::*;
use tracing::info;

use std::collections::HashMap;
use std::convert::TryFrom;

const LOGIN_PATH: &str = "/index.php/component/comprofiler/login";
const EVENTS_PATH: &str = "/index.php/event-list/events-list?format=json";
const USERS_PATH: &str = "/index.php?option=com_comprofiler&task=usersList&listid=5";
const CONCURRENT_REQUESTS: usize = 3;

pub struct Web<'a> {
    base_url: &'a str,
    dates: DateSelect,
    client: reqwest::Client,
}

impl<'a> Web<'a> {
    pub async fn new(
        username: &str,
        password: &str,
        base_url: &'a str,
        dates: DateSelect,
    ) -> Result<Web<'a>, Box<dyn std::error::Error>> {
        let client = Self::create_client()?;

        let web = Self {
            base_url,
            dates,
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

    pub async fn fetch_events(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events_url = match self.dates {
            DateSelect::All => [self.base_url, EVENTS_PATH].join(""),
            DateSelect::NotPast => [self.base_url, EVENTS_PATH, "&filterEvents=notpast"].join(""),
        };

        info!(url=%events_url, "Fetching event list page");
        let events_page = Page::from_url(&self.client, &events_url).await?;
        let events = EventList::try_from((self.base_url, events_page))?.into_inner();

        Ok(events)
    }

    fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
        Ok(reqwest::Client::builder()
            .cookie_store(true)
            .user_agent("Mozilla/5.0")
            .build()?)
    }

    async fn login(&self, username: &str, password: &str) -> anyhow::Result<()> {
        let url = [self.base_url, LOGIN_PATH].join("");

        info!(%url, "Logging in");

        let login_params = [("username", username), ("passwd", password)];
        let rsp = self
            .client
            .post(url)
            .form(&login_params)
            .send()
            .await
            .with_context(|| format!("unable to login to {} due to bad request", self.base_url))?;

        if !rsp.status().is_success() {
            Err(anyhow!(
                "unable to login to {} due to bad response",
                self.base_url
            ))
        } else if rsp.url().path() != "/" {
            Err(anyhow!(
                "unable to login to {} due to bad username or password",
                self.base_url
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
        let url = [self.base_url, USERS_PATH].join("");

        info!(url=%url, "Fetching users");
        let page = Page::from_url(&self.client, &url).await?;
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
        let mut email: Option<String> = None;
        let mut id_emails: HashMap<String, String> = HashMap::new();

        for line in page.as_ref().lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("var addy") {
                let (_, right) = trimmed.split_once("= '").unwrap();
                let (obfuscated, _) = right.split_once("';").unwrap();
                let obfuscated = obfuscated.replace("'+ '", "");
                let obfuscated = obfuscated.replace("' + '", "");
                let obfuscated = obfuscated.replace("' +'", "");
                email = Some(html_escape::decode_html_entities(&obfuscated).to_string());
            } else if trimmed.starts_with("$('#cbMa") {
                let (_, right) = trimmed.split_once("$('#").unwrap();
                let (id, _) = right.split_once("')").unwrap();

                id_emails.insert(id.to_string(), email.take().unwrap().clone());
            } else if trimmed.contains("cbUserURLs") {
                break;
            }
        }

        let document = Document::from(page.as_ref());
        let tbody = document.find(Name("tbody")).next().unwrap();
        let members = tbody
            .find(Name("tr"))
            .map(|tr| {
                let name = tr
                    .find(Class("cbUserListFC_formatname"))
                    .map(|node| node.text())
                    .next()
                    .unwrap_or_else(|| "UNDEFINED".to_string());
                let member_status = tr
                    .find(Class("cbUserListFC_cb_memberstatus"))
                    .next()
                    .unwrap()
                    .text()
                    .parse()?;
                let trip_leader_status =
                    match tr.find(Class("cbUserListFC_cb_tripleaderstatus")).next() {
                        Some(node) => Some(node.text().parse()?),
                        None => None,
                    };
                let position = match tr.find(Class("cbUserListFC_cb_position")).next() {
                    Some(node) => Some(node.text().parse()?),
                    None => None,
                };
                let address = tr
                    .find(Class("cbUserListFC_cb_address"))
                    .next()
                    .unwrap()
                    .text();
                let city = tr
                    .find(Class("cbUserListFC_cb_city"))
                    .next()
                    .unwrap()
                    .text();
                let state = tr
                    .find(Class("cbUserListFC_cb_state"))
                    .next()
                    .unwrap()
                    .text();
                let zipcode = tr
                    .find(Class("cbUserListFC_cb_zipcode"))
                    .next()
                    .unwrap()
                    .text();
                let phone = tr
                    .find(Class("cbUserListFC_cb_phone"))
                    .next()
                    .map(|node| node.text())
                    .map(normalize_phone_number);
                let email_id = tr
                    .find(Class("cbMailRepl"))
                    .next()
                    .unwrap()
                    .attr("id")
                    .unwrap();
                let undefined = "UNDEFINED".to_string();
                let email = id_emails.get(email_id).unwrap_or(&undefined);
                let email = normalize_email(email);

                let user = User {
                    name,
                    member_status,
                    trip_leader_status,
                    position,
                    address,
                    city,
                    state,
                    zipcode,
                    phone,
                    email,
                };

                Ok(user)
            })
            .collect::<Result<Vec<User>, Box<dyn std::error::Error>>>()?;

        Ok(Users(members))
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
        let timestamp = Utc.timestamp(0, 0);
        let event = Event::try_from((event_item, page, timestamp)).unwrap();
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

    #[test]
    fn parse_users() {
        let path = path_to_input("users.html");
        let page = Page::from_file(path).unwrap();
        let users = Users::try_from(page).unwrap();
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
