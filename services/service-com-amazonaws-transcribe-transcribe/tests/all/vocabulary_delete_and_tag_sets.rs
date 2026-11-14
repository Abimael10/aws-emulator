// SPDX-License-Identifier: CC0-1.0

//! Two rules of this service's vocabulary and tagging state, over the wire:
//!
//! 1. A delete in the custom-vocabulary family answers for the record its name
//!    identifies, each family in its own way about PRESENCE: the vocabulary-filter
//!    delete needs the entity to be there, so a name this account holds no filter under
//!    is refused with this service's absent-resource `BadRequestException`, while the
//!    custom-vocabulary delete is idempotent about presence and answers its empty
//!    success for a name holding no record (the class is spanned by
//!    `crate::vocabulary_delete_idempotence`). Either way the call stores nothing. The
//!    other refusal on that path is the documented non-terminal-state one (a `PENDING`
//!    vocabulary), which `crate::vocabularies` pins.
//! 2. A tag set this service REPORTS for a resource it holds is always present:
//!    `ListTagsForResource` answers an empty `Tags` list for a resource that carries
//!    no tags, whatever made it carry none, rather than leaving the member out.
//!
//! Every call goes through the same `MultiServiceRouter` the production router
//! dispatches on, reusing the transcription-job suite's harness
//! (`crate::transcription_jobs`) rather than standing up a second emulator.

use serde_json::{json, Value};

use crate::transcription_jobs::{Reply, Transcribe};

/// Assert `reply` is a SUCCESS, whichever 2xx this protocol puts on the status line.
///
/// Which 2xx an empty awsJson1_1 success carries — the modelled `204` these
/// operations publish today or the `200` the recordings show — is the subject of its
/// own issue (`transcribe-awsjson1-1-success-responses-use-the-inert`). What the rules
/// here are about is success versus refusal, so they are asserted in a way that holds
/// for either.
pub(crate) fn succeeded(reply: &Reply, case: &str) {
    assert!(
        (200..300).contains(&reply.status),
        "case {case:?}: expected a 2xx success, got HTTP {} with body {:?}",
        reply.status,
        reply.body
    );
}

/// Assert `reply` is a client refusal, whichever 4xx shape this service renders it
/// with.
///
/// The status and `__type` of a missing-resource refusal are likewise another issue's
/// subject (`transcribe-client-errors-answer-the-modeled-httperror-instead`); what
/// matters here is that the read after a delete does NOT succeed.
pub(crate) fn refused(reply: &Reply, case: &str) {
    assert!(
        (400..500).contains(&reply.status),
        "case {case:?}: expected a 4xx refusal, got HTTP {} with body {:?}",
        reply.status,
        reply.body
    );
}

/// Assert `reply` is this service's absent-resource refusal for a resource of `kind`:
/// a client error naming `BadRequestException` whose message is the wording that
/// family's not-found refusal carries
/// (`crate::transcription_job_request_rules::not_found_message`), which names the KIND
/// of resource and echoes nothing from the request.
pub(crate) fn absent_resource_refusal(reply: &Reply, case: &str, kind: &str) {
    refused(reply, case);
    assert_eq!(
        reply.error_code(),
        "BadRequestException",
        "case {case:?}: expected the modelled BadRequestException, got {:?}",
        reply.body
    );
    let message = reply.error_message(case);
    let expected = crate::transcription_job_request_rules::not_found_message(kind);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the {kind:?} refusal {expected:?}, got {message:?}"
    );
}

/// Assert a delete answered nothing but its empty success body: either no body at all
/// or the empty JSON object.
///
/// `pub(crate)` like the refusal assertions above it: the filter-delete suite
/// (`crate::vocabulary_filter_delete_requires_the_filter`) asserts the same thing about
/// the same operation, so it reuses this rather than spelling the shape a second time.
pub(crate) fn empty_body(reply: &Reply, case: &str) {
    let empty = match &reply.body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    };
    assert!(
        empty,
        "case {case:?}: a delete must answer an empty body, got {:?}",
        reply.body
    );
}

/// One member of the custom-vocabulary family: its delete, the member that names the
/// resource, the create that establishes one, and the read that proves presence.
struct Family {
    kind: &'static str,
    create: &'static str,
    delete: &'static str,
    read: &'static str,
    name_member: &'static str,
    /// This family's ARN resource type, as the `TranscribeArn` pattern's
    /// `<type>/<name>` segment spells it.
    resource_type: &'static str,
    /// The create members that carry this resource's content, as that operation
    /// models them (`Phrases` for a vocabulary, `Words` for a filter).
    content: fn(&str) -> Value,
    /// Whether this family's delete is IDEMPOTENT about presence: the recorded answer
    /// for a name the account holds no record under is this operation's empty success
    /// for the custom-vocabulary family, and the absent-resource refusal for the filter
    /// family, whose delete needs the entity to be there.
    idempotent_absent: bool,
    /// Whether the family models a BUILD state, so a record of it is `PENDING` until the
    /// transition the clock owes it has fallen due and a delete has to wait for that
    /// state. A vocabulary has `VocabularyState`; a vocabulary filter models none.
    has_build_state: bool,
}

/// The answer a delete of a name the account holds no record of gets, by family: the
/// empty success where that family's delete is idempotent about presence, the
/// absent-resource refusal where it needs the entity.
fn assert_absent_delete(family: &Family, reply: &Reply, case: &str) {
    if family.idempotent_absent {
        succeeded(reply, case);
        empty_body(reply, case);
    } else {
        absent_resource_refusal(reply, case, family.kind);
    }
}

/// The two resource families whose delete this suite spans. Both are created and
/// deleted by name within this service's own store, so no row depends on another
/// service or on an object store.
fn families() -> Vec<Family> {
    vec![
        Family {
            kind: "vocabulary",
            create: "CreateVocabulary",
            delete: "DeleteVocabulary",
            read: "GetVocabulary",
            name_member: "VocabularyName",
            resource_type: "vocabulary",
            content: |name| {
                json!({
                    "VocabularyName": name,
                    "LanguageCode": "en-US",
                    "Phrases": ["Aardvark", "Transcribe"]
                })
            },
            idempotent_absent: true,
            has_build_state: true,
        },
        Family {
            kind: "vocabulary filter",
            create: "CreateVocabularyFilter",
            delete: "DeleteVocabularyFilter",
            read: "GetVocabularyFilter",
            name_member: "VocabularyFilterName",
            resource_type: "vocabulary-filter",
            content: |name| {
                json!({
                    "VocabularyFilterName": name,
                    "LanguageCode": "en-US",
                    "Words": ["crud", "bogus"]
                })
            },
            idempotent_absent: false,
            has_build_state: false,
        },
    ]
}

/// The names a delete is probed with, one row per form the member's own
/// `@length(1..200)` / `@pattern(^[0-9a-zA-Z._-]+$)` constraint allows
/// (`smithy_query(query="operation", operation="DeleteVocabulary")`): the shortest
/// name, a name using every allowed punctuation character, a long one, and one at the
/// published 200-character ceiling.
fn absent_names() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest allowed name", "a".to_string()),
        (
            "every allowed punctuation character",
            "a.b_c-d.0_9".to_string(),
        ),
        ("a plainly named resource", "never-created-at-all".to_string()),
        ("a name at the 200-character ceiling", "n".repeat(200)),
    ]
}

/// RULE 1: a delete of a custom-vocabulary-family resource this account does not hold
/// is answered the way that family answers presence — for every family and every form
/// of name — and stores nothing, so the listing stays empty.
#[tokio::test]
async fn deleting_a_resource_this_account_does_not_hold_stores_nothing() {
    let transcribe = Transcribe::new();

    for family in families() {
        for (name_case, name) in absent_names() {
            let case = format!("{} / {name_case}", family.kind);
            let reply = transcribe
                .call(family.delete, json!({family.name_member: name}))
                .await;
            assert_absent_delete(&family, &reply, &case);

            // The delete must not MINT what it did not find: the resource is still
            // absent to a reader afterwards.
            let read = transcribe
                .call(family.read, json!({family.name_member: name}))
                .await;
            refused(&read, &case);
        }
    }

    // Nor to a listing: both stores are still empty after every delete above.
    let vocabularies = transcribe
        .call("ListVocabularies", json!({"MaxResults": 100}))
        .await;
    assert_eq!(
        vocabularies.ok("the vocabulary listing after absent deletes")["Vocabularies"],
        json!([]),
        "an absent delete must store nothing, got {:?}",
        vocabularies.body
    );
    let filters = transcribe
        .call("ListVocabularyFilters", json!({"MaxResults": 100}))
        .await;
    assert_eq!(
        filters.ok("the filter listing after absent deletes")["VocabularyFilters"],
        json!([]),
        "an absent delete must store nothing, got {:?}",
        filters.body
    );
}

/// RULE 1, the repeat-delete axis: the delete that removed a resource succeeds, and
/// every later run of the same call answers that family's way about a record that is no
/// longer there — however many times a caller runs it, the resource stays gone.
#[tokio::test]
async fn repeating_the_delete_of_a_resource_keeps_it_gone() {
    let transcribe = Transcribe::new();

    for family in families() {
        let name = format!("deleted-once-{}", family.kind.replace(' ', "-"));
        let case = format!("{} / repeated delete", family.kind);
        succeeded(
            &transcribe
                .call(family.create, (family.content)(&name))
                .await,
            &case,
        );

        // A vocabulary is PENDING while its build is owed, and a delete while it is
        // still being processed is the documented refusal (pinned by
        // `crate::vocabularies`), so the row waits for the STATE the clock owes that
        // build — polled through the family's own reader — rather than for a span of the
        // wall clock. The filter family models no build state and needs no wait.
        if family.has_build_state {
            transcribe.vocabulary_ready(&name, None).await;
        }

        for attempt in 1..=3 {
            let case = format!("{case} #{attempt}");
            let reply = transcribe
                .call(family.delete, json!({family.name_member: name}))
                .await;
            if attempt == 1 {
                succeeded(&reply, &case);
                empty_body(&reply, &case);
            } else {
                assert_absent_delete(&family, &reply, &case);
            }
            refused(
                &transcribe
                    .call(family.read, json!({family.name_member: name}))
                    .await,
                &case,
            );
        }
    }
}

/// RULE 1 does not reach across families: a delete answers for the resources of its
/// OWN type, so a vocabulary and a filter sharing one name are independent.
#[tokio::test]
async fn a_delete_only_removes_its_own_resource_family() {
    let transcribe = Transcribe::new();
    let shared = "shared.name";
    let case = "a vocabulary and a filter of the same name";

    succeeded(
        &transcribe
            .call("CreateVocabulary", (families()[0].content)(shared))
            .await,
        case,
    );
    succeeded(
        &transcribe
            .call("CreateVocabularyFilter", (families()[1].content)(shared))
            .await,
        case,
    );
    // The wait is for the STATE the clock owes the vocabulary's build, polled through the
    // family's own reader (`Transcribe::vocabulary_ready`), never for a span of the wall
    // clock; the filter family models no build state, so it needs none.
    transcribe.vocabulary_ready(shared, None).await;

    let deleted = transcribe
        .call("DeleteVocabulary", json!({"VocabularyName": shared}))
        .await;
    succeeded(&deleted, case);
    refused(
        &transcribe
            .call("GetVocabulary", json!({"VocabularyName": shared}))
            .await,
        case,
    );
    // The filter of the same name is untouched, and the now-absent vocabulary delete
    // answers its empty success without touching it.
    transcribe
        .call("GetVocabularyFilter", json!({"VocabularyFilterName": shared}))
        .await
        .ok(case);
    assert_absent_delete(
        &families()[0],
        &transcribe
            .call("DeleteVocabulary", json!({"VocabularyName": shared}))
            .await,
        case,
    );
    transcribe
        .call("GetVocabularyFilter", json!({"VocabularyFilterName": shared}))
        .await
        .ok(case);

    let removed = transcribe
        .call(
            "DeleteVocabularyFilter",
            json!({"VocabularyFilterName": shared}),
        )
        .await;
    succeeded(&removed, case);
    refused(
        &transcribe
            .call("GetVocabularyFilter", json!({"VocabularyFilterName": shared}))
            .await,
        case,
    );
}

/// One way a resource's tag set comes to be empty: the keys it is tagged with first,
/// and the `UntagResource` calls that then remove them.
struct Emptying {
    row: &'static str,
    keys: Vec<&'static str>,
    untags: Vec<Vec<&'static str>>,
}

/// The ways a tag set empties, one row per axis the rule generalises over: whether
/// the resource was ever tagged, how many keys are removed, in how many calls, and
/// whether the untag also names keys the resource does not carry.
fn emptyings() -> Vec<Emptying> {
    vec![
        Emptying {
            row: "never tagged",
            keys: vec![],
            untags: vec![],
        },
        Emptying {
            row: "one tag removed",
            keys: vec!["team"],
            untags: vec![vec!["team"]],
        },
        Emptying {
            row: "three tags removed in one call",
            keys: vec!["team", "env", "cost"],
            untags: vec![vec!["team", "env", "cost"]],
        },
        Emptying {
            row: "three tags removed one call at a time",
            keys: vec!["team", "env", "cost"],
            untags: vec![vec!["cost"], vec!["env"], vec!["team"]],
        },
        Emptying {
            row: "an untag naming keys the resource does not carry",
            keys: vec!["team"],
            untags: vec![vec!["team", "not-a-key", "another-absent-key"]],
        },
    ]
}

/// RULE 2: `ListTagsForResource` reports an EMPTY `Tags` list for a resource that
/// carries no tags, however it came to carry none — never by leaving the member out.
///
/// The rows differ along what the rule generalises over: the resource family, whether
/// the resource was ever tagged, how many keys were removed and in how many calls,
/// and whether the untag named keys the resource does not carry.
#[tokio::test]
async fn an_empty_tag_set_is_reported_as_an_empty_list() {
    let transcribe = Transcribe::new();

    let emptyings = emptyings();

    for family in families() {
        for (index, Emptying { row, keys, untags }) in emptyings.iter().enumerate() {
            let case = format!("{} / {row}", family.kind);
            let name = format!("tagless-{}-{index}", family.kind.replace(' ', "-"));
            let created = transcribe
                .call(family.create, (family.content)(&name))
                .await;
            succeeded(&created, &case);

            let resource_arn = format!(
                "arn:aws:transcribe:us-east-1:{account}:{resource_type}/{name}",
                account = engine::emulator_account_id(),
                resource_type = family.resource_type,
            );

            if !keys.is_empty() {
                let tags: Vec<Value> = keys
                    .iter()
                    .map(|key| json!({"Key": key, "Value": "any"}))
                    .collect();
                transcribe
                    .call(
                        "TagResource",
                        json!({"ResourceArn": resource_arn, "Tags": tags}),
                    )
                    .await
                    .ok(&case);
                let listed = transcribe
                    .call("ListTagsForResource", json!({"ResourceArn": resource_arn}))
                    .await;
                let reported = listed.ok(&case)["Tags"]
                    .as_array()
                    .unwrap_or_else(|| panic!("case {case:?}: expected a Tags list, got {:?}", listed.body))
                    .len();
                assert_eq!(
                    reported,
                    keys.len(),
                    "case {case:?}: a tagged resource must report every tag, got {:?}",
                    listed.body
                );
            }

            for untag in untags {
                succeeded(
                    &transcribe
                        .call(
                            "UntagResource",
                            json!({"ResourceArn": resource_arn, "TagKeys": untag}),
                        )
                        .await,
                    &case,
                );
            }

            let empty = transcribe
                .call("ListTagsForResource", json!({"ResourceArn": resource_arn}))
                .await;
            empty.ok(&case);
            assert_eq!(
                empty.member(&case, "ResourceArn"),
                resource_arn,
                "case {case:?}: the answer must name the resource asked about, got {:?}",
                empty.body
            );
            assert_eq!(
                empty.body.get("Tags"),
                Some(&json!([])),
                "case {case:?}: a resource carrying no tags must report an empty Tags \
                 list, got {:?}",
                empty.body
            );

            // The empty list is the resource's CURRENT tag set, not a constant: a tag
            // added afterwards is reported, and removing it reports empty again.
            transcribe
                .call(
                    "TagResource",
                    json!({
                        "ResourceArn": resource_arn,
                        "Tags": [{"Key": "re-tagged", "Value": "yes"}]
                    }),
                )
                .await
                .ok(&case);
            let retagged = transcribe
                .call("ListTagsForResource", json!({"ResourceArn": resource_arn}))
                .await;
            assert_eq!(
                retagged.ok(&case)["Tags"],
                json!([{"Key": "re-tagged", "Value": "yes"}]),
                "case {case:?}: a tag added after the set emptied must be reported, got {:?}",
                retagged.body
            );
            succeeded(
                &transcribe
                    .call(
                        "UntagResource",
                        json!({"ResourceArn": resource_arn, "TagKeys": ["re-tagged"]}),
                    )
                    .await,
                &case,
            );
            let again = transcribe
                .call("ListTagsForResource", json!({"ResourceArn": resource_arn}))
                .await;
            assert_eq!(
                again.ok(&case)["Tags"],
                json!([]),
                "case {case:?}: removing the last tag again must report an empty list, \
                 got {:?}",
                again.body
            );
        }
    }
}
