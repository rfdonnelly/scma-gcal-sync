use chrono::{NaiveDate, DateTime, Local};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Event {
    pub title: String,
    pub url: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub location: String,
    pub description: String,
    pub comments: Vec<Comment>,
    pub attendees: Vec<Attendee>,
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
