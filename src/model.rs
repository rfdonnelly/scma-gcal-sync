use chrono::{DateTime, FixedOffset, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize, Serializer};

use std::fmt;
use std::str::FromStr;

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

#[derive(Debug, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberStatus {
    Applicant,
    Student,
    AM,
    HM,
    RM,
}

impl Default for MemberStatus {
    fn default() -> Self {
        Self::Applicant
    }
}

impl FromStr for MemberStatus {
    type Err = Box<dyn std::error::Error>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Applicant" => Ok(Self::Applicant),
            "Student" => Ok(Self::Student),
            "AM" => Ok(Self::AM),
            "HM" => Ok(Self::HM),
            "RM" => Ok(Self::RM),
            _ => Err(format!("unrecognized member status '{}'", s).into()),
        }
    }
}

impl fmt::Display for MemberStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applicant => write!(f, "Applicant"),
            Self::Student => write!(f, "Student"),
            Self::AM => write!(f, "AM"),
            Self::HM => write!(f, "HM"),
            Self::RM => write!(f, "RM"),
        }
    }
}

#[derive(Debug, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub enum TripLeaderStatus {
    G,
    S1,
    S2,
}

impl Default for TripLeaderStatus {
    fn default() -> Self {
        Self::G
    }
}

impl FromStr for TripLeaderStatus {
    type Err = Box<dyn std::error::Error>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "G" => Ok(Self::G),
            "S1" => Ok(Self::S1),
            "S2" => Ok(Self::S2),
            _ => Err(format!("unrecognized trip leader status '{}'", s).into()),
        }
    }
}

impl fmt::Display for TripLeaderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::G => write!(f, "G"),
            Self::S1 => write!(f, "S1"),
            Self::S2 => write!(f, "S2"),
        }
    }
}

#[derive(Debug, Default, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub member_status: MemberStatus,
    pub trip_leader_status: Option<TripLeaderStatus>,
    pub position: Option<String>,
    pub address: String,
    pub city: String,
    pub state: String,
    pub zipcode: String,
    pub phone: Option<String>,
    pub email: String,
}

impl User {
    pub fn name_email(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }

    pub fn address(&self) -> String {
        format!(
            "{}, {}, {} {}",
            self.address, self.city, self.state, self.zipcode
        )
    }
}
