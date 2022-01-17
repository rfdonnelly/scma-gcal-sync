use chrono::{DateTime, FixedOffset, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize, Serializer};

use std::fmt;

#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub url: String,
    // SCMA JSON uses "date"
    #[serde(alias = "date")]
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    // SCMA JSON uses "venue"
    #[serde(alias = "venue")]
    pub location: String,
    pub description: String,
    // Not present in SCMA JSON
    #[serde(default)]
    pub comments: Option<Vec<Comment>>,
    // Not present in SCMA JSON
    #[serde(default)]
    pub attendees: Option<Vec<Attendee>>,
    /// The date and time the event page was downloaded.
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}/{})", self.title, self.start_date, self.end_date)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    #[serde(serialize_with = "serialize_datetime_pacific")]
    pub date: DateTime<Local>,
    pub text: String,
}

fn serialize_datetime_pacific<S>(dt: &DateTime<Local>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let pacific = FixedOffset::west(8 * 60 * 60);
    let s = dt.with_timezone(&pacific).to_rfc3339();
    serializer.serialize_str(&s)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Attendee {
    pub name: String,
    pub count: u8,
    pub comment: String,
}

/// Provides event selection by date
#[derive(Copy, Clone)]
pub enum DateSelect {
    /// All events
    All,
    /// Only present (in-progress) and future events
    NotPast,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub email: String,
}
