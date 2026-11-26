// SPDX-License-Identifier: CC0-1.0

//! A tag key in the namespace AWS RESERVES for itself is refused, end to end over HTTP.
//!
//! RULE: a tag a caller attaches may not be in the namespace AWS reserves for the tags it
//! attaches itself — a key opening with `aws:` — so a tagging call naming one is refused
//! `InvalidTagging` (HTTP 400) with the one fixed sentence that states the namespace, and the
//! resource's tag set is left exactly as it was; a key outside that namespace is attached.
//! The rule is about the KEY rather than about the call carrying it, so it holds on every
//! surface that writes a tag set.
//!
//! The rows span what the rule generalises over: how far into the reserved namespace the key
//! reaches (the prefix alone, one segment, several), whether the reserved key travels alone or
//! beside keys that are fine, whether it carries a value at all, and keys that merely LOOK
//! reserved (`aws` with no colon, the namespace in the middle of the key) — which are
//! attached, because the namespace is a prefix. Both tagging surfaces run the whole table:
//! `TagResource`, which adds to a resource that already carries tags, and
//! `CreateDistributionWithTags`, which carries the set at create time.
//!
//! The wire harness, the tag readers and the resource creates are the tagging suites' own
//! ([`crate::distribution_tags`], [`crate::tag_resource`]), reused rather than copied.

use crate::distribution_lifecycle::{config, emulator, list_distributions, Answer};
use crate::distribution_tags::{
    create_distribution_with_tags, list_tags_for_resource, reported_tags, tag, ReportedTags,
    SubmittedTag,
};
use crate::tag_resource::{create_key_value_store, tag_resource};

/// The sentence a tagging call naming a reserved key is refused with, written out as the
/// LITERAL the service answers rather than read back off the crate's own constant, so a
/// reworded refusal fails this suite.
const RESERVED_KEY_MESSAGE: &str = "Tag key cannot start with 'aws:'.";

/// The code the refusal travels under, and the status the model gives it
/// (`smithy_query(query="operation",operation="TagResource")` -> `InvalidTagging`,
/// httpError 400).
const INVALID_TAGGING: &str = "InvalidTagging";

/// One tag set a row submits, and whether the reserved namespace refuses it.
struct Row {
    case: &'static str,
    /// The tags the call names.
    submitted: Vec<SubmittedTag>,
    /// Whether the set names a key in the reserved namespace, which is what decides the
    /// answer.
    reserved: bool,
}

/// Every tag set the rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the reserved prefix alone",
            submitted: vec![tag("aws:", Some("reserved"))],
            reserved: true,
        },
        Row {
            case: "one reserved segment",
            submitted: vec![tag("aws:owner", Some("aws"))],
            reserved: true,
        },
        Row {
            case: "several reserved segments",
            submitted: vec![tag("aws:cloudformation:stack-name", Some("a-stack"))],
            reserved: true,
        },
        Row {
            case: "a reserved key carrying no value at all",
            submitted: vec![tag("aws:createdBy", None)],
            reserved: true,
        },
        Row {
            case: "a reserved key beside keys that are fine",
            submitted: vec![
                tag("owner", Some("edge-team")),
                tag("aws:owner", Some("aws")),
                tag("stage", Some("prod")),
            ],
            reserved: true,
        },
        Row {
            case: "the namespace without its colon",
            submitted: vec![tag("aws", Some("not-reserved"))],
            reserved: false,
        },
        Row {
            case: "a key opening with the namespace's letters",
            submitted: vec![tag("awsome", Some("not-reserved"))],
            reserved: false,
        },
        Row {
            case: "the namespace inside the key rather than at its start",
            submitted: vec![tag("team-aws:owner", Some("not-reserved"))],
            reserved: false,
        },
        Row {
            case: "a key carrying a colon of its own",
            submitted: vec![tag("cost:centre", Some("42"))],
            reserved: false,
        },
    ]
}

/// The tag set `arn` reports, read back through `ListTagsForResource`.
async fn tags_of(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
    case: &str,
) -> ReportedTags {
    let answer = list_tags_for_resource(router, arn).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected ListTagsForResource on {arn} to answer HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    reported_tags(answer.root())
}

/// Assert `answer` is the refusal the reserved namespace earns.
fn assert_refused(answer: &Answer, case: &str, context: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({context}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        INVALID_TAGGING,
        "case {case} ({context}): expected {INVALID_TAGGING}, got {:?} in {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, RESERVED_KEY_MESSAGE,
        "case {case} ({context}): expected the fixed sentence, got {message:?}"
    );
}

/// `TagResource` attaches a key outside the reserved namespace and refuses one inside it,
/// leaving the resource's tag set exactly as it was.
#[tokio::test]
async fn tag_resource_refuses_a_key_in_the_reserved_namespace() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // The resource starts out carrying a tag of its own, so a refused call is visible as
        // the set it did not change.
        let carried = [tag("carried", Some("before"))];
        let created =
            create_key_value_store(&router, &format!("reserved-{index}"), Some(&carried)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from CreateKeyValueStore, got {} with {}",
            created.status, created.raw
        );
        let arn = created
            .root()
            .text_at("ARN")
            .unwrap_or_default()
            .to_string();
        let before = tags_of(&router, &arn, case).await;

        let answer = tag_resource(&router, &arn, Some(&row.submitted)).await;
        if row.reserved {
            assert_refused(&answer, case, "TagResource");
            let after = tags_of(&router, &arn, case).await;
            assert_eq!(
                after, before,
                "case {case} (TagResource): expected the refused call to leave the tag set \
                 {before:?}, got {after:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 204,
            "case {case} (TagResource): expected HTTP 204, got {} with {}",
            answer.status, answer.raw
        );
        let mut expected = before.clone();
        expected.extend(row.submitted.iter().cloned());
        let after = tags_of(&router, &arn, case).await;
        assert_eq!(
            after, expected,
            "case {case} (TagResource): expected the tag set {expected:?}, got {after:?}"
        );
    }
}

/// `CreateDistributionWithTags` carries the same rule: a reserved key is refused and creates
/// nothing, and a key outside the namespace is stored with the distribution.
#[tokio::test]
async fn a_create_with_tags_refuses_a_key_in_the_reserved_namespace() {
    let router = emulator();
    let mut stored = 0usize;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let reference = format!("ref-reserved-tag-{index}");
        let configuration = config(case, &reference);
        let answer =
            create_distribution_with_tags(&router, &configuration, Some(&row.submitted)).await;
        if row.reserved {
            assert_refused(&answer, case, "CreateDistributionWithTags");
            assert_eq!(
                answer.header("location"),
                "",
                "case {case} (CreateDistributionWithTags): expected no Location on a refusal, \
                 got {:?}",
                answer.header("location")
            );
        } else {
            assert_eq!(
                answer.status, 201,
                "case {case} (CreateDistributionWithTags): expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
            stored += 1;
            let arn = answer
                .root()
                .text_at("ARN")
                .unwrap_or_default()
                .to_string();
            let expected: ReportedTags = row.submitted.iter().cloned().collect();
            let after = tags_of(&router, &arn, case).await;
            assert_eq!(
                after, expected,
                "case {case} (CreateDistributionWithTags): expected the stored tag set \
                 {expected:?}, got {after:?}"
            );
        }
    }

    // A refused create claims nothing: the account holds exactly the distributions the
    // admitted rows created.
    let listed = list_distributions(&router, "").await;
    let quantity: usize = listed
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default();
    assert_eq!(
        quantity, stored,
        "expected the account to hold {stored} distributions, got {quantity}"
    );
}
