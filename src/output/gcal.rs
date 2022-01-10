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

        // for event in cal_events {
        //     let event_id = event.id.unwrap();
        //     let event_summary = event.summary.unwrap();
        //     info!(%event_summary, "Deleting event");
        //     hub.events().delete(calendar_id, &event_id).doit().await?;
        // }

        for event in events {
            let id: u32 = event.id.parse()?;
            let id = format!("scma1{:05}", id);
            let event_id = &id.clone();
            let summary = format!("SCMA: {}", event.title);
            let start = EventDateTime {
                date: Some(event.start_date.to_string()),
                ..Default::default()
            };
            let end = EventDateTime {
                date: Some(event.end_date.to_string()),
                ..Default::default()
            };

            let cal_event = CalEvent {
                id: Some(id),
                summary: Some(summary),
                start: Some(start),
                end: Some(end),
                description: Some(event.description.clone()),
                location: Some(event.location.clone()),
                ..Default::default()
            };

            let rsp = {
                let result = hub.events().patch(cal_event.clone(), calendar_id, event_id).doit().await;
                match result {
                    Err(_) => {
                        let rsp = hub.events().insert(cal_event, calendar_id).doit().await?;
                        info!("Inserted event={:#?}", rsp.1);
                        rsp
                    }
                    Ok(rsp) => {
                        info!("Updated event={:#?}", rsp.1);
                        rsp
                    }
                }
            };
        }

        Ok(())
    }
}
