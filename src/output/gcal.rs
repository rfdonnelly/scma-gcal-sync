use crate::model::Event;
use crate::GAuth;

use chrono::Duration;
use futures::{stream, StreamExt, TryStreamExt};
use google_calendar3::{api, CalendarHub};
use tracing::{debug, info, trace};

use std::collections::HashSet;
use std::fmt::Write;

pub struct GCal {
    calendar_id: String,
    hub: CalendarHub,
    dry_run: bool,
    notify_acl_insert: bool,
}

#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub enum AclSyncOp {
    Insert(String),
    Delete(String),
}

// To enable named argument
#[derive(Clone, Copy)]
struct SendNotifications(bool);

impl From<SendNotifications> for bool {
    fn from(s: SendNotifications) -> bool {
        s.0
    }
}

impl From<bool> for SendNotifications {
    fn from(b: bool) -> Self {
        Self(b)
    }
}

const CALENDAR_DESCRIPTION: &str = "This calendar is synced daily with the SCMA event calendar (https://www.rockclimbing.org/index.php/event-list/events-list) by scma-gcal-sync (https://github.com/rfdonnelly/scma-gcal-sync).";
const DESCRIPTION_BUFFER_SIZE: usize = 4098;
const CONCURRENT_REQUESTS: usize = 3;
/// The number of concurrent ACL insert/delete requests to make.  Experienced rate limiting with a
/// value of 3.
const CONCURRENT_REQUESTS_ACL: usize = 1;
const SCOPE: api::Scope = api::Scope::Full;

impl GCal {
    pub async fn new(
        calendar_name: &str,
        calendar_owners: &[String],
        auth: GAuth,
        dry_run: bool,
        notify_acl_insert: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hub = Self::create_hub(auth).await?;
        let calendar_id =
            Self::calendars_get_or_insert_by_name(&hub, calendar_name, dry_run).await?;

        let gcal = Self {
            calendar_id,
            hub,
            dry_run,
            notify_acl_insert,
        };

        for calendar_owner in calendar_owners {
            gcal.acl_insert(calendar_owner, "owner", SendNotifications(false))
                .await?;
        }

        Ok(gcal)
    }

    async fn create_hub(gauth: GAuth) -> Result<CalendarHub, Box<dyn std::error::Error>> {
        let scopes = [SCOPE];
        let token = gauth.auth().token(&scopes).await?;
        info!(expiration_time=?token.expiration_time(), "Got token");

        let client =
            hyper::Client::builder().build(hyper_rustls::HttpsConnector::with_native_roots());

        let hub = CalendarHub::new(client, gauth.into());

        Ok(hub)
    }

    /// Returns the Calendar.id of the named calendar.
    ///
    /// If named calendar does not exist, a new calendar will be created.
    async fn calendars_get_or_insert_by_name(
        hub: &CalendarHub,
        calendar_name: &str,
        dry_run: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
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

                let calendar_id = if dry_run {
                    return Err("Cannot create calendar during dry run".into());
                } else {
                    let req = api::Calendar {
                        summary: Some(calendar_name.to_string()),
                        description: Some(CALENDAR_DESCRIPTION.to_string()),
                        ..Default::default()
                    };
                    let (rsp, calendar) =
                        hub.calendars().insert(req).add_scope(SCOPE).doit().await?;
                    trace!(?rsp, "calendars.insert");
                    debug!(?calendar, "calendars.insert");

                    calendar.id.as_ref().unwrap().clone()
                };

                info!(%calendar_name, %calendar_id, "Inserted new calendar");

                calendar_id
            }
        };

        Ok(calendar_id)
    }

    // Syncs emails with readers in calendar ACL
    pub async fn acl_sync(&self, emails: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let acls = self.acl_list().await?;
        let ops = Self::acl_sync_ops(emails, &acls);

        stream::iter(ops)
            .map(|op| self.acl_insert_or_delete(op))
            .buffer_unordered(CONCURRENT_REQUESTS_ACL)
            .try_collect::<Vec<_>>()
            .await?;

        Ok(())
    }

    async fn acl_insert_or_delete(&self, op: AclSyncOp) -> Result<(), Box<dyn std::error::Error>> {
        match op {
            AclSyncOp::Insert(email) => {
                self.acl_insert(&email, "reader", self.notify_acl_insert.into())
                    .await?
            }
            AclSyncOp::Delete(email) => self.acl_delete(&email).await?,
        }

        Ok(())
    }

    /// Returns a list of operations that need to be performed on the ACL to bring the ACL in sync
    /// with a set of user emails.
    ///
    /// Operates on the "reader" role only.
    ///
    /// This effectively performs a diff from readers to emails.
    ///
    /// For example, if given the set of emails:
    ///
    /// * user0@example.com
    /// * user1@example.com
    ///
    /// And the set of readers:
    ///
    /// * user1@example.com
    /// * user2@example.com
    ///
    /// To bring the readers in sync with the emails, the following operations would need to be
    /// performed on the readers:
    ///
    /// * Insert user0@example.com
    /// * Delete user2@example.com
    fn acl_sync_ops(emails: &[&str], rules: &[api::AclRule]) -> Vec<AclSyncOp> {
        let readers: HashSet<String> = rules
            .iter()
            .filter(|rule| rule.role == Some("reader".to_string()))
            .map(|rule| {
                rule.scope
                    .as_ref()
                    .unwrap()
                    .value
                    .as_ref()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let emails: HashSet<String> = emails.iter().map(|email| email.to_string()).collect();

        let inserts = emails
            .difference(&readers)
            .map(|email| AclSyncOp::Insert(email.to_string()));
        let deletes = readers
            .difference(&emails)
            .map(|email| AclSyncOp::Delete(email.to_string()));
        let ops: Vec<AclSyncOp> = inserts.chain(deletes).collect();
        info!(?ops);

        ops
    }

    async fn acl_insert(
        &self,
        email: &str,
        role: &str,
        send_notifications: SendNotifications,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(%email, %role, send_notifications=%bool::from(send_notifications), "Adding user");

        let req = api::AclRule {
            role: Some(role.to_string()),
            scope: Some(api::AclRuleScope {
                type_: Some("user".to_string()),
                value: Some(email.to_string()),
            }),
            ..Default::default()
        };
        if !self.dry_run {
            let (rsp, rule) = self
                .hub
                .acl()
                .insert(req, &self.calendar_id)
                .send_notifications(send_notifications.into())
                .doit()
                .await?;
            trace!(?rsp, "acl.insert");
            debug!(?rule, "acl.insert");
        }

        Ok(())
    }

    async fn acl_delete(&self, email: &str) -> Result<(), Box<dyn std::error::Error>> {
        info!(%email, "Deleting user");

        let rule_id = format!("user:{}", email);
        if !self.dry_run {
            let rsp = self
                .hub
                .acl()
                .delete(&self.calendar_id, &rule_id)
                .doit()
                .await?;
            trace!(?rsp, "acl.delete");
        }

        Ok(())
    }

    /// Fetches entire ACL by fetching all pages of the ACL
    async fn acl_list(&self) -> Result<Vec<api::AclRule>, Box<dyn std::error::Error>> {
        let mut rules = Vec::new();
        let mut page_token = None;

        loop {
            let (mut next_rules, next_page_token) = self.acl_list_page(page_token).await?;
            rules.append(&mut next_rules);
            page_token = next_page_token;

            if page_token.is_none() {
                break;
            }
        }

        Ok(rules)
    }

    /// Fetches a single page of the ACL
    async fn acl_list_page(
        &self,
        page_token: Option<String>,
    ) -> Result<(Vec<api::AclRule>, Option<String>), Box<dyn std::error::Error>> {
        let call = self.hub.acl().list(&self.calendar_id).add_scope(SCOPE);
        let call = match page_token {
            Some(page_token) => call.page_token(&page_token),
            None => call,
        };
        let (rsp, acl) = call.doit().await?;
        trace!(?rsp, "acl.list");
        debug!(?acl, "acl.list");

        Ok((acl.items.unwrap(), acl.next_page_token))
    }

    pub async fn write(&self, events: &[Event]) -> Result<(), Box<dyn std::error::Error>> {
        stream::iter(events)
            .map(|event| self.events_patch_or_insert(event))
            .buffer_unordered(CONCURRENT_REQUESTS)
            .try_collect::<Vec<_>>()
            .await?;

        Ok(())
    }

    pub async fn events_patch_or_insert(
        &self,
        event: &Event,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let g_event = api::Event::try_from(event)?;

        let event_id = g_event.id.as_ref().unwrap().clone();
        if !self.dry_run {
            let result = self
                .hub
                .events()
                .patch(g_event.clone(), &self.calendar_id, &event_id)
                .add_scope(SCOPE)
                .doit()
                .await;
            match result {
                Ok(rsp) => {
                    let (rsp, g_event) = rsp;
                    trace!(?rsp, "events.patch");
                    debug!(?g_event, "events.patch");

                    let link = g_event.html_link.as_ref().unwrap();
                    info!(%event.id, %event, %link, "Updated");
                }
                Err(_) => {
                    let (rsp, g_event) = self
                        .hub
                        .events()
                        .insert(g_event, &self.calendar_id)
                        .add_scope(SCOPE)
                        .doit()
                        .await?;
                    trace!(?rsp, "events.insert");
                    debug!(?g_event, "events.insert");

                    let link = g_event.html_link.as_ref().unwrap();
                    info!(%event.id, %event, %link, "Inserted");
                }
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
    fn acl_sync_ops() {
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
        let actual = GCal::acl_sync_ops(&emails, &rules).tap_mut(|ops| ops.sort());
        let expected = vec![
            AclSyncOp::Insert("user0@example.com".to_string()),
            AclSyncOp::Delete("user2@example.com".to_string()),
        ];
        assert_eq!(actual, expected);
    }
}
