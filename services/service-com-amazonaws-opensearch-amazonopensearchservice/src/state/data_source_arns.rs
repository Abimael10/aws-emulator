// SPDX-License-Identifier: CC0-1.0

//! What an ARN a request hands this service as a DATA SOURCE has to name — the ONE
//! statement of those rules, so every operation that takes such an ARN judges it alike.
//!
//! Two things about an ARN are readable from the value itself, before anything is looked
//! up, and both decide whether the request can be served at all:
//!
//! * WHOSE resource it names. Every ARN carries the account that holds the resource, and
//!   this service serves one account: a value naming another account names a resource the
//!   caller cannot reach, so the request is refused rather than stored
//!   ([`foreign_account_arn_refusal`]).
//! * WHAT KIND of resource it names. An operation that takes a data source takes the kinds
//!   it can actually read from, so an ARN naming a resource of any other type names no data
//!   source at all ([`unsupported_data_source_refusal`]).
//!
//! Whether the resource the ARN names EXISTS is a different question, answered where the
//! resource is held ([`super::ComAmazonawsOpensearchAmazonopensearchserviceState::data_source_activation`])
//! rather than here: an attachment carries that outcome in its status, as the operation's
//! own documentation says it does. These two rules are about the VALUE, which is why they
//! are stated as refusals of the request.
//!
//! Nothing here reaches the clock or the store: each rule is a pure function of the value a
//! request carried, so the same request decides the same way twice.

use ::service_common::arn;

use crate::ops::types::ValidationException;

/// The resource types of THIS SERVICE an application can attach as a data source: a
/// DOMAIN, whose indexes hold the data, and a DIRECT QUERY data source, which reaches data
/// held elsewhere. An OpenSearch Serverless collection is a data source too, and is
/// recognised by the reading that resolves it in the store holding it
/// ([`super::serverless_collections`]).
///
/// `AttachDataSource` documents "the data source must be an Amazon OpenSearch Service
/// domain", and the account's own data sources of this service are the two resources the
/// attachment resolution knows about
/// ([`super::ComAmazonawsOpensearchAmazonopensearchserviceState::data_source_activation`]),
/// so the kinds this rule admits and the kinds that resolution can resolve are one set.
const DATA_SOURCE_RESOURCE_TYPES: [&str; 2] = ["domain", "datasource"];

/// Whether `arn` names a kind of resource this service attaches to an application at all.
///
/// The kinds are this service's own two ([`DATA_SOURCE_RESOURCE_TYPES`]) and an AWS Glue
/// Data Catalog, which is named with no resource id at all — there is exactly one per
/// account and Region — so the same set a pending attachment is resolved against
/// ([`super::is_account_glue_data_catalog_arn`]) is what this admits, read for the KIND
/// alone: WHOSE catalog it is is [`foreign_account_arn_refusal`]'s question. A value that
/// is no ARN names nothing either, which the operation's own malformed-ARN refusal states
/// first.
#[must_use]
pub fn names_a_data_source(arn: &str) -> bool {
    // One kind is a resource this service does NOT hold and reads where it is held
    // ([`super::serverless_collections`]): an OpenSearch Serverless COLLECTION, which an
    // application reads exactly the way it reads a domain. The kind is recognised by the one
    // reading that resolution makes of its ARN.
    if super::serverless_collections::names_a_collection(arn) {
        return true;
    }
    arn::parse(arn).is_some_and(|parsed| {
        let of_this_service = parsed.vendor == super::ARN_NAMESPACE
            && DATA_SOURCE_RESOURCE_TYPES.contains(&parsed.resource_type());
        // A catalog ARN naming no Region names no catalog: an account holds one catalog
        // per Region, so the Region is part of what addresses it.
        let data_catalog = parsed.vendor == super::GLUE_ARN_NAMESPACE
            && parsed.resource == super::GLUE_DATA_CATALOG_RESOURCE
            && !parsed.region.is_empty();
        of_this_service || data_catalog
    })
}

/// The refusal for an ARN that names no kind of resource this service can attach as a data
/// source, or `None` for one that does.
///
/// The sentence names the member and the value the request carried, because WHICH input
/// could not be read as a data source is what a caller has to fix — the same shape as this
/// service's other refusals of an ARN-valued member ([`super::malformed_arn`]).
pub fn unsupported_data_source_refusal(member: &str, arn: &str) -> Option<ValidationException> {
    if names_a_data_source(arn) {
        return None;
    }
    Some(ValidationException {
        message: Some(format!(
            "{member} {arn} does not name a supported data source."
        )),
    })
}

/// The refusal for an ARN naming a resource of ANOTHER Amazon Web Services account, or
/// `None` for one the caller's own account holds.
///
/// Every resource ARN carries the account that holds the resource, and this service serves
/// the account its requests are made for ([`::engine::emulator_account_id`]): a value
/// carrying another account id names a resource no request of this account can reach, so
/// the call is refused as the malformed input it is rather than performed against a resource
/// in someone else's account. An ARN carrying NO account field names no account to compare —
/// a partition-global resource — so there is nothing to refuse, and neither does a value
/// that is no ARN at all, which its operation's own malformed-ARN refusal states.
///
/// The sentence names the member, the account the VALUE carries and the account the request
/// was made for, and nothing else: the two account ids are what a caller compares to see
/// which end is wrong, and both are read off the request and the caller's own identity rather
/// than written here. The ARN is not quoted whole — the member says which value it is, and
/// the account segment is the part of it this refusal is about.
pub fn foreign_account_arn_refusal(member: &str, arn: &str) -> Option<ValidationException> {
    let account = arn::parse(arn)?.account;
    let caller = ::engine::emulator_account_id();
    if account.is_empty() || account == caller {
        return None;
    }
    Some(ValidationException {
        message: Some(format!(
            "{member} account {account} does not match caller account {caller}"
        )),
    })
}
