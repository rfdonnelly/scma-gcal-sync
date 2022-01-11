use crate::model::{
    Event,
};

use google_calendar3::{
    api::Event as CalEvent,
    api::EventDateTime,
    CalendarHub,
};
use tracing::info;
use yup_oauth2::{
    InstalledFlowAuthenticator,
    InstalledFlowReturnMethod,
};

use std::fmt::Write;

pub struct GCal<'a> {
    calendar_name: &'a str,
    client_secret_json_path: &'a str,
    oauth_token_json_path: &'a str,
}

const DESCRIPTION_BUFFER_SIZE: usize = 4098;

impl<'a> GCal<'a> {
    pub fn new(
        calendar_name: &'a str,
        client_secret_json_path: &'a str,
        oauth_token_json_path: &'a str,
    ) -> Self {
        Self {
            calendar_name,
            client_secret_json_path,
            oauth_token_json_path,
        }
    }

    pub async fn write(&self, events: &[Event]) -> Result<(), Box<dyn std::error::Error>> {
        let secret = yup_oauth2::read_application_secret(self.client_secret_json_path).await?;

        info!("Authenticating");
        let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
            .persist_tokens_to_disk(self.oauth_token_json_path)
            .build()
            .await?;

        let client = hyper::Client::builder()
            .build(hyper_rustls::HttpsConnector::with_native_roots());

        let hub = CalendarHub::new(client, auth);

        info!("Listing calendars");
        let (_, list) = hub
            .calendar_list()
            .list()
            .doit()
            .await?;
        let calendars = list.items.unwrap();

        info!(calendar_name=%self.calendar_name, "Finding calendar");
        let calender_entry = calendars
            .iter()
            .find(|entry| entry.summary.as_ref().unwrap() == self.calendar_name)
            .unwrap();
        let calendar_id = calender_entry.id.as_ref().unwrap();
        info!(%calendar_id, "Found calendar");

        for event in events {
            let cal_event = CalEvent::try_from(event)?;

            let _rsp = {
                let event_id = cal_event.id.as_ref().unwrap().clone();
                let result = hub.events().patch(cal_event.clone(), calendar_id, &event_id).doit().await;
                match result {
                    Err(_) => {
                        let rsp = hub.events().insert(cal_event, calendar_id).doit().await?;
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
        }

        Ok(())
    }
}

impl TryFrom<&Event> for CalEvent {
    type Error = Box<dyn ::std::error::Error>;

    fn try_from(event: &Event) -> Result<Self, Self::Error> {
        let id = event_id(&event)?;
        let summary = event_summary(&event);
        let start = event_start(&event);
        let end = event_end(&event);
        let description = event_description(&event)?;
        let location = event.location.clone();

        let cal_event = CalEvent {
            id: Some(id),
            summary: Some(summary),
            start: Some(start),
            end: Some(end),
            description: Some(description),
            location: Some(location),
            ..Default::default()
        };

        Ok(cal_event)
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

fn event_start(event: &Event) -> EventDateTime {
    EventDateTime {
        date: Some(event.start_date.to_string()),
        ..Default::default()
    }
}

fn event_end(event: &Event) -> EventDateTime {
    EventDateTime {
        date: Some(event.end_date.to_string()),
        ..Default::default()
    }
}

fn event_description(event: &Event) -> Result<String, Box<dyn ::std::error::Error>> {
    let mut buffer = String::with_capacity(DESCRIPTION_BUFFER_SIZE);
    write!(buffer, "{}\n\n", event.url)?;
    write!(buffer, "{}", event.description)?;

    if let Some(attendees) = event.attendees.as_ref() {
        write!(buffer, "<h3>Attendees</h3><ul>")?;
        for attendee in attendees {
            write!(buffer, "<li>{} ({}) {}</li>", attendee.name, attendee.count, attendee.comment)?;
        }
        write!(buffer, "</ul>")?;
    }

    if let Some(comments) = event.comments.as_ref() {
        write!(buffer, "<h3>Comments</h3><ul>")?;
        for comment in comments {
            write!(buffer, "<li>{} ({}) {}</li>", comment.author, comment.date, comment.text)?;
        }
        write!(buffer, "</ul>")?;
    }

    Ok(buffer)
}
