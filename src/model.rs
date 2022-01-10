use chrono::{NaiveDate, DateTime, Local};
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(rename(deserialize = "date"))]
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(rename(deserialize = "venue"))]
    pub location: String,
    pub description: String,
    #[serde(skip_deserializing)]
    pub comments: Option<Vec<Comment>>,
    #[serde(skip_deserializing)]
    pub attendees: Option<Vec<Attendee>>,
}

#[derive(Debug, Serialize)]
pub struct Comment {
    pub author: String,
    pub date: DateTime<Local>,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct Attendee {
    pub name: String,
    pub count: u8,
    pub comment: String,
}
