use crate::model::Event;

use chrono::Duration;
use futures::{stream, StreamExt, TryStreamExt};
use google_calendar3::{api, CalendarHub};
use tracing::info;
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use std::fmt::Write;

pub struct GCal {
    calendar_id: String,
    hub: CalendarHub,
}

const DESCRIPTION_BUFFER_SIZE: usize = 4098;
const CONCURRENT_REQUESTS: usize = 3;
const SCOPE: api::Scope = api::Scope::Full;

impl GCal {
    pub async fn new(
        calendar_name: &str,
        client_secret_json_path: &str,
        oauth_token_json_path: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hub = Self::create_hub(client_secret_json_path, oauth_token_json_path).await?;

        info!(%calendar_name, "Finding calendar");
        let (_, list) = hub.calendar_list().list().add_scope(SCOPE).doit().await?;
        let calendars = list.items.unwrap();

        let calender_entry = calendars
            .iter()
            .find(|entry| entry.summary.as_ref().unwrap() == calendar_name)
            .unwrap();
        let calendar_id = calender_entry.id.as_ref().unwrap().clone();
        info!(%calendar_id, "Found calendar");

        let gcal = Self { calendar_id, hub };

        Ok(gcal)
    }

    async fn create_hub(
        client_secret_json_path: &str,
        oauth_token_json_path: &str,
    ) -> Result<CalendarHub, Box<dyn std::error::Error>> {
        let secret = yup_oauth2::read_application_secret(client_secret_json_path).await?;

        info!(oauth_client_id=?secret.client_id, "Authenticating");
        let auth =
            InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
                .persist_tokens_to_disk(oauth_token_json_path)
                .build()
                .await?;

        let scopes = [SCOPE];
        let token = auth.token(&scopes).await?;
        info!(expiration_time=?token.expiration_time(), "Got token");

        let client =
            hyper::Client::builder().build(hyper_rustls::HttpsConnector::with_native_roots());

        let hub = CalendarHub::new(client, auth);

        Ok(hub)
    }

    pub async fn write(&self, events: &[Event]) -> Result<(), Box<dyn std::error::Error>> {
        stream::iter(events)
            .map(|event| self.patch_or_insert_event(event))
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>()
            .await?;

        Ok(())
    }

    pub async fn patch_or_insert_event(
        &self,
        event: &Event,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let g_event = api::Event::try_from(event)?;

        let _rsp = {
            let event_id = g_event.id.as_ref().unwrap().clone();
            let result = self
                .hub
                .events()
                .patch(g_event.clone(), &self.calendar_id, &event_id)
                .add_scope(SCOPE)
                .doit()
                .await;
            match result {
                Err(_) => {
                    let rsp = self
                        .hub
                        .events()
                        .insert(g_event, &self.calendar_id)
                        .add_scope(SCOPE)
                        .doit()
                        .await?;
                    let link = rsp.1.html_link.as_ref().unwrap();
                    info!(%event.id, %event, %link, "Inserted");
                    rsp
                }
                Ok(rsp) => {
                    let link = rsp.1.html_link.as_ref().unwrap();
                    info!(%event.id, %event, %link, "Updated");
                    rsp
                }
            }
        };

        Ok(())
    }
}

impl TryFrom<&Event> for api::Event {
    type Error = Box<dyn ::std::error::Error>;

    fn try_from(event: &Event) -> Result<Self, Self::Error> {
        let id = event_id(event)?;
        let summary = event_summary(event);
        let start = event_start(event);
        let end = event_end(event);
        let description = event_description(event)?;
        let location = event.location.clone();

        let g_event = api::Event {
            id: Some(id),
            summary: Some(summary),
            start: Some(start),
            end: Some(end),
            description: Some(description),
            location: Some(location),
            ..Default::default()
        };

        Ok(g_event)
    }
}

fn event_id(event: &Event) -> Result<String, std::num::ParseIntError> {
    let id: u32 = event.id.parse()?;
    let id = format!("{:05}", id);
    Ok(id)
}

fn event_summary(event: &Event) -> String {
    format!("SCMA: {}", event.title)
}

fn event_start(event: &Event) -> api::EventDateTime {
    api::EventDateTime {
        date: Some(event.start_date.to_string()),
        ..Default::default()
    }
}

fn event_end(event: &Event) -> api::EventDateTime {
    // WORKAROUND: Google Calendar seems to require all-day-multi-day events to end on the day
    // after.  Otherwise they show as 1 day short.
    let end_date = if event.start_date == event.end_date {
        event.end_date
    } else {
        event.end_date + Duration::days(1)
    };

    api::EventDateTime {
        date: Some(end_date.to_string()),
        ..Default::default()
    }
}

fn event_description(event: &Event) -> Result<String, Box<dyn ::std::error::Error>> {
    let mut buffer = String::with_capacity(DESCRIPTION_BUFFER_SIZE);
    write!(buffer, "{}", event.url)?;
    write!(buffer, "<h3>Description</h3>")?;
    write!(buffer, "{}", event.description)?;

    write!(buffer, "<h3>Attendees</h3>")?;
    match event.attendees.as_ref() {
        Some(attendees) => {
            write!(buffer, "<ol>")?;
            for attendee in attendees {
                write!(
                    buffer,
                    "<li>{} ({}) {}</li>",
                    attendee.name, attendee.count, attendee.comment
                )?;
            }
            write!(buffer, "</ol>")?;
        }
        None => {
            write!(buffer, "None")?;
        }
    }

    write!(buffer, "<h3>Comments</h3>")?;
    match event.comments.as_ref() {
        Some(comments) => {
            write!(buffer, "<ul>")?;
            for comment in comments {
                write!(
                    buffer,
                    "<li>{} ({}) {}</li>",
                    comment.author, comment.date, comment.text
                )?;
            }
            write!(buffer, "</ul>")?;
        }
        None => {
            write!(buffer, "None")?;
        }
    }

    if let Some(timestamp) = event.timestamp {
        let pacific = chrono::FixedOffset::west(8 * 60 * 60);
        write!(buffer, "\n\nLast synced at {} by <a href='https://github.com/rfdonnelly/scma-gcal-sync'>scma-gcal-sync</a>.", timestamp.with_timezone(&pacific).to_rfc3339_opts(chrono::SecondsFormat::Secs, false))?;
    }

    Ok(buffer)
}
