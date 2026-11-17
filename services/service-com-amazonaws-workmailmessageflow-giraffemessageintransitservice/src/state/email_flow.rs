// SPDX-License-Identifier: CC0-1.0

//! How a message gets INTO transit: the email flow of one WorkMail organization,
//! which a message enters when mail that arrived for a mailbox of that organization is
//! handed to it.
//!
//! This API models no create: `GetRawMessageContent` and `PutRawMessageContent` act on a
//! message that is ALREADY in transit, and the thing that puts it there is WorkMail
//! delivery — mail arriving for a mailbox of an organization, which then runs that
//! organization's email flow and hands a synchronous Run Lambda rule the message's id.
//! So the producer is reached from the service that RECEIVED the mail, over the one
//! shared engine, and this module is the seam it comes in through: the caller says which
//! organization the mail was handed to, for which recipients, and with which bytes, and
//! what is decided here is whether that organization really holds a mailbox for one of
//! those recipients.
//!
//! Nothing here fabricates either side. The organization is AMAZON WORKMAIL's resource,
//! so it is resolved against WorkMail's own store in the shared engine — a client makes
//! it resolvable by calling `CreateOrganization`, `CreateUser` and `RegisterToWorkMail`,
//! and this reads what those writes left — and the bytes are the caller's, stored
//! verbatim as the content the two modelled operations then act on. The ARN the caller's
//! action names is read through the tree's one resource-ARN parse
//! ([`::service_common::arn`]) rather than split again here.
//!
//! Both halves are regional: an organization's ARN names the Region it was created in
//! and an in-transit message belongs to the message flow of exactly one Region
//! ([`super::MessageKey`]), so a message enters transit in the Region the delivering
//! request was made in and the ARN must be that Region's.
//!
//! LOCK ORDER: WorkMail's store is taken first and released before this service's store
//! is taken, so no guard of one is ever held while the other is acquired and a lock cycle
//! between the two is unreachable. This service's own handlers take this store and then
//! AMAZON S3's ([`super::referenced_object`]), so the one order over the three stores is
//! WorkMail -> this service -> Amazon S3.

use ::service_com_amazonaws_workmail_workmailservice::state as workmail;

use super::ComAmazonawsWorkmailmessageflowGiraffemessageintransitserviceState as FlowState;

/// The vendor field of a WorkMail resource ARN: the service namespace WorkMail publishes
/// (`smithy_query(query="service_info")` on WorkMail's model reports `arnNamespace`
/// `workmail`), which is what says an ARN names a WorkMail resource at all.
const WORKMAIL_VENDOR: &str = "workmail";

/// The resource type an organization's ARN names, which is what makes an ARN the address
/// of an organization rather than of another WorkMail resource.
const ORGANIZATION_RESOURCE_TYPE: &str = "organization";

/// One message handed to a WorkMail organization's email flow.
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    /// The Region the delivering request was made in, which is the message flow the
    /// message enters.
    pub region: &'a str,
    /// The ARN of the organization the mail was handed to.
    pub organization_arn: &'a str,
    /// The recipients the mail was accepted for. The message enters the flow when the
    /// organization holds a mailbox for one of them.
    pub recipients: &'a [String],
    /// The id the message is addressed by for as long as it is in transit.
    pub message_id: &'a str,
    /// The message's raw MIME bytes, as they arrived.
    pub content: &'a [u8],
}

/// What became of a [`Delivery`] — reported rather than logged here, so the wording of
/// the line an operator reads stays with the service that made the delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entered {
    /// The message is in transit in the email flow of the organization with this id, and
    /// is addressable by [`Delivery::message_id`] through this service's endpoint in
    /// [`Delivery::region`].
    InTransit { organization_id: String },
    /// No organization of this account is addressed by that ARN in that Region, so there
    /// is no email flow for the message to enter.
    UnknownOrganization,
    /// The organization is addressed, but it is not in a state that takes mail.
    OrganizationUnavailable { state: String },
    /// The organization holds no mailbox for any of the recipients, so the mail is for
    /// nobody in it.
    NoMailbox,
}

/// The two stores one delivery reads and writes, as owned handles.
///
/// Resolved once, by [`email_flow`], and cheap to clone and to hold: a delivery is
/// carried out behind the request that accepted the mail, which is a different task from
/// the one that resolved it.
#[derive(Clone)]
pub struct EmailFlow {
    /// This service's own store, which the message enters.
    messages: ::engine::ServiceState<FlowState>,
    /// AMAZON WORKMAIL's store, which holds the organization and its mailboxes.
    organizations: ::engine::ServiceState<workmail::ComAmazonawsWorkmailWorkmailserviceState>,
}

impl ::std::fmt::Debug for EmailFlow {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter.debug_struct("EmailFlow").finish_non_exhaustive()
    }
}

/// The email flow seam over the shared engine, or `None` when either store is
/// unreachable (the engine is not the shared in-memory one) — never a fresh store, so an
/// unreachable one is not reported as an organization that holds no mailbox.
pub fn email_flow<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<EmailFlow> {
    Some(EmailFlow {
        messages: super::service_state(engine)?,
        organizations: workmail::service_state(engine)?,
    })
}

impl EmailFlow {
    /// Hand `delivery` to the organization's email flow: the message is in transit once
    /// the organization really holds a mailbox for one of the recipients, and
    /// [`super::ComAmazonawsWorkmailmessageflowGiraffemessageintransitserviceState::message`]
    /// answers with it from that moment.
    ///
    /// Takes WorkMail's store and releases it before taking this service's, the lock
    /// order the module docs state.
    pub fn receive(&self, delivery: &Delivery<'_>) -> Entered {
        let Some(organization_id) = organization_id_of(delivery.organization_arn) else {
            return Entered::UnknownOrganization;
        };
        let holds_mailbox = {
            let store = self.organizations.lock();
            let Some(organization) = store
                .organizations
                .get(organization_id)
                // The id alone is not the address: the ARN also names the partition, the
                // Region and the account, so an ARN that differs in any of them is not
                // this organization's.
                .filter(|organization| organization.arn == delivery.organization_arn)
            else {
                return Entered::UnknownOrganization;
            };
            if organization.state != workmail::ORGANIZATION_STATE_ACTIVE {
                return Entered::OrganizationUnavailable {
                    state: organization.state.clone(),
                };
            }
            delivery
                .recipients
                .iter()
                .any(|recipient| holds_mailbox_for(organization, recipient))
        };
        if !holds_mailbox {
            return Entered::NoMailbox;
        }
        self.messages.lock().receive_in_transit(
            delivery.region,
            delivery.message_id,
            delivery.content.to_vec(),
        );
        Entered::InTransit {
            organization_id: organization_id.to_string(),
        }
    }
}

/// The `OrganizationId` the ARN `arn` addresses, or `None` for a value that is not the
/// ARN of a WorkMail organization at all.
///
/// Read through the tree's ONE resource-ARN parse
/// ([`::service_common::arn`]): the value has to be a well-formed ARN whose vendor is
/// WorkMail's own namespace and whose resource is an organization, and the id is the
/// resource id of that type — the form WorkMail's `CreateOrganization` mints and
/// `DescribeOrganization` reports. Only the id is read here; the rest of the ARN is
/// compared against the organization the id resolves to, so one spelling is never accepted
/// for another.
fn organization_id_of(arn: &str) -> Option<&str> {
    ::service_common::arn::parse_of(arn, WORKMAIL_VENDOR)?;
    ::service_common::arn::resource_id_of_type(arn, ORGANIZATION_RESOURCE_TYPE)
        .filter(|id| !id.is_empty())
}

/// Whether `organization` holds a mailbox for `recipient`.
///
/// A mailbox is an entity that is REGISTERED to WorkMail: `RegisterToWorkMail` "enables
/// the user, group, or resource" and is what gives it a mailbox and a primary email
/// address, so an entity that only exists carries no mail. The address is matched the way
/// WorkMail's own store keys its email index — the addr-spec, folded — so
/// `"Someone" <USER@example.com>` and `user@example.com` name one mailbox, and the
/// reading of the address is the shared one every surface of this tree reads a mail
/// address with.
fn holds_mailbox_for(organization: &workmail::Organization, recipient: &str) -> bool {
    let address = ::service_common::ses_identities::addr_spec(recipient).to_ascii_lowercase();
    organization
        .email_index
        .get(&address)
        .and_then(|entity_id| organization.users.get(entity_id))
        .is_some_and(|user| user.state == workmail::entity_state::ENABLED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is read out of an organization's ARN, and nothing else is read as one.
    #[test]
    fn only_an_organization_arn_names_an_organization() {
        // (case, the value, the id it addresses)
        let rows: [(&str, &str, Option<&str>); 10] = [
            (
                "an organization ARN",
                "arn:aws:workmail:us-east-1:123456789012:organization/m-0123456789abcdef0123456789abcdef",
                Some("m-0123456789abcdef0123456789abcdef"),
            ),
            (
                "an ARN in another Region and account",
                "arn:aws:workmail:eu-west-1:210987654321:organization/m-1",
                Some("m-1"),
            ),
            (
                "an ARN in another partition",
                "arn:aws-us-gov:workmail:us-gov-west-1:123456789012:organization/m-1",
                Some("m-1"),
            ),
            (
                "an ARN of another WorkMail resource",
                "arn:aws:workmail:us-east-1:123456789012:user/m-1/u-1",
                None,
            ),
            (
                "an organization ARN of another vendor",
                "arn:aws:workmailmessageflow:us-east-1:123456789012:organization/m-1",
                None,
            ),
            (
                "an organization ARN naming no id",
                "arn:aws:workmail:us-east-1:123456789012:organization/",
                None,
            ),
            ("an organization id on its own", "m-1", None),
            ("the empty value", "", None),
            (
                "an ARN with a field missing",
                "arn:aws:workmail:123456789012:organization/m-1",
                None,
            ),
            (
                "six fields that do not start with arn",
                "x:y:workmail:r:a:organization/m-1",
                None,
            ),
        ];

        for (case, value, expected) in rows {
            let read = organization_id_of(value);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
