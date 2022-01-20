use crate::model::Event;

use chrono::Duration;
use futures::{stream, StreamExt, TryStreamExt};
use google_calendar3::{api, CalendarHub};
use tracing::{debug, info, trace};
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use std::fmt::Write;
use std::collections::HashSet;

pub struct GCal {
    calendar_id: String,
    hub: CalendarHub,
}

#[derive(Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum UserDiff {
    Add(String),
    Del(String),
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
        let (rsp, list) = hub.calendar_list().list().add_scope(SCOPE).doit().await?;
        trace!(?rsp, "calendar_list.list");
        debug!(?list, "calendar_list.list");
        let calendars = list.items.unwrap();

        let find_calendar = calendars
            .iter()
            .find(|entry| entry.summary.as_ref().unwrap() == calendar_name);
        let calendar_id = match find_calendar {
            Some(calendar) => {
                let calendar_id = calendar.id.as_ref().unwrap().clone();
                info!(%calendar_name, %calendar_id, "Found existing calendar");

                calendar_id
            }
            None => {
                info!(%calendar_name, "Calendar not found, inserting new calendar");

                let req = api::Calendar {
                    summary: Some(calendar_name.to_string()),
                    ..Default::default()
                };
                let (rsp, calendar) = hub.calendars().insert(req).add_scope(SCOPE).doit().await?;
                trace!(?rsp);
                debug!(?calendar);

                let calendar_id = calendar.id.as_ref().unwrap().clone();
                info!(%calendar_name, %calendar_id, "Inserted new calendar");

                calendar_id
            }
        };

        let gcal = Self { calendar_id, hub };

        Ok(gcal)
    }

    // Syncs emails with readers in calendar ACL
    pub async fn acl_sync(
        &self,
        emails: &[&str],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let acls = self.acl_list().await?;
        let diffs = Self::acl_diff(emails, &acls);

        for diff in diffs {
            match diff {
                UserDiff::Add(email) => self.acl_insert(&email, "reader").await?,
                UserDiff::Del(email) => self.acl_delete(&email).await?,
            }
        }

        Ok(())
    }

    /// Returns a diff between emails and readers (ACL rules with the reader role).
    pub fn acl_diff(
        emails: &[&str],
        rules: &[api::AclRule],
    ) -> Vec<UserDiff> {
        let readers: HashSet<String> = rules
            .iter()
            .filter(|rule| rule.role == Some("reader".to_string()))
            .map(|rule| rule.scope.as_ref().unwrap().value.as_ref().unwrap().to_string())
            .collect();
        let emails: HashSet<String> = emails.iter().map(|email| email.to_string()).collect();

        let to_add = emails.difference(&readers).map(|email| UserDiff::Add(email.to_string()));
        let to_del = readers.difference(&emails).map(|email| UserDiff::Del(email.to_string()));
        let diffs: Vec<UserDiff> = to_add.chain(to_del).collect();
        info!(?diffs);

        diffs
    }

    async fn acl_insert(
        &self,
        email: &str,
        role: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(%email, "Adding user");

        let req = api::AclRule {
            role: Some(role.to_string()),
            scope: Some(api::AclRuleScope {
                type_: Some("user".to_string()),
                value: Some(email.to_string()),
            }),
            ..Default::default()
        };
        let (rsp, rule) = self.hub.acl()
            .insert(req, &self.calendar_id)
            .send_notifications(true)
            .doit()
            .await?;
        trace!(?rsp);
        debug!(?rule);

        Ok(())
    }

    async fn acl_delete(
        &self,
        email: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(%email, "Deleting user");

        let rule_id = format!("user:{}", email);
        let rsp = self.hub.acl()
            .delete(&self.calendar_id, &rule_id)
            .doit()
            .await?;
        trace!(?rsp);

        Ok(())
    }

    /// Fetches entire ACL using one or more requests for pagination
    async fn acl_list(
        &self,
    ) -> Result<Vec<api::AclRule>, Box<dyn std::error::Error>> {
        let mut rules = Vec::new();
        let mut page_token = None;

        loop {
            let (mut next_rules, next_page_token) = self.acl_list_page(page_token).await?;
            rules.append(&mut next_rules);
            page_token = next_page_token;

            if page_token.is_none() { break; }
        }

        Ok(rules)
    }

    /// Fetches a single page of the ACL
    async fn acl_list_page(
        &self,
        page_token: Option<String>,
    ) -> Result<(Vec<api::AclRule>, Option<String>), Box<dyn std::error::Error>> {
        let call = self.hub.acl()
            .list(&self.calendar_id)
            .add_scope(SCOPE);
        let call = match page_token {
            Some(page_token) => call.page_token(&page_token),
            None => call,
        };
        let (rsp, acl) = call
            .doit()
            .await?;
        trace!(?rsp);
        debug!(?acl);

        Ok((acl.items.unwrap(), acl.next_page_token))
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
                let (rsp, g_event) = self
                    .hub
                    .events()
                    .insert(g_event, &self.calendar_id)
                    .add_scope(SCOPE)
                    .doit()
                    .await?;
                trace!(?rsp);
                debug!(?g_event);

                let link = g_event.html_link.as_ref().unwrap();
                info!(%event.id, %event, %link, "Inserted");
            }
            Ok(rsp) => {
                let (rsp, g_event) = rsp;
                trace!(?rsp);
                debug!(?g_event);

                let link = g_event.html_link.as_ref().unwrap();
                info!(%event.id, %event, %link, "Updated");
            }
        }

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

#[cfg(test)]
mod test {
    use super::*;

    use tap::prelude::*;

    #[test]
    fn acl_diff() {
        let emails = vec!["user0@example.com", "user1@example.com"];
        let rules = vec![
            api::AclRule {
                role: Some("ignored".to_string()),
                scope: Some(api::AclRuleScope {
                    type_: Some("user".to_string()),
                    value: Some("ignored@example.com".to_string()),
                }),
                ..Default::default()
            },
            api::AclRule {
                role: Some("reader".to_string()),
                scope: Some(api::AclRuleScope {
                    type_: Some("user".to_string()),
                    value: Some("user1@example.com".to_string()),
                }),
                ..Default::default()
            },
            api::AclRule {
                role: Some("reader".to_string()),
                scope: Some(api::AclRuleScope {
                    type_: Some("user".to_string()),
                    value: Some("user2@example.com".to_string()),
                }),
                ..Default::default()
            },
        ];
        let actual = GCal::acl_diff(&emails, &rules)
            .tap_mut(|diffs| diffs.sort());
        let expected = vec![
            UserDiff::Add("user0@example.com".to_string()),
            UserDiff::Del("user2@example.com".to_string()),
        ];
        assert_eq!(actual, expected);
    }
}
