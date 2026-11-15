// SPDX-License-Identifier: CC0-1.0

//! How MANY times a refused change is reported in a `ChangeResourceRecordSets`
//! refusal body.
//!
//! The rule: a refusal reports each problem ONCE. `InvalidChangeBatch` models
//! two members that could carry the same problems — the flat `message` and a
//! `messages` LIST — and the service fills only the flat one, with the whole
//! problem list rendered as a single bracketed, comma-separated list; the list
//! member is left off the wire. A client that reads `message`, one that
//! enumerates `messages`, and one that concatenates both all see each refused
//! change exactly once, so the number of problems a client counts is the number
//! of changes the batch could not apply.
//!
//! A second rule shares the table: a DELETE must restate the record set it
//! deletes — "you must specify all the same values that you specified when you
//! created it" — so failing to restate it is ONE failure, reported as one
//! sentence about the values, whichever of the values (record data, TTL, alias
//! target) differs and however many of them do.
//!
//! The table spans what the two rules generalise over: the FIELD of a
//! strict-match DELETE that fails to match, the kind of problem (a DELETE that
//! matches nothing, a duplicate CREATE), the record TYPE, and the NUMBER of
//! refused changes in one batch.
//!
//! The harness and the change builders come from the suites that own them
//! (`hosted_zone_records`, `record_set_validation`), so what is asserted is the
//! HTTP/restXml body a Route 53 client reads.

use crate::hosted_zone_records::{create_zone_ok, router, Reply};
use crate::record_set_validation::{change, list, message, submit, Rrset};

/// The zone every row below works inside.
const ZONE: &str = "example.com";

/// A hosted zone id an `AliasTarget` can name, so an alias row does not depend
/// on the zone the test happens to create: the CloudFront distribution zone id
/// AWS documents for alias targets, as `record_set_validation` names it too.
const CLOUDFRONT_ZONE: &str = "Z2FDTNDATAQYW2";

/// The ONE canonical form of a name, which is the form a refusal names a record
/// set in: fully qualified by the root dot and folded to lower case.
fn canonical(name: &str) -> String {
    format!("{}.", name.trim_end_matches('.').to_lowercase())
}

/// How a refusal names the record set a change was about.
fn label(name: &str, rr_type: &str) -> String {
    format!("[name='{}', type='{rr_type}']", canonical(name))
}

/// The problem a DELETE that does not name the record set it would delete is
/// reported as, for the given mismatch clause.
fn delete_problem(name: &str, rr_type: &str, clause: &str) -> String {
    format!(
        "Tried to delete resource record set {} but {clause}",
        label(name, rr_type)
    )
}

/// The problem a CREATE over a record set the zone already holds is reported as.
fn create_problem(name: &str, rr_type: &str) -> String {
    format!(
        "Tried to create resource record set {} but it already exists",
        label(name, rr_type)
    )
}

/// One refused batch: what the zone holds first, the changes it refuses, and the
/// problems the refusal must report.
struct Row {
    label: &'static str,
    /// The record sets created before the refused batch is submitted.
    seed: Vec<Rrset>,
    /// The refused batch, as `(action, record set)` pairs.
    refused: Vec<(&'static str, Rrset)>,
    /// The problems, in the order the changes were submitted in.
    problems: Vec<String>,
}

/// The rows: the mismatching field, the problem kind, the record type and the
/// number of refused changes in one batch.
fn rows() -> Vec<Row> {
    let www = Rrset::new(&format!("www.{ZONE}"), "A")
        .ttl(300)
        .values(&["203.0.113.10"]);
    let text = Rrset::new(&format!("text.{ZONE}"), "TXT")
        .ttl(60)
        .values(&["\"one\"", "\"two\""]);
    let mail = Rrset::new(&format!("mail.{ZONE}"), "MX")
        .ttl(600)
        .values(&["10 mx.example.net."]);
    let aliased = Rrset::new(&format!("cdn.{ZONE}"), "A")
        .alias(CLOUDFRONT_ZONE, "d111111abcdef8.cloudfront.net");

    // The one clause a strict-match DELETE is refused with, whichever of the
    // values the request failed to restate.
    const VALUES: &str = "the values provided do not match the current values";

    vec![
        Row {
            label: "a DELETE whose single value does not match",
            seed: vec![www.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("www.{ZONE}"), "A")
                    .ttl(300)
                    .values(&["203.0.113.99"]),
            )],
            problems: vec![delete_problem(&format!("www.{ZONE}"), "A", VALUES)],
        },
        Row {
            label: "a DELETE whose TTL alone does not match",
            seed: vec![www.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("www.{ZONE}"), "A")
                    .ttl(60)
                    .values(&["203.0.113.10"]),
            )],
            problems: vec![delete_problem(&format!("www.{ZONE}"), "A", VALUES)],
        },
        Row {
            label: "a DELETE of a multi-valued TXT record set missing one value",
            seed: vec![text.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("text.{ZONE}"), "TXT")
                    .ttl(60)
                    .values(&["\"one\""]),
            )],
            problems: vec![delete_problem(&format!("text.{ZONE}"), "TXT", VALUES)],
        },
        Row {
            label: "a DELETE of an MX record set whose preference differs",
            seed: vec![mail.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("mail.{ZONE}"), "MX")
                    .ttl(600)
                    .values(&["20 mx.example.net."]),
            )],
            problems: vec![delete_problem(&format!("mail.{ZONE}"), "MX", VALUES)],
        },
        Row {
            label: "a DELETE whose alias target does not match",
            seed: vec![aliased.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("cdn.{ZONE}"), "A")
                    .alias(CLOUDFRONT_ZONE, "d222222abcdef8.cloudfront.net"),
            )],
            problems: vec![delete_problem(&format!("cdn.{ZONE}"), "A", VALUES)],
        },
        Row {
            label: "a DELETE whose value and TTL both differ",
            seed: vec![www.clone()],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("www.{ZONE}"), "A")
                    .ttl(60)
                    .values(&["203.0.113.99"]),
            )],
            problems: vec![delete_problem(&format!("www.{ZONE}"), "A", VALUES)],
        },
        Row {
            label: "a DELETE of a record set the zone does not hold",
            seed: vec![],
            refused: vec![(
                "DELETE",
                Rrset::new(&format!("absent.{ZONE}"), "AAAA")
                    .ttl(300)
                    .values(&["2001:db8::1"]),
            )],
            problems: vec![delete_problem(
                &format!("absent.{ZONE}"),
                "AAAA",
                "it was not found",
            )],
        },
        Row {
            label: "a CREATE over a record set the zone already holds",
            seed: vec![www.clone()],
            refused: vec![("CREATE", www.clone())],
            problems: vec![create_problem(&format!("www.{ZONE}"), "A")],
        },
        Row {
            label: "two mismatching DELETEs in one batch",
            seed: vec![www.clone(), mail.clone()],
            refused: vec![
                (
                    "DELETE",
                    Rrset::new(&format!("www.{ZONE}"), "A")
                        .ttl(300)
                        .values(&["203.0.113.99"]),
                ),
                (
                    "DELETE",
                    Rrset::new(&format!("mail.{ZONE}"), "MX")
                        .ttl(900)
                        .values(&["10 mx.example.net."]),
                ),
            ],
            problems: vec![
                delete_problem(&format!("www.{ZONE}"), "A", VALUES),
                delete_problem(&format!("mail.{ZONE}"), "MX", VALUES),
            ],
        },
        Row {
            label: "three refused changes of three different kinds",
            seed: vec![www.clone(), text.clone()],
            refused: vec![
                (
                    "DELETE",
                    Rrset::new(&format!("www.{ZONE}"), "A")
                        .ttl(300)
                        .values(&["203.0.113.99"]),
                ),
                ("CREATE", text.clone()),
                (
                    "DELETE",
                    Rrset::new(&format!("absent.{ZONE}"), "AAAA")
                        .ttl(300)
                        .values(&["2001:db8::1"]),
                ),
            ],
            problems: vec![
                delete_problem(&format!("www.{ZONE}"), "A", VALUES),
                create_problem(&format!("text.{ZONE}"), "TXT"),
                delete_problem(&format!("absent.{ZONE}"), "AAAA", "it was not found"),
            ],
        },
    ]
}

/// How many problem sentences the whole refusal document carries, however they
/// are escaped: every problem this suite drives opens with the same two words.
fn problem_sentences(reply: &Reply) -> usize {
    reply.body.matches("Tried to ").count()
}

#[tokio::test]
async fn a_change_batch_refusal_reports_each_problem_once() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.label;
        let router = router();
        let zone_id = create_zone_ok(&router, ZONE, &format!("ref-once-{index}")).await;

        for record in &row.seed {
            let seeded = submit(&router, &zone_id, &change("CREATE", record)).await;
            assert_eq!(
                seeded.status, 200,
                "case {case:?}: the seed record set must be created, got {} with body {}",
                seeded.status, seeded.body
            );
        }

        let batch: String = row
            .refused
            .iter()
            .map(|(action, record)| change(action, record))
            .collect();
        let reply = submit(&router, &zone_id, &batch).await;

        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidChangeBatch",
            "case {case:?}: expected InvalidChangeBatch, got {}",
            reply.body
        );

        // The whole problem list, in one pair of brackets, in the flat member.
        let expected = format!("[{}]", row.problems.join(", "));
        let actual = message(&reply);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the refusal {expected:?}, got {actual:?}"
        );

        // The modelled list member is not on the wire beside it.
        let listed = reply.all("messages/member");
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no `messages` entries beside the flat message, got \
             {listed:?} in {}",
            reply.body
        );

        // So the document states each problem exactly once: a client reading
        // both members counts one problem per refused change, not two.
        let sentences = problem_sentences(&reply);
        assert_eq!(
            sentences,
            row.problems.len(),
            "case {case:?}: expected {} problem sentence(s) in the whole body, got {sentences} \
             in {}",
            row.problems.len(),
            reply.body
        );

        // The refused batch landed nothing: every seeded record set is still
        // there, so what was asserted above is a refusal and not a partial
        // apply.
        let after = list(&router, &zone_id).await;
        let names = after.all("ResourceRecordSet/Name");
        for record in &row.seed {
            let canonical = canonical(&record.canonical());
            assert!(
                names.contains(&canonical.as_str()),
                "case {case:?}: {canonical} must still be listed after the refusal, got {names:?}"
            );
        }
    }
}
