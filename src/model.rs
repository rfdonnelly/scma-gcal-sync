#[derive(Debug, Default)]
pub struct Event {
    name: String,
    url: String,
    start_date: String,
    end_date: String,
    location: String,
    description: String,
    comments: Vec<Comment>,
    attendees: Vec<Attendee>,
}

#[derive(Debug, Default)]
pub struct Attendee {
    name: String,
    count: u8,
    comment: String,
}

#[derive(Debug, Default)]
pub struct Comment {
    author: String,
    timestamp: String,
    text: String,
}
