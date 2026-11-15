// SPDX-License-Identifier: CC0-1.0

//! How Control Catalog reports the invalid-filter refusal it SURFACES from the component
//! that evaluates a filtered listing's filter, driven over HTTP as a client drives it.
//!
//! THE RULE UNDER TEST: a refusal this service answers because the component it delegated
//! the work to raised an exception reports that component's reason, then the envelope stated
//! around the surfaced call — `(Service: <the component that raised it>, Status Code: <the
//! status that call was answered with>, Request ID: <that call's own id>) (SDK Attempt Count:
//! 1)`. So the envelope's fixed labels are always there, the component and the status a
//! client parses back out of the text are the ones that decided the refusal, and the request
//! id belongs to the refusal rather than being fixed text. A refusal the catalog front door
//! decides for itself — an ARN naming no published control, a cursor it never issued — names
//! no component and carries no envelope at all.
//!
//! What the rule generalises over is the whole invalid-filter class: both operations that
//! take a single-ARN resource filter and therefore both filter members, the number of
//! entries, whether the entries name published resources, whether an entry names a resource
//! at all, and whether the request also asks for a page size — none of which the message may
//! vary with except in the request id.
//!
//! The harness (`Emulator`, `error_code`) is `control_catalog.rs`'s, and the message is read
//! through `validation_messages::message_of`, so this suite drives the same front door and
//! reads a refusal the same way as its siblings.

use std::collections::BTreeSet;

use service_com_amazonaws_controlcatalog_controlcatalog::state::ComAmazonawsControlcatalogControlcatalogState as Catalog;

use crate::control_catalog::{error_code, Emulator};
use crate::validation_messages::message_of;

/// The sentence the invalid-filter class carries, whatever the filter held.
const INVALID_FILTER: &str =
    "The provided filter is not valid. Ensure that each part of the filter is valid, and try again.";

/// The component the filtered listings' filter evaluation belongs to, as the envelope of a
/// refusal it raised names it.
const ONTOLOGY_COMPONENT: &str = "ControlCatalogOntology";

/// The literal segment that closes a surfaced refusal: the attempts the surfaced call took.
/// A refusal of the caller's own filter is not retried, so there was exactly one.
///
/// This is the form of the workspace's one renderer of a surfaced refusal, which every
/// service that reports one shares, rather than a claim of its own about the wire: what the
/// rule requires is the sentence, the labels and the per-refusal values asserted below.
const ATTEMPT_SUFFIX: &str = ") (SDK Attempt Count: 1)";

/// A filter entry naming `arn`. Both entry shapes carry one optional `Arn` member
/// (`smithy_query(query="shape",shape="DomainResourceFilter,ObjectiveResourceFilter")`).
fn entry(arn: &str) -> String {
    format!(r#"{{"Arn":"{arn}"}}"#)
}

/// An entry naming no resource: `Arn` is optional on both entry shapes, so such a body
/// passes the boundary constraint walk and reaches the handler.
fn nameless_entry() -> String {
    "{}".to_string()
}

/// One of the two operations that take a single-ARN resource filter: its route, the input
/// member holding the filter, that filter's list member, and the published ARNs its entries
/// can name.
struct FilteredListing {
    /// Route of the operation (`smithy_query(query="operation",
    /// operation="ListObjectives,ListCommonControls")`: POST `/objectives`, POST
    /// `/common-controls`).
    route: &'static str,
    /// The input member holding the filter structure.
    filter_member: &'static str,
    /// The filter structure's list member, holding the resource entries.
    list_member: &'static str,
    /// The ARN kind segment an entry's `Arn` names.
    kind: &'static str,
    /// Published ARNs of the kind that member's entries name, out of the catalog served.
    published: Vec<String>,
}

impl FilteredListing {
    /// A request body whose filter list holds `entries` (each already JSON).
    fn body(&self, entries: &[String]) -> String {
        format!(
            r#"{{"{}":{{"{}":[{}]}}}}"#,
            self.filter_member,
            self.list_member,
            entries.join(",")
        )
    }

    /// An ARN of this filter's kind carrying `identifier`.
    fn arn(&self, identifier: &str) -> String {
        format!("arn:aws:controlcatalog:::{}/{identifier}", self.kind)
    }
}

/// Both filtered listings, with the published ARNs each one's filter entries can name.
fn filtered_listings() -> Vec<FilteredListing> {
    let catalog = Catalog::default();
    let domains: Vec<String> = catalog.domains.keys().take(4).cloned().collect();
    let objectives: Vec<String> = catalog.objectives.keys().take(4).cloned().collect();
    assert!(
        domains.len() >= 3 && objectives.len() >= 3,
        "case catalog: the catalog must publish several domains and objectives for this \
         table, got {} domains and {} objectives",
        domains.len(),
        objectives.len()
    );
    vec![
        FilteredListing {
            route: "/objectives",
            filter_member: "ObjectiveFilter",
            list_member: "Domains",
            kind: "domain",
            published: domains,
        },
        FilteredListing {
            route: "/common-controls",
            filter_member: "CommonControlFilter",
            list_member: "Objectives",
            kind: "objective",
            published: objectives,
        },
    ]
}

/// The request id a surfaced refusal states, taken out of `message` by the literal segments
/// that bracket it, or `None` when `message` is not in the surfaced form for `status`.
fn surfaced_request_id(message: &str, status: u16) -> Option<String> {
    let prefix = format!(
        "{INVALID_FILTER} (Service: {ONTOLOGY_COMPONENT}, Status Code: {status}, Request ID: "
    );
    Some(
        message
            .strip_prefix(prefix.as_str())?
            .strip_suffix(ATTEMPT_SUFFIX)?
            .to_string(),
    )
}

/// Every filter outside the one-entry rule is refused with the class's sentence followed by
/// the envelope of the component that evaluated it, on both operations and whatever the
/// filter held.
#[tokio::test]
async fn an_unacceptable_filter_is_refused_with_the_surfaced_component_envelope() {
    let emulator = Emulator::new();
    let mut request_ids: Vec<String> = Vec::new();

    for listing in filtered_listings() {
        let first = entry(&listing.published[0]);
        let second = entry(&listing.published[1]);
        // An identifier of the length the catalog's own ARNs carry that nothing is
        // published under, so the row is refused for the filter it is rather than for what
        // it names.
        let unpublished = entry(&listing.arn(&"z".repeat(25)));
        let nameless = nameless_entry();

        // (row, query, entries). The rows vary the entry count, whether the entries name
        // published resources, whether an entry names a resource at all, and whether the
        // caller also asks for a page size — the page size is a query parameter
        // (`@httpQuery maxResults`), so it is spelled on the URI.
        let rows: Vec<(String, &str, Vec<String>)> = vec![
            (
                "two published resources".to_string(),
                "",
                vec![first.clone(), second.clone()],
            ),
            (
                "two published resources and a page size".to_string(),
                "?maxResults=10",
                vec![first.clone(), second.clone()],
            ),
            (
                "three published resources".to_string(),
                "",
                vec![first.clone(), second.clone(), entry(&listing.published[2])],
            ),
            (
                "every published resource this table takes".to_string(),
                "",
                listing.published.iter().map(|arn| entry(arn)).collect(),
            ),
            (
                "the same resource twice".to_string(),
                "",
                vec![first.clone(), first.clone()],
            ),
            (
                "a published and an unpublished resource".to_string(),
                "",
                vec![first.clone(), unpublished.clone()],
            ),
            (
                "one entry naming no resource".to_string(),
                "?maxResults=1",
                vec![nameless.clone()],
            ),
            (
                "two entries, neither naming a resource".to_string(),
                "",
                vec![nameless.clone(), nameless.clone()],
            ),
        ];

        for (row, query, entries) in rows {
            let case = format!("{}{query} with {row}", listing.route);
            let (status, response) = emulator
                .post(
                    &format!("{}{query}", listing.route),
                    &listing.body(&entries),
                )
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {response:?}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {response:?}"
            );
            let message = message_of(&response).unwrap_or_else(|| {
                panic!("case {case:?}: the refusal carries no message: {response:?}")
            });
            // The status the envelope states is the one the refusal was answered with,
            // read off this response rather than written down.
            let id = surfaced_request_id(&message, status);
            assert!(
                id.is_some(),
                "case {case:?}: expected {INVALID_FILTER:?} followed by \"(Service: \
                 {ONTOLOGY_COMPONENT}, Status Code: {status}, Request ID: \
                 <id>){ATTEMPT_SUFFIX}\", got {message:?}"
            );
            let id = id.unwrap_or_default();
            assert!(
                ::service_common::query_error_envelope::is_request_id(&id),
                "case {case:?}: the surfaced call's request id must have the form AWS reports \
                 one in, got {id:?} (message {message:?})"
            );
            // Nothing of the request is rendered into the refusal: not an ARN it carried,
            // not the filter member at fault, and not the number of entries sent.
            for echoed in [
                listing.published[0].as_str(),
                listing.filter_member,
                listing.list_member,
            ] {
                assert!(
                    !message.contains(echoed),
                    "case {case:?}: the refusal echoes {echoed:?}, got {message:?}"
                );
            }
            request_ids.push(id);
        }
    }

    // Per REFUSAL, not per filter: no two of the refusals above stated one request id.
    let distinct: BTreeSet<&String> = request_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        request_ids.len(),
        "each surfaced refusal states the request id of its own call, got {request_ids:?}"
    );
}

/// The other half of the rule: a refusal the catalog front door decides itself names no
/// component and carries no envelope, and two refusals of the SAME filter differ in nothing
/// but the request id.
#[tokio::test]
async fn only_a_surfaced_refusal_carries_the_envelope_and_only_its_id_varies() {
    let emulator = Emulator::new();

    // (case, path and query, body). An ARN naming no published control and a cursor this
    // service never issued are both decided by the front door itself, so neither refusal
    // reports a surfaced call.
    let front_door: Vec<(&str, String, &str)> = vec![
        (
            "an ARN naming no published control",
            "/get-control".to_string(),
            r#"{"ControlArn":"arn:aws:controlcatalog:::control/nosuchcontrolidatall0000"}"#,
        ),
        (
            "a cursor /objectives never issued",
            "/objectives?nextToken=zzzz".to_string(),
            "{}",
        ),
        (
            "a cursor /common-controls never issued",
            "/common-controls?nextToken=zzzz".to_string(),
            "{}",
        ),
    ];
    for (case, path_and_query, body) in front_door {
        let (status, response) = emulator.post(&path_and_query, body).await;
        assert!(
            (400..500).contains(&status),
            "case {case:?}: expected a client refusal, got {status} with {response:?}"
        );
        let message = message_of(&response).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal carries no message: {response:?}")
        });
        assert!(
            !message.contains("(Service: "),
            "case {case:?}: a refusal this service decides itself states no surfaced call, \
             got {message:?}"
        );
    }

    // Two refusals of one filter: the same text, a different request id each time.
    let listing = filtered_listings()
        .into_iter()
        .next()
        .expect("the service declares a filtered listing");
    let body = listing.body(&[
        entry(&listing.published[0]),
        entry(&listing.published[1]),
    ]);
    let mut ids: Vec<String> = Vec::new();
    for attempt in 0..2 {
        let (status, response) = emulator.post(listing.route, &body).await;
        let message = message_of(&response).unwrap_or_else(|| {
            panic!("attempt {attempt}: the refusal carries no message: {response:?}")
        });
        let id = surfaced_request_id(&message, status);
        assert!(
            id.is_some(),
            "attempt {attempt}: expected the surfaced form around {INVALID_FILTER:?}, got \
             {message:?}"
        );
        ids.push(id.unwrap_or_default());
    }
    assert_ne!(
        ids.first(),
        ids.last(),
        "two refusals of the same filter are two surfaced calls, so each states its own \
         request id, got {ids:?}"
    );
}
