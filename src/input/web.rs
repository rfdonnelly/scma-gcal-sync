use crate::model::{Attendee, Comment, Event};

use chrono::NaiveDate;
use futures::{stream, StreamExt, TryStreamExt};
use select::document::Document;
use select::node::Data;
use select::node::Node;
use select::predicate::{And, Attr, Class, Name};
use tap::prelude::*;
use tracing::info;

use std::convert::TryFrom;

const LOGIN_PATH: &str = "/index.php/component/comprofiler/login";
const EVENTS_PATH: &str = "/index.php/event-list/events-list?format=json";
const CONCURRENT_REQUESTS: usize = 3;

pub struct Web<'a> {
    username: &'a str,
    password: &'a str,
    base_url: &'a str,
    min_date: Option<NaiveDate>,
}

impl<'a> Web<'a> {
    pub fn new(
        username: &'a str,
        password: &'a str,
        base_url: &'a str,
        min_date: Option<NaiveDate>,
    ) -> Self {
        Self {
            username,
            password,
            base_url,
            min_date,
        }
    }

    pub async fn read(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let client = Self::create_client()?;
        self.login(&client, self.username, self.password).await?;

        let events_url = [self.base_url, EVENTS_PATH].join("");
        info!(url=%events_url, "Fetching event list page");
        let events_page = Page::from_url(&client, &events_url).await?;
        let events = EventList::try_from((self.base_url, events_page))?;

        let events = match self.min_date {
            None => events,
            Some(min_date) => events
                .into_iter()
                .filter(|event| event.end_date > min_date)
                .collect(),
        };

        let events = Self::fetch_events(&client, events).await?;

        Ok(events)
    }

    fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
        Ok(reqwest::Client::builder()
            .cookie_store(true)
            .user_agent("Mozilla/5.0")
            .build()?)
    }

    async fn login<S>(
        &self,
        client: &reqwest::Client,
        username: S,
        password: S,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        S: AsRef<str>,
    {
        let url = [self.base_url, LOGIN_PATH].join("");

        info!(%url, "Logging in");

        let login_params = [
            ("username", username.as_ref()),
            ("passwd", password.as_ref()),
        ];
        let rsp = client.post(url).form(&login_params).send().await?;

        if !rsp.status().is_success() {
            Err("login failed".into())
        } else if rsp.url().path() != "/" {
            Err("bad username or password".into())
        } else {
            Ok(())
        }
    }

    async fn fetch_events(
        client: &reqwest::Client,
        events: EventList,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let events = stream::iter(events)
            .map(|event| Self::fetch_event(client, event))
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>()
            .await?
            .tap_mut(|events| events.sort_by_key(|event| event.start_date));

        Ok(events)
    }

    async fn fetch_event(
        client: &reqwest::Client,
        event: Event,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        info!(%event.id, %event, url=%event.url, "Fetching event");
        let event_page = Page::from_url(client, &event.url).await?;
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

                Comment { author, date, text }
            })
            .collect();
        let comments = Some(comments);

        let attendee_names = document
            .find(Class("attendee_name"))
            .map(|node| node.text());
        let attendee_comments = document
            .find(Class("number_of_tickets"))
            .map(|node| node.text());
        let attendees = attendee_names
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
        let attendees = Some(attendees);

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
struct EventList(Vec<Event>);

impl TryFrom<(&str, Page)> for EventList {
    type Error = Box<dyn std::error::Error>;

    fn try_from(url_page: (&str, Page)) -> Result<Self, Self::Error> {
        let (base_url, page) = url_page;

        let mut events: Vec<Event> = serde_json::from_str(page.as_ref())?;

        for event in &mut events {
            event.url = [base_url, &event.url].join("");
            event.description = parse_description(&event.description);
        }

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

fn parse_description(s: &str) -> String {
    let document = Document::from(s);
    let mut buffer = String::with_capacity(s.len());
    let node = document.find(Name("body")).next().unwrap();
    parse_node_text(&node, &mut buffer);
    buffer
}

fn parse_node_text(node: &Node, buffer: &mut String) {
    for child in node.children() {
        match child.data() {
            Data::Text(_) => {
                let text = child.as_text().unwrap();
                match text {
                    // Ignore newline-only text elements
                    "\n" => (),
                    _ => buffer.push_str(text),
                }
            }
            Data::Element(_, _) => {
                // Handles case where we transition from a non-newline element to a newline element
                // I.e. Inserts a newline between a non-newline element and a newline element
                maybe_newline(&child, buffer);
                parse_node_text(&child, buffer);
                // Insert a newline at the end of a newline element
                maybe_newline(&child, buffer);
            }
            Data::Comment(_) => (),
        }
    }
}

fn maybe_newline(node: &Node, buffer: &mut String) {
    let buffer_ends_with_newline = buffer.chars().last().unwrap_or_default() == '\n';
    let is_newline_element = matches!(node.name(), Some("p" | "div"));
    let insert_newline = !buffer.is_empty() && !buffer_ends_with_newline && is_newline_element;
    if insert_newline {
        buffer.push('\n');
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use indoc::indoc;

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

    #[test]
    fn parse_description_blank() {
        let input = "";
        let expected = "";
        assert_eq!(parse_description(input), expected);
    }

    #[test]
    fn parse_description_text() {
        let input = "Trip Leader: Mike Sauter";
        let expected = "Trip Leader: Mike Sauter";
        assert_eq!(parse_description(input), expected);
    }

    #[test]
    fn parse_description_basic_html() {
        let input = "<p>Trip Leaders: Chao & C. Irving</p>\r\n<p>2 days of hard climbing in the Needles.  You should be a competent 5.9 climber to attend this outing as there are no easy routes here.  No kidding!</p>";
        let expected = indoc! {"
            Trip Leaders: Chao & C. Irving
            2 days of hard climbing in the Needles.  You should be a competent 5.9 climber to attend this outing as there are no easy routes here.  No kidding!
        "};
        assert_eq!(parse_description(input), expected);
    }

    #[test]
    fn parse_description_div() {
        let input = "<font face=\"Arial, Verdana\"><span style=\"font-size: 13.3333px;\">Camping Fri and Sat nights at Joshua Tree, Ryan Campground.</span></font><div><font face=\"Arial, Verdana\"><div style=\"font-size: 13.3333px;\">Fri and Sat nights : Four campsites:</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#3 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#4 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space:pre\">\t</span>#6 (2 parking spaces)</div><div style=\"font-size: 13.3333px;\"><span style=\"white-space: pre;\">\t</span>#7 (2 parking spaces)</div><div style=\"style\"><span style=\"font-size: 13.3333px;\">Trip Leader: Rob Donnelly</span></div></font></div>";
        let expected = indoc! {"
            Camping Fri and Sat nights at Joshua Tree, Ryan Campground.
            Fri and Sat nights : Four campsites:
            \t#3 (2 parking spaces)
            \t#4 (2 parking spaces)
            \t#6 (2 parking spaces)
            \t#7 (2 parking spaces)
            Trip Leader: Rob Donnelly
        "};
        assert_eq!(parse_description(input), expected);
    }
}
