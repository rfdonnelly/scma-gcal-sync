use crate::model::User;
use crate::GAuth;

use google_people1::{api, PeopleService};
use indexmap::IndexMap;
use tap::prelude::*;
use tracing::{debug, info, trace};

use std::collections::{HashMap, HashSet};

const SCOPE: api::Scope = api::Scope::Contact;

const CONTACT_GROUPS_GET_MAX_MEMBERS: i32 = 999;
const PEOPLE_BATCH_CREATE_MAX_CONTACTS: usize = 50;
const PEOPLE_BATCH_GET_MAX_CONTACTS: usize = 50;
const PEOPLE_BATCH_UPDATE_MAX_CONTACTS: usize = 50;
const GROUP_FIELDS: &str = "name";
const PERSON_FIELDS_GET: &str = "addresses,emailAddresses,names,phoneNumbers,userDefined";
const PERSON_FIELDS_UPDATE: &str = "addresses,phoneNumbers,userDefined";

const SCMA_MEMBER_STATUS_KEY: &str = "SCMA Member Status";
const SCMA_TRIP_LEADER_STATUS_KEY: &str = "SCMA Trip Leader Status";
const SCMA_POSITION_KEY: &str = "SCMA Position";

/// Synchronizes SCMA members with Google Contacts using the algorithm below.
///
/// 1. Find the ContactGroup.resourceName by name using the contactGroups.list API method
///
/// 2. Get the ContactGroup.memberResourceNames by ContactGroup.resourceName using the
///    contactGroups.get API method (may need to paginate, API doc doesn't set an upper bound)
///
/// 3. Get the Person.emailAddresses by Person.resourceName using the people.getBatchGet API method
///    (the max is 200, so need to make multiple requests)
///
/// 4. Diff user emails with people emails. Determine which need to be added, updated, or removed.
///
/// 5. Sync
///
///    * Add -- Use the people.batchCreateContacts API method to add.
///
///      NOTE: Pre-existing contacts not part of the named ContactGroup will not be accounted for
///      during synchronization and will therefore be added instead of updated.  The Google
///      Contacts "Merge & fix" feature can be used to reconcile any duplicate contacts this may
///      create.
///
///    * Update -- Use the people.batchUpdateContacts API method to update.
///
///      First, merge SCMA User data with Google People Person obtained by the people.getBatchGet
///      API. Then, make one or more calls to people.batchUpdateContacts.
///
///      TODO?: A update is performed whether an update needs to be performed. This could be
///      improved by only updating Persons that need an update.
///
///    * Remove -- Do nothing.
///
///      Currently, nothing is done for Persons that exist in the Google People ContactGroup that
///      do not or no longer exist in the SCMA.
///
///      TODO?: Add an option to delete these contacts using the people.batchDeleteContacts?
///      TODO?: Move these contacts to a different ContactGroup (e.g. "SCMA Alumni")?
pub struct GPpl {
    hub: PeopleService,
    /// The unique identifer for the ContactGroup assigned by the People API
    group_resource_name: String,
    dry_run: bool,
}

#[derive(Debug)]
struct PersonSyncOpsResult {
    inserts: Vec<User>,
    updates: Vec<(User, PersonWrapper)>,
    deletes: Vec<PersonWrapper>,
}

impl GPpl {
    pub async fn new(
        group_name: &str,
        auth: GAuth,
        dry_run: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hub = Self::create_hub(auth).await?;
        let group_resource_name =
            Self::contact_groups_get_or_create_by_name(&hub, group_name).await?;

        Ok(Self {
            hub,
            group_resource_name,
            dry_run,
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
            ignores = ops.deletes.len(),
            "Determined sync operations"
        );
        trace!(?ops);

        info!(count=%ops.inserts.len(), "Adding people");
        self.people_batch_create(ops.inserts).await?;

        info!(count=%ops.updates.len(), "Updating people");
        let people = self.people_batch_update_ops(ops.updates);
        self.people_batch_update(people).await?;

        let ignores: Vec<_> = ops.deletes.iter().map(PersonWrapper::name_email).collect();
        info!(count=%ignores.len(), ?ignores, "Ignoring people found in Google Contacts but not a current member of the SCMA");

        Ok(())
    }

    fn people_batch_update_ops(&self, updates: Vec<(User, PersonWrapper)>) -> Vec<PersonWrapper> {
        updates
            .into_iter()
            .map(|(user, person)| person.update(user))
            .collect()
    }

    async fn people_batch_update(
        &self,
        people: Vec<PersonWrapper>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for people_chunk in people.chunks(PEOPLE_BATCH_UPDATE_MAX_CONTACTS) {
            let contacts = people_chunk
                .iter()
                .map(|person| (person.resource_name.clone(), person.person.clone()))
                .collect();
            let req = api::BatchUpdateContactsRequest {
                contacts: Some(contacts),
                read_mask: Some(PERSON_FIELDS_GET.to_string()),
                update_mask: Some(PERSON_FIELDS_UPDATE.to_string()),
                ..Default::default()
            };

            info!(
            count=people_chunk.len(),
            people=?people_chunk.iter().map(PersonWrapper::name_email).collect::<Vec<String>>(),
            "Updating contacts"
            );
            if !self.dry_run {
                let (rsp, update_response) = self
                    .hub
                    .people()
                    .batch_update_contacts(req)
                    .add_scope(SCOPE)
                    .doit()
                    .await?;
                trace!(?rsp, "people.batchUpdateContacts");
                debug!(?update_response, "people.batchUpdateContacts");
            }
        }

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
            .get(group_resource_name)
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

            lower = upper;
            upper = (lower + PEOPLE_BATCH_GET_MAX_CONTACTS).min(resource_names.len());
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

        let people = get_people_response
            .responses
            .unwrap_or_default()
            .into_iter()
            .map(|person_response| person_response.person.unwrap_or_default())
            .map(PersonWrapper::from)
            .collect();

        Ok(people)
    }

    async fn people_batch_create(
        &self,
        users: Vec<User>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for users_chunk in users.chunks(PEOPLE_BATCH_CREATE_MAX_CONTACTS) {
            info!(people=?users_chunk.iter().map(User::name_email).collect::<Vec<String>>(), "Adding people");
            let contacts = users_chunk
                .iter()
                .map(|user| create_api_person(user, &self.group_resource_name))
                .map(|person| api::ContactToCreate {
                    contact_person: Some(person),
                })
                .collect();
            let req = api::BatchCreateContactsRequest {
                contacts: Some(contacts),
                ..Default::default()
            };
            if !self.dry_run {
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

        let user_emails: HashSet<String> = HashSet::from_iter(users.keys().cloned());
        let person_emails: HashSet<String> = HashSet::from_iter(people.keys().cloned());

        let inserts: Vec<_> = user_emails
            .difference(&person_emails)
            .map(|email| users.remove(&email.to_string()).unwrap())
            .collect();
        let deletes: Vec<_> = person_emails
            .difference(&user_emails)
            .map(|email| people.remove(&email.to_string()).unwrap())
            .collect();
        let updates: Vec<_> = user_emails
            .intersection(&person_emails)
            .map(|email| {
                let user = users.remove(&email.to_string()).unwrap();
                let person = people.remove(&email.to_string()).unwrap();
                (user, person)
            })
            .collect();

        PersonSyncOpsResult {
            inserts,
            updates,
            deletes,
        }
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

    /// Updates self with user information.
    ///
    /// The Google People people.updateContact and people.batchUpdateContacts APIs overwrite the
    /// fields (and everything below them) specified via the FieldMask.  Because of this, we need to
    /// first get the fields we want to update then perform a manual merge of existing data and the
    /// data we want to update (or insert). In other words, we need to perform a read-modify-write.
    ///
    /// This function performs the merging of the SCMA User into the Google People Person.
    ///
    /// For each merge field, it first attempts to find existing entries by type or key.  If an
    /// existing entry is found, it's value is overwritten.  If an existing entry is not found, one is
    /// inserted.
    ///
    /// The following information is updated:
    ///
    /// * Phone number
    /// * Address
    /// * Member status
    /// * Trip leader status
    /// * Position
    ///
    /// The following information is _not_ updated:
    ///
    /// * Membership
    ///
    ///   The person was found via their membership to the group_resource_name and
    ///   therefore their membership is already as desired.
    ///
    /// * Name
    ///
    ///   Prefer the name in Google Contacts.
    ///
    /// * Email
    ///
    ///   The person-user pair was matched via their email and therefore the email is already as
    ///   desired.
    fn update(mut self, user: User) -> Self {
        let new_phone_number = create_api_phone_number(&user);
        self.person.phone_numbers =
            person_phone_numbers_update_or_insert(new_phone_number, self.person.phone_numbers);

        let new_address = create_api_address(&user);
        self.person.addresses =
            person_addresses_update_or_insert(new_address, self.person.addresses);

        self.person.user_defined =
            person_user_defined_update_or_insert(&user, self.person.user_defined);

        self
    }
}

impl From<api::Person> for PersonWrapper {
    fn from(person: api::Person) -> Self {
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
                // Use SCMA email if available otherwise use first email
                let find_type = Some("SCMA".to_string());
                let find_result = emails.iter().find(|email| email.type_ == find_type);
                match find_result {
                    Some(email) => email.value.clone(),
                    None => emails.first().unwrap().value.clone(),
                }
            }
            None => None,
        };

        Self {
            resource_name,
            name,
            email,
            person,
        }
    }
}

fn create_api_address(user: &User) -> api::Address {
    api::Address {
        type_: Some("SCMA".to_string()),
        formatted_value: Some(user.address()),
        ..Default::default()
    }
}

fn create_api_phone_number(user: &User) -> api::PhoneNumber {
    api::PhoneNumber {
        type_: Some("SCMA".to_string()),
        value: user.phone.clone(),
        ..Default::default()
    }
}

fn create_api_member_status(user: &User) -> api::UserDefined {
    api::UserDefined {
        key: Some(SCMA_MEMBER_STATUS_KEY.to_string()),
        value: Some(user.member_status.to_string()),
        ..Default::default()
    }
}

fn create_api_trip_leader_status(user: &User) -> api::UserDefined {
    api::UserDefined {
        key: Some(SCMA_TRIP_LEADER_STATUS_KEY.to_string()),
        value: Some(user.trip_leader_status()),
        ..Default::default()
    }
}

fn create_api_position(user: &User) -> api::UserDefined {
    api::UserDefined {
        key: Some(SCMA_POSITION_KEY.to_string()),
        value: Some(user.position()),
        ..Default::default()
    }
}

impl User {
    fn trip_leader_status(&self) -> String {
        self.trip_leader_status
            .as_ref()
            .map(|status| status.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    }

    fn position(&self) -> String {
        self.position
            .as_ref()
            .map(|position| position.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    }
}

fn create_api_person(user: &User, group_resource_name: &str) -> api::Person {
    let name = api::Name {
        unstructured_name: Some(user.name.clone()),
        ..Default::default()
    };
    let email_address = api::EmailAddress {
        type_: Some("SCMA".to_string()),
        value: Some(user.email.clone()),
        ..Default::default()
    };
    let address = create_api_address(user);
    let phone_number = create_api_phone_number(user);
    let membership = api::Membership {
        contact_group_membership: Some(api::ContactGroupMembership {
            contact_group_resource_name: Some(group_resource_name.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let member_status = create_api_member_status(user);
    let trip_leader_status = create_api_trip_leader_status(user);
    let position = create_api_position(user);

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

fn person_phone_numbers_update_or_insert(
    new_phone_number: api::PhoneNumber,
    phone_numbers: Option<Vec<api::PhoneNumber>>,
) -> Option<Vec<api::PhoneNumber>> {
    match phone_numbers {
        None => Some(vec![new_phone_number]),
        Some(mut phone_numbers) => {
            let find_result = phone_numbers
                .iter_mut()
                .find(|phone_number| phone_number.type_ == new_phone_number.type_);

            match find_result {
                // Update
                Some(phone_number) => *phone_number = new_phone_number,
                // Or insert
                None => phone_numbers.push(new_phone_number),
            }

            Some(phone_numbers)
        }
    }
}

fn person_addresses_update_or_insert(
    new_address: api::Address,
    addresses: Option<Vec<api::Address>>,
) -> Option<Vec<api::Address>> {
    match addresses {
        None => Some(vec![new_address]),
        Some(mut addresses) => {
            let find_result = addresses
                .iter_mut()
                .find(|address| address.type_ == new_address.type_);

            match find_result {
                // Update
                Some(address) => *address = new_address,
                // Or insert
                None => addresses.push(new_address),
            }

            Some(addresses)
        }
    }
}

fn person_user_defined_update_or_insert(
    user: &User,
    user_defined: Option<Vec<api::UserDefined>>,
) -> Option<Vec<api::UserDefined>> {
    let user_defined = match user_defined {
        Some(user_defined) => user_defined
            .into_iter()
            .map(|user_defined| {
                (
                    user_defined.key.unwrap_or_default(),
                    user_defined.value.unwrap_or_default(),
                )
            })
            .collect::<IndexMap<_, _>>(),
        None => IndexMap::new(),
    }
    .tap_mut(|user_defined| {
        user_defined.insert(
            SCMA_MEMBER_STATUS_KEY.to_string(),
            user.member_status.to_string(),
        );
        user_defined.insert(
            SCMA_TRIP_LEADER_STATUS_KEY.to_string(),
            user.trip_leader_status(),
        );
        user_defined.insert(SCMA_POSITION_KEY.to_string(), user.position());
    })
    .into_iter()
    .map(|(k, v)| api::UserDefined {
        key: Some(k),
        value: Some(v),
        ..Default::default()
    })
    .collect();

    Some(user_defined)
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
            self.name == other.name && self.email == other.email
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
