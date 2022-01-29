use crate::model::User;
use crate::GAuth;

use google_people1::{api, PeopleService};
use tracing::{debug, info, trace};

use std::collections::{HashMap, HashSet};

const SCOPE: api::Scope = api::Scope::Contact;

const CONTACT_GROUPS_GET_MAX_MEMBERS: i32 = 999;
const PEOPLE_BATCH_CREATE_MAX_CONTACTS: usize = 50;
const PEOPLE_BATCH_GET_MAX_CONTACTS: usize = 50;
const GROUP_FIELDS: &str = "name";
const PERSON_FIELDS_GET: &str = "addresses,emailAddresses,names,phoneNumbers,userDefined";

/// Algorithm
///
/// 1. Find the ContactGroup.resourceName by name using the contactGroups.list API method
/// 2. Get the ContactGroup.memberResourceNames by ContactGroup.resourceName using the
///    contactGroups.get API method (may need to paginate, API doc doesn't set an upper bound)
/// 3. Get the Person.emailAddresses by Person.resourceName using the people.getBatchGet API method
///    (the max is 200, so need to make multiple requests)
/// 4. Diff user emails with people emails. Determine which need to be added, removed, or updated.
/// 5. Sync
///    * Add -- Use the people.batchCreateContacts API method to add.  TODO How should pre-existing
///      but untagged contacts be handled?  For example, person already exists in user's Google
///      Contacts but isn't a member of the named ContactGroup.  If we don't account for this, we
///      will end up adding a duplicate entry.  Do we just let this happen, then rely on Google
///      Contacts "Merge & fix" feature?
///    * Remove -- TODO determine the desired action.  Should old members be removed or moved to a
///      different ContactGroup (e.g. "SCMA Alumni")?  If moved, how do we handle case where membership
///      lapsed? For now, use the people.batchDeleteContacts API method to remove.
///
///      https://developers.google.com/people/api/rest/v1/people/batchDeleteContacts
///    * Update -- Use the people.batchUpdateContacts API method to update
pub struct GPpl {
    hub: PeopleService,
    /// The unique identifer for the ContactGroup assigned by the People API
    group_resource_name: String,
}

#[derive(Debug)]
struct PersonSyncOpsResult {
    inserts: Vec<User>,
    updates: Vec<(User, PersonWrapper)>,
    deletes: Vec<PersonWrapper>,
}

impl GPpl {
    pub async fn new(group_name: &str, auth: GAuth) -> Result<Self, Box<dyn std::error::Error>> {
        let hub = Self::create_hub(auth).await?;
        let group_resource_name =
            Self::contact_groups_get_or_create_by_name(&hub, group_name).await?;

        Ok(Self {
            hub,
            group_resource_name,
        })
    }

    pub async fn people_sync(&self, users: Vec<User>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Getting group member resource names");
        let member_resource_names = self
            .contact_groups_get_member_resource_names(&self.group_resource_name)
            .await?;
        info!(member_count=%member_resource_names.len(), "Got group member resource names");
        trace!(?member_resource_names);

        info!("Getting group member details");
        let members = if member_resource_names.is_empty() {
            Vec::new()
        } else {
            self.people_batch_get(&member_resource_names).await?
        };
        info!(member_count = members.len(), "Got group member details");
        trace!(?members);

        info!(user_count = users.len(), "Determining sync operations");
        let ops = Self::people_sync_ops(users, members);
        info!(
            inserts = ops.inserts.len(),
            updates = ops.updates.len(),
            deletes = ops.deletes.len(),
            "Determined sync operations"
        );
        trace!(?ops);

        let inserts: Vec<_> = ops.inserts.iter().map(User::name_email).collect();
        info!(count=%inserts.len(), "Adding people");
        self.people_batch_create(ops.inserts).await?;

        let updates: Vec<_> = ops
            .updates
            .iter()
            .map(|(user, _person)| user.name_email())
            .collect();
        info!(count=%updates.len(), "Updating people");
        // TODO ...

        let deletes: Vec<_> = ops.deletes.iter().map(PersonWrapper::name_email).collect();
        info!(count=%deletes.len(), ?deletes, "Deleting people");
        // TODO ...

        todo!("Do insert, update, and delete");

        Ok(())
    }

    async fn create_hub(gauth: GAuth) -> Result<PeopleService, Box<dyn std::error::Error>> {
        let scopes = [SCOPE];
        let token = gauth.auth().token(&scopes).await?;
        info!(expiration_time=?token.expiration_time(), "Got token");

        let client =
            hyper::Client::builder().build(hyper_rustls::HttpsConnector::with_native_roots());

        let hub = PeopleService::new(client, gauth.into());

        Ok(hub)
    }

    /// Returns the ContactGroup.resourceName of the named ContactGroup.
    ///
    /// If the named ContactGroup does not exist, a new ContactGroup will be created.
    async fn contact_groups_get_or_create_by_name(
        hub: &PeopleService,
        group_name: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        info!(%group_name, "Finding group");
        let (rsp, list) = hub
            .contact_groups()
            .list()
            .group_fields(GROUP_FIELDS)
            .add_scope(SCOPE)
            .doit()
            .await?;
        trace!(?rsp, "contact_groups.list");
        debug!(?list, "contact_groups.list");

        let groups = list.contact_groups.unwrap();
        let find_group = groups
            .iter()
            .find(|group| group.name.as_ref().unwrap() == group_name);
        let group_resource_name = match find_group {
            Some(group) => {
                let group_resource_name = group.resource_name.as_ref().unwrap().clone();
                info!(%group_name, %group_resource_name, "Found existing contact group");

                group_resource_name
            }
            None => {
                info!(%group_name, "Contact group not found, creating new contact group");

                let req = api::CreateContactGroupRequest {
                    contact_group: Some(api::ContactGroup {
                        name: Some(group_name.to_string()),
                        ..Default::default()
                    }),
                    read_group_fields: Some(GROUP_FIELDS.to_string()),
                };
                let (rsp, group) = hub
                    .contact_groups()
                    .create(req)
                    .add_scope(SCOPE)
                    .doit()
                    .await?;
                trace!(?rsp, "contact_groups.create");
                debug!(?group, "contact_groups.create");

                let group_resource_name = group.resource_name.as_ref().unwrap().clone();
                info!(%group_name, %group_resource_name, "Created new contact group");

                group_resource_name
            }
        };

        Ok(group_resource_name)
    }

    // Returns all Person.resource_names belonging to the given ContactGroup.resource_name
    async fn contact_groups_get_member_resource_names(
        &self,
        group_resource_name: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let (rsp, group) = self
            .hub
            .contact_groups()
            .get(&group_resource_name)
            .max_members(CONTACT_GROUPS_GET_MAX_MEMBERS)
            .group_fields(GROUP_FIELDS)
            .add_scope(SCOPE)
            .doit()
            .await?;
        trace!(?rsp);
        debug!(?group);

        let member_resource_names = group.member_resource_names.unwrap_or_default();

        Ok(member_resource_names)
    }

    // Returns name, email, and phone number for the given Person.resource_names
    async fn people_batch_get(
        &self,
        resource_names: &[String],
    ) -> Result<Vec<PersonWrapper>, Box<dyn std::error::Error>> {
        let mut people = Vec::new();
        let mut lower = 0;
        let mut upper = PEOPLE_BATCH_GET_MAX_CONTACTS.min(resource_names.len());

        loop {
            let (left, _) = resource_names.split_at(upper);
            let (_, chunk) = left.split_at(lower);

            info!(
                "Getting group member details {} to {} of {}",
                lower + 1,
                upper,
                resource_names.len()
            );
            let mut people_page = self.people_batch_get_page(chunk).await?;
            people.append(&mut people_page);

            if upper == resource_names.len() {
                break;
            }

            (lower, upper) = {
                let lower_next = upper;
                let upper_next = (upper + PEOPLE_BATCH_GET_MAX_CONTACTS).min(resource_names.len());
                (lower_next, upper_next)
            };
        }

        Ok(people)
    }

    async fn people_batch_get_page(
        &self,
        resource_names: &[String],
    ) -> Result<Vec<PersonWrapper>, Box<dyn std::error::Error>> {
        let mut builder = self
            .hub
            .people()
            .get_batch_get()
            .person_fields(PERSON_FIELDS_GET);

        for resource_name in resource_names {
            builder = builder.add_resource_names(resource_name);
        }

        let (rsp, get_people_response) = builder.add_scope(SCOPE).doit().await?;
        trace!(?rsp);
        debug!(?get_people_response);

        let people: Vec<api::Person> = get_people_response
            .responses
            .unwrap_or_default()
            .iter()
            .map(|person_response| person_response.person.clone().unwrap_or_default())
            .collect();

        let people = people
            .into_iter()
            .map(|person| {
                let resource_name = person.resource_name.as_ref().unwrap().clone();
                let name = person
                    .names
                    .as_ref()
                    .unwrap()
                    .iter()
                    .next()
                    .unwrap()
                    .display_name
                    .as_ref()
                    .unwrap()
                    .clone();
                let email = match person.email_addresses.as_ref() {
                    Some(emails) => {
                        let email = emails.iter().next().unwrap();
                        let email = email.value.as_ref().unwrap().clone();
                        Some(email)
                    }
                    None => None,
                };
                PersonWrapper {
                    resource_name,
                    name,
                    email,
                    person,
                }
            })
            .collect();

        Ok(people)
    }

    async fn people_batch_create(
        &self,
        users: Vec<User>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for users_chunk in users.chunks(PEOPLE_BATCH_CREATE_MAX_CONTACTS) {
            info!(people=?users_chunk.iter().map(User::name_email).collect::<Vec<String>>(), "Adding people");
            let people: Vec<api::Person> = users_chunk
                .into_iter()
                .map(|user| create_person(user, &self.group_resource_name))
                .collect();
            let contacts = people
                .into_iter()
                .map(|person| api::ContactToCreate {
                    contact_person: Some(person),
                })
                .collect();
            let req = api::BatchCreateContactsRequest {
                contacts: Some(contacts),
                ..Default::default()
            };
            info!(req=%serde_json::to_string(&req)?);
            let (rsp, batch_create_contacts) = self
                .hub
                .people()
                .batch_create_contacts(req)
                .add_scope(SCOPE)
                .doit()
                .await?;
            trace!(?rsp);
            debug!(?batch_create_contacts);
        }

        Ok(())
    }

    /// People w/o an email are ignored.
    ///
    /// This effectively performs a diff from People to Users.
    fn people_sync_ops(users: Vec<User>, people: Vec<PersonWrapper>) -> PersonSyncOpsResult {
        let mut users: HashMap<String, User> = users
            .into_iter()
            .map(|user| (user.email.clone(), user))
            .collect();
        let mut people: HashMap<String, PersonWrapper> = people
            .into_iter()
            .filter_map(|person| {
                if let Some(ref email) = person.email {
                    Some((email.clone(), person))
                } else {
                    None
                }
            })
            .collect();

        let user_emails: HashSet<String> = users.keys().map(|email| email.clone()).collect();
        let person_emails: HashSet<String> = people.keys().map(|email| email.clone()).collect();

        let inserts: Vec<_> = user_emails
            .difference(&person_emails)
            .map(|email| {
                let user = users.remove(&email.to_string()).unwrap();
                user
            })
            .collect();
        let deletes: Vec<_> = person_emails
            .difference(&user_emails)
            .map(|email| {
                let person = people.remove(&email.to_string()).unwrap();
                person
            })
            .collect();
        let updates: Vec<_> = user_emails
            .intersection(&person_emails)
            .map(|email| {
                let user = users.remove(&email.to_string()).unwrap();
                let person = people.remove(&email.to_string()).unwrap();
                (user, person)
            })
            .collect();

        let ops = PersonSyncOpsResult {
            inserts,
            updates,
            deletes,
        };

        ops
    }
}

#[derive(Debug, Default, Clone)]
struct PersonWrapper {
    resource_name: String,
    name: String,
    email: Option<String>,
    person: api::Person,
}

impl PersonWrapper {
    fn name_email(&self) -> String {
        match &self.email {
            Some(email) => format!("{} <{}>", self.name, email),
            None => self.name.clone(),
        }
    }
}

fn create_person(user: &User, group_resource_name: &str) -> api::Person {
    let name = api::Name {
        unstructured_name: Some(user.name.clone()),
        ..Default::default()
    };
    let email_address = api::EmailAddress {
        type_: Some("SCMA".to_string()),
        value: Some(user.email.clone()),
        ..Default::default()
    };
    let address = api::Address {
        type_: Some("SCMA".to_string()),
        formatted_value: Some(user.address()),
        ..Default::default()
    };
    let phone_number = api::PhoneNumber {
        type_: Some("SCMA".to_string()),
        value: user.phone.clone(),
        ..Default::default()
    };
    let membership = api::Membership {
        contact_group_membership: Some(api::ContactGroupMembership {
            contact_group_resource_name: Some(group_resource_name.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let member_status = api::UserDefined {
        key: Some("SCMA Member Status".to_string()),
        value: Some(user.member_status.to_string()),
        ..Default::default()
    };
    let trip_leader_status = user
        .trip_leader_status
        .as_ref()
        .map(|status| status.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let trip_leader_status = api::UserDefined {
        key: Some("SCMA Trip Leader Status".to_string()),
        value: Some(trip_leader_status),
        ..Default::default()
    };
    let position = user.position.as_ref().map(|position| position.to_string()).unwrap_or_else(|| "n/a".to_string());
    let position = api::UserDefined {
        key: Some("SCMA Position".to_string()),
        value: Some(position),
        ..Default::default()
    };

    api::Person {
        names: Some(vec![name]),
        email_addresses: Some(vec![email_address]),
        addresses: Some(vec![address]),
        phone_numbers: Some(vec![phone_number]),
        memberships: Some(vec![membership]),
        user_defined: Some(vec![member_status, trip_leader_status, position]),
        ..Default::default()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    impl PartialEq for PersonSyncOpsResult {
        fn eq(&self, other: &Self) -> bool {
            self.inserts == other.inserts
                && self.updates == other.updates
                && self.deletes == other.deletes
        }
    }

    impl PartialEq for PersonWrapper {
        fn eq(&self, other: &Self) -> bool {
            self.name == other.name
                && self.email == other.email
        }
    }

    #[test]
    fn people_sync_ops() {
        let users = vec![
            User {
                name: "User 0".to_string(),
                email: "user0@example.com".to_string(),
                ..Default::default()
            },
            User {
                name: "User 1".to_string(),
                email: "user1@example.com".to_string(),
                ..Default::default()
            },
        ];

        let people = vec![
            PersonWrapper {
                name: "User 1".to_string(),
                email: Some("user1@example.com".to_string()),
                ..Default::default()
            },
            PersonWrapper {
                name: "User 2".to_string(),
                email: Some("user2@example.com".to_string()),
                ..Default::default()
            },
        ];

        let actual = GPpl::people_sync_ops(users, people);
        let expected = PersonSyncOpsResult {
            inserts: vec![User {
                name: "User 0".to_string(),
                email: "user0@example.com".to_string(),
                ..Default::default()
            }],
            updates: vec![(
                User {
                    name: "User 1".to_string(),
                    email: "user1@example.com".to_string(),
                    ..Default::default()
                },
                PersonWrapper {
                    name: "User 1".to_string(),
                    email: Some("user1@example.com".to_string()),
                    ..Default::default()
                },
            )],
            deletes: vec![PersonWrapper {
                name: "User 2".to_string(),
                email: Some("user2@example.com".to_string()),
                ..Default::default()
            }],
        };
        assert_eq!(actual, expected);
    }
}
