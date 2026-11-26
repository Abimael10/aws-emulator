// SPDX-License-Identifier: CC0-1.0

//! The ONE envelope this service's brand-profile identifier-format gate answers with,
//! read across the whole surface that accepts the identifier member.
//!
//! A child suite of `brand_profile_refusal_bodies`, which owns the wire harness and the
//! identifier tables; this file owns the reading of the gate's envelope over every
//! operation that reaches it. Declared by its parent, because this crate's one test
//! target declares the suites directly under `tests/all/`.
//!
//! The rule under test, over a SET of inputs rather than one recorded request: a value
//! that satisfies the identifier member's modelled constraints but is no identifier of a
//! brand profile — neither the bare id nor the ARN of one — is refused, before any
//! lookup, with `400` `ValidationException` whose `message` is this gate's one sentence
//! over the value the request gave, and whose `fieldList` is EXACTLY ONE entry naming the
//! member by its BARE model name (`brandProfileId`, not a JSON pointer) and stating that
//! same sentence again. One gate, one envelope: it is the identifier member's, not one
//! operation's, so every operation that accepts the member answers it identically —
//! whether the member arrives as a path label or as a query parameter, and whatever form
//! the refused value took.
//!
//! The rows differ in the operation refusing (every implemented one accepting the
//! member), in the HTTP method, in where the member sits in the request, and in the way
//! the value misses the documented form. Past the gate the classification still holds: an
//! identifier of a documented form that reaches no stored profile is a missing RESOURCE
//! for the operations addressed at one, and an empty page for the one that takes the
//! member as a filter.

use serde_json::{json, Value};

use super::{invalid_format_message, malformed_identifiers, unknown_identifiers, Emulator};

/// The member a refused brand-profile identifier is reported against, as the model names
/// it: `brandProfileId` on every operation that accepts a `BrandProfileIdOrArn`.
const IDENTIFIER_MEMBER: &str = "brandProfileId";

/// An attribute name the attribute-addressed rows carry. The profile is resolved before
/// the attribute is looked up, so the name only has to satisfy the modelled
/// `BrandProfileAttributeName`.
const ATTRIBUTE_NAME: &str = "CompanyName";

/// A well-formed `RegistrationIdOrArn` the import row carries as its body member: the
/// documented bare form, this service's registration prefix and a body of the minted
/// width. The profile the request addresses is resolved first, so this value's own
/// resolution never comes up.
const REGISTRATION_ID: &str = "reg-00000000000000000";

/// Where the identifier member sits in a request, which decides which values can reach it
/// over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// A single path segment (`{brandProfileId}`): a value carrying a `/` would be two
    /// segments, so it addresses another route and never reaches the member.
    PathSegment,
    /// A greedy path label (`{brandProfileId+}`), which carries a `/` whole.
    GreedyLabel,
    /// A query parameter (`?brandProfileId=`). Every character the identifier member's
    /// modelled pattern admits is legal in a URI query, so the rows write the value as it
    /// is.
    QueryParameter,
}

impl Placement {
    /// Whether a request can carry `identifier` on a member in this position at all.
    fn carries(self, identifier: &str) -> bool {
        self != Placement::PathSegment || !identifier.contains('/')
    }
}

/// One request that carries a `BrandProfileIdOrArn` on `brandProfileId`.
struct Addressed {
    /// The operation, as the model spells it.
    operation: &'static str,
    method: &'static str,
    placement: Placement,
    /// The request target, with the identifier already in the position the operation takes
    /// it in.
    target: String,
    body: Option<Value>,
}

/// Every IMPLEMENTED operation of this service that accepts a `BrandProfileIdOrArn`,
/// addressed with `identifier`.
///
/// The two registration-pushing operations (`CreateRegistrationsFromBrandProfile`,
/// `UpdateRegistrationsFromBrandProfile`) are left out: they are still scaffolded and
/// answer `501`, so no request reaches a gate of this service through them, and they are
/// their own issues.
fn addressed_with(identifier: &str) -> Vec<Addressed> {
    let profile = format!("/v1/brand-profiles/{identifier}");
    vec![
        Addressed {
            operation: "GetBrandProfile",
            method: "GET",
            placement: Placement::GreedyLabel,
            target: profile.clone(),
            body: None,
        },
        Addressed {
            operation: "DeleteBrandProfile",
            method: "DELETE",
            placement: Placement::GreedyLabel,
            target: profile.clone(),
            body: None,
        },
        Addressed {
            operation: "UpdateBrandProfile",
            method: "PUT",
            placement: Placement::GreedyLabel,
            target: profile.clone(),
            body: Some(json!({ "brandProfileName": "renamed" })),
        },
        Addressed {
            operation: "ListBrandProfileAttributes",
            method: "GET",
            placement: Placement::PathSegment,
            target: format!("{profile}/attributes"),
            body: None,
        },
        Addressed {
            operation: "CreateBrandProfileAttributes",
            method: "POST",
            placement: Placement::PathSegment,
            target: format!("{profile}/attributes"),
            body: Some(json!({ "attributes": [{
                "attributeName": ATTRIBUTE_NAME,
                "attributeType": "TEXT",
                "attributeValue": "a value",
            }] })),
        },
        Addressed {
            operation: "GetBrandProfileAttribute",
            method: "GET",
            placement: Placement::PathSegment,
            target: format!("{profile}/attributes/{ATTRIBUTE_NAME}"),
            body: None,
        },
        Addressed {
            operation: "UpdateBrandProfileAttribute",
            method: "PUT",
            placement: Placement::PathSegment,
            target: format!("{profile}/attributes/{ATTRIBUTE_NAME}"),
            body: Some(json!({ "attributeValue": "a value" })),
        },
        Addressed {
            operation: "DeleteBrandProfileAttribute",
            method: "DELETE",
            placement: Placement::PathSegment,
            target: format!("{profile}/attributes/{ATTRIBUTE_NAME}"),
            body: None,
        },
        Addressed {
            operation: "ListRegistrationsFromBrandProfile",
            method: "GET",
            placement: Placement::PathSegment,
            target: format!("{profile}/registrations"),
            body: None,
        },
        Addressed {
            operation: "UpdateBrandProfileFromRegistration",
            method: "POST",
            placement: Placement::PathSegment,
            target: format!("{profile}/update-from-registration"),
            body: Some(json!({ "registrationId": REGISTRATION_ID })),
        },
        Addressed {
            operation: "ListJobs",
            method: "GET",
            placement: Placement::QueryParameter,
            target: format!("/v1/jobs?brandProfileId={identifier}"),
            body: None,
        },
    ]
}

/// How many implemented operations of this service accept the identifier member, so a
/// table that quietly stopped addressing one of them fails rather than passing smaller.
const OPERATIONS_ACCEPTING_THE_MEMBER: usize = 11;

#[tokio::test]
async fn every_operation_accepting_the_identifier_answers_the_one_gate_envelope() {
    let em = Emulator::new();
    // A live profile, so no refusal below can be mistaken for an empty store: the gate is
    // about the VALUE the request gave, not about what the store holds. Its name is one of
    // the refused rows, since a name satisfies the member's pattern without being an
    // identifier.
    let created = em
        .create(json!({ "brandProfileName": "a-profile-name" }))
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );

    let mut checked = 0_usize;
    for identifier in malformed_identifiers() {
        let stated = invalid_format_message(&identifier);
        let addressed = addressed_with(&identifier);
        assert_eq!(
            addressed.len(),
            OPERATIONS_ACCEPTING_THE_MEMBER,
            "case identifier={identifier:?}: the table must address every operation that \
             accepts the member, addressed {}",
            addressed.len()
        );
        for row in addressed {
            if !row.placement.carries(&identifier) {
                continue;
            }
            let case = format!(
                "{} {} {:?} identifier={identifier:?}",
                row.operation, row.method, row.placement
            );
            let reply = em.call(row.method, &row.target, row.body).await;
            checked += 1;

            assert_eq!(
                reply.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
            assert_eq!(
                reply.string("message"),
                stated,
                "case {case}: expected message {stated:?}, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.body["fieldList"],
                json!([{ "path": IDENTIFIER_MEMBER, "message": stated }]),
                "case {case}: expected one entry naming {IDENTIFIER_MEMBER:?} and stating \
                 {stated:?}, got {:?}",
                reply.body
            );
            // Spelled out besides the whole-list comparison, because a pointer under that
            // same key is exactly what a client keying off the entry's path cannot look
            // the member up by.
            let path = reply.body["fieldList"][0]["path"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: no entry path in {:?}", reply.body));
            assert!(
                !path.starts_with('/'),
                "case {case}: the entry must name the member rather than point at it, got \
                 {path:?}"
            );
        }
    }

    assert!(
        checked >= OPERATIONS_ACCEPTING_THE_MEMBER * 2,
        "expected the gate to be read over every operation accepting the member and over \
         several forms of a value that is none, read {checked} case(s)"
    );
}

#[tokio::test]
async fn a_documented_identifier_that_reaches_no_profile_is_classified_past_the_gate() {
    // The gate classifies the VALUE before any lookup, so it refuses only what can name no
    // profile in any state: an identifier of a documented form that this store holds no
    // record for is a missing resource for an operation addressed at a profile, and an
    // empty page for the one that takes the member as a filter. Without this, a gate that
    // refused every identifier it was handed would satisfy the rule above.
    let em = Emulator::new();

    for identifier in unknown_identifiers() {
        for row in addressed_with(&identifier) {
            if !row.placement.carries(&identifier) {
                continue;
            }
            let case = format!(
                "{} {} {:?} identifier={identifier:?}",
                row.operation, row.method, row.placement
            );
            let reply = em.call(row.method, &row.target, row.body).await;

            if row.placement == Placement::QueryParameter {
                assert_eq!(
                    reply.status, 200,
                    "case {case}: a filter matching nothing is an empty page, got {} / {:?}",
                    reply.status, reply.body
                );
                assert_eq!(
                    reply.body["jobs"],
                    json!([]),
                    "case {case}: expected no job to match, got {:?}",
                    reply.body
                );
                continue;
            }
            assert_eq!(
                (reply.status, reply.error.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {case}: expected 404, got {} / {:?}",
                reply.status,
                reply.body
            );
            assert!(
                reply.body["fieldList"].is_null(),
                "case {case}: a missing resource names no member at fault, got {:?}",
                reply.body
            );
        }
    }
}
