// SPDX-License-Identifier: CC0-1.0

//! How this service words "there is no such custom language model", over the wire.
//!
//! THE RULE THESE ROWS ASSERT: the not-found wording belongs to the resource FAMILY, not
//! to the operation and not to a template over every family — so every operation that
//! looks a custom language model up answers the SAME sentence for a name the serving
//! Region holds no model under ([`ABSENT_MODEL_MESSAGE`]), whichever way that operation
//! addresses the model (by `ModelName`, or by the ARN a tagging call carries). The text
//! is entirely FIXED: it names the family the way that family's own documentation names
//! it ("the specified custom language model") and carries no value of the request, so no
//! name the caller asked about appears in it.
//!
//! The rows are a TABLE spanning what the rule generalises over: which operation looks
//! the model up (the read, the update, the delete, and all three tagging operations,
//! which address it by ARN), crossed with the forms of name the member's own
//! `@length(1..200)` / `@pattern(^[0-9a-zA-Z._-]+$)` constraint allows
//! (`smithy_query(query="operation", operation="DescribeLanguageModel")`): the shortest
//! allowed name, every allowed punctuation character, a plainly spelled one, and a name
//! at the published 200-character ceiling.
//!
//! Two more tests keep the rule honest: the sentence is this FAMILY's — another family
//! asked about the very same absent name answers its own wording — and it is about
//! ABSENCE, so a model the Region does hold is read, updated, tagged and deleted without
//! it.
//!
//! Every call goes through the same `MultiServiceRouter` the production router
//! dispatches on, reusing the transcription-job suite's harness
//! (`crate::transcription_jobs`) and this family's request builder
//! (`crate::language_models::language_model_request`).

use serde_json::{json, Value};

use crate::language_models::language_model_request;
use crate::transcription_jobs::Transcribe;

/// How this service words "there is no such custom language model"
/// (`crate::state::LANGUAGE_MODEL_NOT_FOUND_MESSAGE`).
///
/// `pub(crate)`: the delete suite of this family
/// (`crate::language_model_delete_requires_the_model`) asserts the same sentence, and
/// reads it from here rather than keeping a second copy of it.
pub(crate) const ABSENT_MODEL_MESSAGE: &str = "The specified custom language model \
     couldn't be found. Check the language model name and try your request again.";

/// The status this service puts on every client error
/// (`crate::scaffolded::wire_status`).
const CLIENT_ERROR_STATUS: u16 = 400;

/// The error shape this service's absent-resource refusals carry.
const ABSENT_RESOURCE_SHAPE: &str = "BadRequestException";

/// The `LanguageCode` and `BaseModelName` the held model below is created with: values
/// of the modelled enums (`smithy_query(query="shape",
/// shape="CLMLanguageCode,BaseModelName")`).
const LANGUAGE_CODE: &str = "en-US";
const BASE_MODEL: &str = "WideBand";

/// The IAM role an update may re-point a model's training access at, in the form
/// `DataAccessRoleArn`'s `@pattern` pins.
const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/TranscribeAccess";

/// The Region every call here is served in — the one a request carrying no SigV4
/// credential scope is served in — so a tagging ARN is predictable.
const REGION: &str = "us-east-1";

/// An Amazon Transcribe ARN of `resource_type` naming `name`, in the form
/// `TranscribeArn`'s `@pattern` pins.
fn arn(resource_type: &str, name: &str) -> String {
    format!(
        "arn:aws:transcribe:{REGION}:{account}:{resource_type}/{name}",
        account = engine::emulator_account_id()
    )
}

/// One operation that looks a custom language model up.
struct Lookup {
    /// What this row varies, named in every assertion.
    case: &'static str,
    operation: &'static str,
    /// This operation's request for the model named `name`.
    request: fn(&str) -> Value,
}

/// Every operation of this service that resolves a custom language model: the read, the
/// update, the delete, and the three tagging operations, which name it by ARN.
fn lookups() -> Vec<Lookup> {
    vec![
        Lookup {
            case: "the read",
            operation: "DescribeLanguageModel",
            request: |name| json!({"ModelName": name}),
        },
        Lookup {
            case: "the update",
            operation: "UpdateLanguageModel",
            request: |name| {
                json!({
                    "ModelName": name,
                    // The one other member of this operation that names something, and
                    // optional (`smithy_query(query="operation",
                    // operation="UpdateLanguageModel")`): the lookup is what refuses the
                    // request, before anything would be re-encrypted.
                    "DataAccessRoleArn": ROLE_ARN
                })
            },
        },
        Lookup {
            case: "the delete",
            operation: "DeleteLanguageModel",
            request: |name| json!({"ModelName": name}),
        },
        Lookup {
            case: "the tag listing, addressing the model by ARN",
            operation: "ListTagsForResource",
            request: |name| json!({"ResourceArn": arn("language-model", name)}),
        },
        Lookup {
            case: "the tag write, addressing the model by ARN",
            operation: "TagResource",
            request: |name| {
                json!({
                    "ResourceArn": arn("language-model", name),
                    "Tags": [{"Key": "team", "Value": "speech"}]
                })
            },
        },
        Lookup {
            case: "the tag removal, addressing the model by ARN",
            operation: "UntagResource",
            request: |name| {
                json!({
                    "ResourceArn": arn("language-model", name),
                    "TagKeys": ["team"]
                })
            },
        },
    ]
}

/// The forms of absent name every row is probed with, one per form the member's own
/// constraint allows, including both published length bounds.
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

/// RULE: every operation that looks a custom language model up answers this family's one
/// not-found sentence for a name the Region holds no model under, for every form of
/// name, and the refusal carries no value of the request.
#[tokio::test]
async fn every_lookup_of_an_absent_model_answers_this_familys_sentence() {
    let transcribe = Transcribe::new();

    for lookup in lookups() {
        for (name_case, name) in absent_names() {
            let case = format!("{} — {} / {name_case}", lookup.operation, lookup.case);
            let refused = transcribe
                .call(lookup.operation, (lookup.request)(&name))
                .await;
            refused.refused(&case, CLIENT_ERROR_STATUS, ABSENT_RESOURCE_SHAPE);
            let message = refused.error_message(&case);
            assert_eq!(
                message, ABSENT_MODEL_MESSAGE,
                "case {case:?}: expected this family's not-found sentence \
                 {ABSENT_MODEL_MESSAGE:?}, got {message:?}"
            );
            // A name of three characters or fewer is skipped: the wording contains most
            // short letter runs somewhere.
            if name.len() > 3 {
                assert!(
                    !message.contains(&name),
                    "case {case:?}: the refusal must not echo the requested {name:?}, \
                     got {message:?}"
                );
            }
        }
    }
}

/// The sentence belongs to THIS family: another family asked about the very same absent
/// name answers its own wording, so a client can tell the two refusals apart.
#[tokio::test]
async fn another_familys_refusal_about_the_same_name_is_worded_its_own_way() {
    let transcribe = Transcribe::new();
    let name = "never-created-at-all";

    for (case, operation, request) in [
        (
            "a custom vocabulary of that name",
            "GetVocabulary",
            json!({"VocabularyName": name}),
        ),
        (
            "a custom vocabulary filter of that name",
            "GetVocabularyFilter",
            json!({"VocabularyFilterName": name}),
        ),
        (
            "a transcription job of that name",
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": name}),
        ),
    ] {
        let refused = transcribe.call(operation, request).await;
        refused.refused(case, CLIENT_ERROR_STATUS, ABSENT_RESOURCE_SHAPE);
        let message = refused.error_message(case);
        assert_ne!(
            message, ABSENT_MODEL_MESSAGE,
            "case {case:?}: another family must not answer the language-model sentence, \
             got {message:?}"
        );
    }
}

/// The sentence is about ABSENCE: a model the Region holds is looked up by every one of
/// those operations without it, and only once the delete has removed it does the
/// sentence come back.
#[tokio::test]
async fn a_model_the_region_holds_is_not_refused() {
    let transcribe = Transcribe::new();
    let name = "held.model-01";
    let case = "a model the Region holds";

    transcribe
        .call(
            "CreateLanguageModel",
            language_model_request(name, LANGUAGE_CODE, BASE_MODEL),
        )
        .await
        .ok_status(case, 200);

    let read = transcribe
        .call("DescribeLanguageModel", json!({"ModelName": name}))
        .await;
    assert_eq!(
        read.ok(case)["LanguageModel"]["ModelName"].as_str(),
        Some(name),
        "case {case:?}: the held model must read back, got {:?}",
        read.body
    );
    transcribe
        .call(
            "ListTagsForResource",
            json!({"ResourceArn": arn("language-model", name)}),
        )
        .await
        .ok(case);

    // Training has to have finished before the model may be deleted, so the delete waits
    // for the status the clock owes it rather than for a span of the wall clock.
    transcribe.language_model_completed(name, None).await;
    transcribe
        .call("DeleteLanguageModel", json!({"ModelName": name}))
        .await
        .ok_status(case, 200);

    // Removed, so every lookup of the name is this family's sentence again.
    for lookup in lookups() {
        let gone = format!("{} — {} after the delete", lookup.operation, lookup.case);
        let refused = transcribe
            .call(lookup.operation, (lookup.request)(name))
            .await;
        refused.refused(&gone, CLIENT_ERROR_STATUS, ABSENT_RESOURCE_SHAPE);
        let message = refused.error_message(&gone);
        assert_eq!(
            message, ABSENT_MODEL_MESSAGE,
            "case {gone:?}: expected this family's not-found sentence, got {message:?}"
        );
    }
}
