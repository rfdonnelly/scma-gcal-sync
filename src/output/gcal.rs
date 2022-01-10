use crate::model::{
    Event,
};

use chrono::NaiveDate;
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

pub struct GCal<'a> {
    calendar_name: &'a str,
    client_secret_json_path: &'a str,
    oauth_token_json_path: &'a str,
    min_date: Option<NaiveDate>,
}

impl<'a> GCal<'a> {
    pub fn new(
        calendar_name: &'a str,
        client_secret_json_path: &'a str,
        oauth_token_json_path: &'a str,
        min_date: Option<NaiveDate>,
    ) -> Self {
        Self {
            calendar_name,
            client_secret_json_path,
            oauth_token_json_path,
            min_date,
        }
    }

    pub async fn write(&self, events: &[Event]) -> Result<(), Box<dyn std::error::Error>> {
        let secret = yup_oauth2::read_application_secret(self.client_secret_json_path).await?;

        let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
            .persist_tokens_to_disk(self.oauth_token_json_path)
            .build()
            .await?;

        let scopes = ["https://www.googleapis.com/auth/calendar"];

        let client = hyper::Client::builder()
            .build(hyper_rustls::HttpsConnector::with_native_roots());

        let hub = CalendarHub::new(client, auth);

        let (_, list) = hub
            .calendar_list()
            .list()
            .doit()
            .await?;

        let calendar_name = &self.calendar_name;
        let calendars = list.items.unwrap();
        let calender_entry = calendars
            .iter()
            .find(|entry| entry.summary.as_ref().unwrap() == self.calendar_name)
            .unwrap();

        let calendar_id = calender_entry.id.as_ref().unwrap();

        let (_, cal_events) = hub
            .events()
            .list(calendar_id)
            .doit()
            .await?;
        let cal_events = cal_events.items.unwrap();

        for event in events {
            let cal_event = CalEvent::try_from(event)?;

            let rsp = {
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
        let event_id = &id.clone();
        let summary = event_summary(&event);
        let start = event_start(&event);
        let end = event_end(&event);
        let description = event.description.clone();
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
