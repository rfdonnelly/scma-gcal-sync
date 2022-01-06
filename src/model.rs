use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Event {
    pub title: String,
    pub url: String,
    pub start_date: String,
    pub end_date: String,
    pub location: String,
    pub description: String,
    pub comments: Vec<Comment>,
    pub attendees: Vec<Attendee>,
}

#[derive(Debug, Default, Serialize)]
pub struct Comment {
    pub author: String,
    pub date: String,
    pub text: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Attendee {
    pub name: String,
    pub count: u8,
    pub comment: String,
}
