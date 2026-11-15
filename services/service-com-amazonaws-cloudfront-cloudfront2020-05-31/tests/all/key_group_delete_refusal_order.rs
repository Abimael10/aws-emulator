// SPDX-License-Identifier: CC0-1.0

//! The ORDER `DeleteKeyGroup` reports its refusals in, when more than one of them is live at
//! once.
//!
//! The sibling suite [`crate::key_group_delete`] pins each refusal on its own. The rule here
//! is the one that decides what a caller is told when two of them hold for the same request:
//! the delete resolves the identifier first, then evaluates the version precondition, then
//! the state conflict — so an identifier the store holds nothing for is the absent-resource
//! refusal whatever version token the request carries and whatever a stored cache behaviour
//! trusts, and a version token that is not the group's current one is reported before what
//! the group is referenced by.
//!
//! That is HTTP's own order (the resource, then its precondition, then its state), and it is
//! what keeps a refusal from describing a resource the caller cannot see: a 412 for an id
//! nothing is stored under would confirm the id exists, and a 409 under a stale token would
//! report a reference the caller has no current view of.
//!
//! The rows are the product of three dimensions — WHICH identifier the delete names (the
//! stored group, an id never minted, a value that is no identifier of this surface), WHICH
//! `If-Match` it carries (the current token bare and quoted, no header at all, a value that
//! is no version token, another group's well-formed token), and WHETHER a stored cache
//! behaviour trusts the group — and every row's expected answer is computed from the rule
//! above rather than written out per row, so a row cannot quietly encode a different order.
//!
//! The harness is [`crate::distribution_lifecycle`]'s, the key group writer and reader
//! [`crate::key_group_lifecycle`]'s and the trusting distribution's document
//! [`crate::distributions_by_key_group`]'s — reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, create_distribution, emulator, Answer, API_VERSION};
use crate::distributions_by_key_group::{config_xml, Row};
use crate::key_group_lifecycle::{create_group, get_group, mint_public_keys, Group};

/// The sentences CloudFront sends for these refusals, spelled out here rather than taken from
/// the crate, so a reworded refusal fails this suite.
const NO_SUCH_KEY_GROUP_MESSAGE: &str = "The specified key group does not exist.";
const INVALID_IF_MATCH_VERSION_MESSAGE: &str =
    "The If-Match version is missing or not valid for the resource.";
const PRECONDITION_FAILED_MESSAGE: &str =
    "The request failed because it didn't meet the preconditions for this operation.";
const KEY_GROUP_IN_USE_MESSAGE: &str =
    "The specified key group is currently associated with a cache behavior. Please \
     disassociate the key group before deleting.";

/// A key group id in the shape this surface mints that nothing in the store holds.
const NEVER_MINTED_ID: &str = "2f3a9d54-6c71-4f0b-8d2e-5a7c1b9e4f68";
/// A path value that is no identifier of this surface at all.
const NOT_AN_ID: &str = "not-a-key-group-id";

/// Which identifier the delete under test names.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// The group the row stored, which the other dimensions are varied around.
    Stored,
    /// An id of the minted shape the store holds no group for.
    NeverMinted,
    /// A value that is no identifier of this surface.
    NotAnId,
}

/// Which `If-Match` the delete under test carries.
#[derive(Debug, Clone, Copy)]
enum Header {
    /// The group's current token, as CloudFront's own `ETag` header reports it.
    CurrentBare,
    /// The group's current token, spelled as HTTP spells an entity tag.
    CurrentQuoted,
    /// No `If-Match` header at all.
    Absent,
    /// A value that is no version token.
    NotAToken,
    /// Another group's token: well formed, but not this group's current one.
    AnotherGroups,
}

/// The one distribution a referencing row stores: its default cache behaviour trusts the key
/// group at index 0, which is the group under the delete.
const TRUSTING: Row = Row {
    case: "the default cache behaviour trusts the group under the delete",
    default_groups: &[0],
    ordered: &[],
    enabled: true,
};

/// What the answer must be, computed from the rule: the identifier is resolved first, then the
/// version precondition, then the state conflict. `(status, code, message)`, with the success
/// answer as `(204, "", "")`.
fn expected(target: Target, header: Header, referenced: bool) -> (u16, &'static str, &'static str) {
    match target {
        Target::NeverMinted | Target::NotAnId => {
            (404, "NoSuchResource", NO_SUCH_KEY_GROUP_MESSAGE)
        }
        Target::Stored => match header {
            Header::Absent | Header::NotAToken => (
                400,
                "InvalidIfMatchVersion",
                INVALID_IF_MATCH_VERSION_MESSAGE,
            ),
            Header::AnotherGroups => (412, "PreconditionFailed", PRECONDITION_FAILED_MESSAGE),
            Header::CurrentBare | Header::CurrentQuoted if referenced => {
                (409, "ResourceInUse", KEY_GROUP_IN_USE_MESSAGE)
            }
            Header::CurrentBare | Header::CurrentQuoted => (204, "", ""),
        },
    }
}

/// `DELETE /2020-05-31/key-group/{Id}` under the version token `if_match`.
async fn delete_group(router: &MultiServiceRouter, id: &str, if_match: Option<&str>) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/key-group/{id}"),
        None,
        if_match,
    )
    .await
}

/// Store one key group naming `key`, under `name`, and report its id and current token.
async fn store_group(router: &MultiServiceRouter, name: &str, key: &str) -> (String, String) {
    let group = Group {
        case: format!("the key group {name}"),
        name: name.to_string(),
        items: vec![key.to_string()],
        comment: None,
    };
    let answer = create_group(router, &group.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {name}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer.root().text_at("Id").unwrap_or_default().to_string();
    let etag = answer.header("etag").to_string();
    assert!(
        !id.is_empty() && !etag.is_empty(),
        "case {name}: expected a minted id and version token, got {id:?} and {etag:?} in {}",
        answer.raw
    );
    (id, etag)
}

/// Every combination of identifier, `If-Match` and reference, each answered in the order the
/// rule states and each leaving the store exactly as a refusal found it.
#[tokio::test]
async fn two_live_refusals_are_reported_in_the_rule_s_order() {
    let targets = [Target::Stored, Target::NeverMinted, Target::NotAnId];
    let headers = [
        Header::CurrentBare,
        Header::CurrentQuoted,
        Header::Absent,
        Header::NotAToken,
        Header::AnotherGroups,
    ];

    for target in targets {
        for header in headers {
            for referenced in [false, true] {
                let case =
                    format!("{target:?} under {header:?} with referenced={referenced}");
                let router = emulator();
                let keys = mint_public_keys(&router, "key-group-delete-order", 1).await;

                let (subject_id, subject_etag) =
                    store_group(&router, "key-group-delete-order-subject", &keys[0]).await;
                // A second group only so that its token is a well-formed version token that
                // is NOT the subject's: the stale-token row needs one the store really minted.
                let (_, other_etag) =
                    store_group(&router, "key-group-delete-order-other", &keys[0]).await;

                if referenced {
                    let created = create_distribution(
                        &router,
                        &config_xml(
                            "key-group-delete-order",
                            &TRUSTING,
                            std::slice::from_ref(&subject_id),
                        ),
                    )
                    .await;
                    assert_eq!(
                        created.status, 201,
                        "case {case:?}: expected HTTP 201 from CreateDistribution, got {} with {}",
                        created.status, created.raw
                    );
                }

                let id = match target {
                    Target::Stored => subject_id.clone(),
                    Target::NeverMinted => NEVER_MINTED_ID.to_string(),
                    Target::NotAnId => NOT_AN_ID.to_string(),
                };
                let quoted = format!("\"{subject_etag}\"");
                let if_match: Option<&str> = match header {
                    Header::CurrentBare => Some(subject_etag.as_str()),
                    Header::CurrentQuoted => Some(quoted.as_str()),
                    Header::Absent => None,
                    Header::NotAToken => Some("not-a-version"),
                    Header::AnotherGroups => Some(other_etag.as_str()),
                };

                let (status, code, message) = expected(target, header, referenced);
                let answer = delete_group(&router, &id, if_match).await;
                assert_eq!(
                    answer.status, status,
                    "case {case:?}: expected HTTP {status}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    code,
                    "case {case:?}: expected the wire code {code:?}, got {}",
                    answer.raw
                );
                let served = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    served, message,
                    "case {case:?}: expected the sentence {message:?}, got {served:?}"
                );

                // A refusal removes nothing: the subject is still readable under the token it
                // was created with. The one row that succeeds removed it, so its reader is the
                // absent-group refusal instead.
                let read = get_group(&router, &subject_id).await;
                if status == 204 {
                    assert_eq!(
                        read.status, 404,
                        "case {case:?}: expected HTTP 404 from GetKeyGroup after the delete, \
                         got {} with {}",
                        read.status, read.raw
                    );
                    assert_eq!(
                        read.error_code(),
                        "NoSuchResource",
                        "case {case:?}: expected NoSuchResource after the delete, got {}",
                        read.raw
                    );
                } else {
                    assert_eq!(
                        read.status, 200,
                        "case {case:?}: expected the refused delete to leave the group \
                         readable, got {} with {}",
                        read.status, read.raw
                    );
                    assert_eq!(
                        read.header("etag"),
                        subject_etag,
                        "case {case:?}: expected the refused delete to leave the version token \
                         alone, got {:?}",
                        read.header("etag")
                    );
                }
            }
        }
    }
}
