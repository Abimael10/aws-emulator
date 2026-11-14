// SPDX-License-Identifier: CC0-1.0

//! What a delete in this service's custom-vocabulary families answers for a name the
//! serving Region holds no record under, over the wire.
//!
//! THE RULE THESE ROWS ASSERT: a custom-vocabulary delete is IDEMPOTENT about PRESENCE.
//! What the call asks for is that the Region hold no vocabulary of that name, and a name
//! it already holds none under is in exactly that state, so the request is answered with
//! the operation's own empty success (HTTP 200 and no members — the model gives both
//! deletes `Unit` output, `smithy_query(query="operation",
//! operation="DeleteVocabulary,DeleteMedicalVocabulary")`) and NOTHING is stored or
//! removed. The one thing such a delete refuses is a record whose BUILD has not finished
//! — "Your request didn't pass one or more validation tests. This can occur when the
//! entity you're trying to delete doesn't exist or if it's in a non-terminal state (such
//! as IN PROGRESS)", `BadRequestException`'s own documentation — and a refused delete
//! removes nothing.
//!
//! The rows are a TABLE spanning what the rule generalises over: which family's delete
//! is called (both custom-vocabulary families, which the rule covers together), the form
//! of the name (every form the member's own `@length(1..200)` /
//! `@pattern(^[0-9a-zA-Z._-]+$)` constraint allows, at both published length bounds), how
//! the Region comes to hold no such record (never created, created and already deleted,
//! held only by the OTHER family, held only by another Region), and how many times the
//! delete is run.
//!
//! The boundary is pinned too: the empty success is about PRESENCE only, so a record
//! whose build is still owed is refused, and a delete never MINTS the record it did not
//! find — every reader still answers "absent" afterwards.
//!
//! Every call goes through the same `MultiServiceRouter` the production router
//! dispatches on, reusing the transcription-job suite's harness
//! (`crate::transcription_jobs`) and the shared delete assertions
//! (`crate::vocabulary_delete_and_tag_sets`).

use serde_json::json;

use crate::transcription_jobs::Transcribe;
use crate::vocabulary_delete_and_tag_sets::{empty_body, succeeded};

/// The status this service puts on every client error
/// (`crate::scaffolded::wire_status`).
const CLIENT_ERROR_STATUS: u16 = 400;

/// The shape every absent-record and non-terminal-state refusal of this service carries.
const CLIENT_ERROR_SHAPE: &str = "BadRequestException";

/// The success status this awsJson1_1 service answers an empty output with.
const SUCCESS_STATUS: u16 = 200;

/// `en-US`: a value of the service-wide `LanguageCode` enum every family accepts, and
/// the only language Amazon Transcribe Medical supports.
const LANGUAGE_CODE: &str = "en-US";

/// One custom-vocabulary family whose delete the rule covers.
struct Family {
    /// What this row varies, named in every assertion.
    case: &'static str,
    create: &'static str,
    read: &'static str,
    delete: &'static str,
    /// The create request that establishes a record named `name` — each family's own
    /// source of terms.
    create_body: fn(&str) -> serde_json::Value,
}

/// Both custom-vocabulary families.
fn families() -> Vec<Family> {
    vec![
        Family {
            case: "the custom vocabulary family",
            create: "CreateVocabulary",
            read: "GetVocabulary",
            delete: "DeleteVocabulary",
            create_body: |name| {
                json!({
                    "VocabularyName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "Phrases": ["Aardvark", "Transcribe"]
                })
            },
        },
        Family {
            case: "the custom medical vocabulary family",
            create: "CreateMedicalVocabulary",
            read: "GetMedicalVocabulary",
            delete: "DeleteMedicalVocabulary",
            // This family's terms come only from a table file, and no object store is
            // mounted here, so its build settles in the service's own FAILED state —
            // terminal, which is all a delete needs of it.
            create_body: |name| {
                json!({
                    "VocabularyName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "VocabularyFileUri": "s3://DOC-EXAMPLE-BUCKET/clinical-terms.txt"
                })
            },
        },
    ]
}

/// The forms of name a delete that finds nothing is probed with, one per form the
/// member's own constraint allows, including both published length bounds.
fn absent_names() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest allowed name", "a".to_string()),
        (
            "every allowed punctuation character",
            "a.b_c-d.0_9".to_string(),
        ),
        ("a plainly spelled name", "never-created-at-all".to_string()),
        ("a name at the 200-character ceiling", "n".repeat(200)),
    ]
}

/// RULE: a delete of a name the Region holds no record under is the operation's empty
/// success, for both families and every form of name — and it mints nothing, so the
/// family's reader still answers "absent" and the listing is still empty.
#[tokio::test]
async fn deleting_a_name_this_region_holds_no_record_under_is_an_empty_success() {
    let transcribe = Transcribe::new();

    for family in families() {
        for (name_case, name) in absent_names() {
            let case = format!("{} / {name_case}", family.delete);

            // Run it more than once: an idempotent delete answers the same thing however
            // many times a caller runs it.
            for attempt in 1..=3 {
                let attempt_case = format!("{case} #{attempt}");
                let reply = transcribe
                    .call(family.delete, json!({"VocabularyName": name}))
                    .await;
                reply.ok_status(&attempt_case, SUCCESS_STATUS);
                empty_body(&reply, &attempt_case);
            }

            // The delete must not MINT what it did not find.
            transcribe
                .call(family.read, json!({"VocabularyName": name}))
                .await
                .refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
        }
    }

    for (list, member) in [
        ("ListVocabularies", "Vocabularies"),
        ("ListMedicalVocabularies", "Vocabularies"),
    ] {
        let listed = transcribe.call(list, json!({"MaxResults": 100})).await;
        assert_eq!(
            listed.ok(list)[member],
            json!([]),
            "case {list:?}: an absent delete must store nothing, got {:?}",
            listed.body
        );
    }
}

/// RULE, the ways a Region comes to hold no such record: a record that was deleted
/// already, one held only by the OTHER family, and one held only by another Region. Each
/// one answers the same empty success, and leaves the record that IS there untouched.
#[tokio::test]
async fn every_way_a_record_is_absent_answers_the_same_empty_success() {
    let transcribe = Transcribe::new();
    let shared = "shared.name";
    let home = Some("us-east-1");
    let elsewhere = Some("us-west-2");

    for family in families() {
        // A record of this family in another Region, and one of the OTHER family at
        // home: neither is this delete's to remove.
        transcribe
            .call_in_region(family.create, (family.create_body)(shared), elsewhere)
            .await
            .ok_status(family.case, SUCCESS_STATUS);
        let other = families()
            .into_iter()
            .find(|row| row.delete != family.delete)
            .expect("the table holds both families");
        transcribe
            .call_in_region(other.create, (other.create_body)(shared), home)
            .await
            .ok_status(other.case, SUCCESS_STATUS);

        // The home Region holds no record of THIS family under the name, so its delete
        // is the empty success...
        let reply = transcribe
            .call_in_region(family.delete, json!({"VocabularyName": shared}), home)
            .await;
        succeeded(&reply, family.case);
        empty_body(&reply, family.case);

        // ...and neither the other Region's record nor the other family's was touched.
        let in_other_region = transcribe
            .call_in_region(family.read, json!({"VocabularyName": shared}), elsewhere)
            .await;
        assert_eq!(
            in_other_region.ok(family.case)["VocabularyName"].as_str(),
            Some(shared),
            "case {:?}: another Region's record must survive the delete, got {:?}",
            family.case,
            in_other_region.body
        );
        let in_other_family = transcribe
            .call_in_region(other.read, json!({"VocabularyName": shared}), home)
            .await;
        assert_eq!(
            in_other_family.ok(other.case)["VocabularyName"].as_str(),
            Some(shared),
            "case {:?}: the other family's record must survive the delete, got {:?}",
            other.case,
            in_other_family.body
        );

        // Clean up, so the next family starts from an empty store: both records are
        // removed through their own calls, each in its own Region — each after the state
        // the clock owes its build has settled, polled through the shared harness's own
        // wait on that family's reader.
        for (read, delete, region) in [
            (family.read, family.delete, elsewhere),
            (other.read, other.delete, home),
        ] {
            transcribe
                .vocabulary_state_settled(read, shared, region)
                .await;
            transcribe
                .call_in_region(delete, json!({"VocabularyName": shared}), region)
                .await
                .ok_status("the cleanup delete", SUCCESS_STATUS);
        }
    }
}

/// The BOUNDARY of the rule: the empty success is about presence only. A record whose
/// build has not finished is in a non-terminal state, so its delete is refused and the
/// record survives; once the build the clock owed has settled, the same delete succeeds.
#[tokio::test]
async fn a_record_whose_build_is_still_owed_is_refused_rather_than_deleted() {
    let transcribe = Transcribe::new();

    for family in families() {
        let name = format!("pending-{}", family.delete);
        transcribe
            .call(family.create, (family.create_body)(&name))
            .await
            .ok_status(family.case, SUCCESS_STATUS);

        // Straight after the create the build is still owed, so the record is PENDING
        // and the delete is refused.
        transcribe
            .call(family.delete, json!({"VocabularyName": name}))
            .await
            .refused(family.case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);

        // Refused, so nothing was removed.
        let read = transcribe
            .call(family.read, json!({"VocabularyName": name}))
            .await;
        assert_eq!(
            read.ok(family.case)["VocabularyName"].as_str(),
            Some(name.as_str()),
            "case {:?}: a refused delete must leave the record readable, got {:?}",
            family.case,
            read.body
        );

        // Once the build has settled the state that blocked the delete is gone, so the
        // same call removes the record and every repeat of it is the empty success. The
        // wait is for the STATE the clock owes that build, polled through the shared
        // harness's wait on this family's reader, never a span of the wall clock.
        transcribe
            .vocabulary_state_settled(family.read, &name, None)
            .await;
        for attempt in 1..=2 {
            let attempt_case = format!("{} once built #{attempt}", family.case);
            let reply = transcribe
                .call(family.delete, json!({"VocabularyName": name}))
                .await;
            reply.ok_status(&attempt_case, SUCCESS_STATUS);
            empty_body(&reply, &attempt_case);
        }
        transcribe
            .call(family.read, json!({"VocabularyName": name}))
            .await
            .refused(family.case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
    }
}
