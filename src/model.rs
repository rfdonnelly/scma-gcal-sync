use chrono::{NaiveDate, DateTime, Local};
use serde::{Serialize, Deserialize};

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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub date: DateTime<Local>,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Attendee {
    pub name: String,
    pub count: u8,
    pub comment: String,
}
