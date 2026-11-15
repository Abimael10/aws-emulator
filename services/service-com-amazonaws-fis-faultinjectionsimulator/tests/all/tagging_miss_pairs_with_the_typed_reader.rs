// SPDX-License-Identifier: CC0-1.0

//! WHO decides which error shape a 404 of this service names: the OPERATION that was
//! called, not the record that is missing.
//!
//! RULE: on `restJson1` a client resolves an error shape from the `x-amzn-errortype`
//! header, and the shape a 404 may name there is the one the CALLED operation's own model
//! authorises. FIS's ARN-addressed tag operations declare no error at all
//! (`smithy_query(query="operation", operation="TagResource,UntagResource,
//! ListTagsForResource",
//! model_path="services/service-com-amazonaws-fis-faultinjectionsimulator/smithy-model/com-amazonaws-fis-faultinjectionsimulator.json")`
//! reports `errors: []` for each), so no modelled error shape of this service is theirs to
//! announce and their miss travels under the tagging front door's own generic shape
//! (`NotFoundException`, "Resource was not found"). The readers that ARE asked for a
//! resource of one named kind bind `ResourceNotFoundException` at `@httpError 404`
//! (`smithy_query(query="operation", operation="GetExperimentTemplate,GetExperiment")`),
//! so they keep that modelled shape and its kind-specific sentence.
//!
//! What the tag suites next door assert row by row, each surface on its own, this suite
//! PAIRS: one and the same absent record is addressed through BOTH surfaces, in one
//! emulator, within one case — so the two shapes cannot be explained by the record's state
//! or by the id the request carries, only by which operation was called. That pairing is
//! the invariant a client depends on: a tag call and the resource's own reader report the
//! same missing record under the shape each of their models allows.
//!
//! The rows differ along what the rule generalises over: which KIND of taggable record is
//! missing (an experiment template and an experiment, the two that carry caller tags),
//! whether it NEVER existed or existed and was deleted, and — inside each row — which of
//! the three tag operations serves the ARN-addressed call and whether the shape is read
//! from the header or from the body beside it.
//!
//! The resources, the ARN helpers and the one-call harness come from the tag suites
//! (`resource_tagging`, `tagging_response_envelope`), so these are the very same calls
//! whose status, sentence and persisted effect those suites assert.

use serde_json::json;

use crate::experiment_lifecycle::{emulator, send, FALLBACK_REGION};
use crate::resource_tagging::{new_template, template_id_shaped, ACCOUNT_ID, ID_SUFFIX_LEN};
use crate::tagging_response_envelope::{call, operations, Envelope, NOT_FOUND};

/// The shape the ARN-addressed tagging surface answers a miss with: its operations model no
/// error, so this is the front door's own.
const SURFACE_NOT_FOUND: &str = "NotFoundException";

/// The shape an operation whose model DECLARES a not-found error answers it with.
const MODELLED_NOT_FOUND: &str = "ResourceNotFoundException";

/// One absent record, reachable through both surfaces: the ARN the tagging surface
/// addresses it by, the path its own typed reader takes, and the sentence that reader
/// words the miss with.
struct Missing {
    case: String,
    arn: String,
    reader: &'static str,
    reader_uri: String,
    reader_sentence: String,
}

/// Assert one answer carries `status`, names `shape` in BOTH the header a `restJson1`
/// client resolves the shape from and the body beside it, and words the miss `sentence`.
fn assert_announced(answer: &Envelope, status: u16, shape: &str, sentence: &str, case: &str) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some(shape),
        "case {case:?}: expected the error-type header to name {shape:?}, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.code(),
        shape,
        "case {case:?}: expected the body to name {shape:?}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.message(),
        sentence,
        "case {case:?}: expected the sentence {sentence:?}, got {:?}",
        answer.message()
    );
}

/// An ARN of this service for `resource_type` and `id`, as this service issues one.
fn arn_of(resource_type: &str, id: &str) -> String {
    format!("arn:aws:fis:{FALLBACK_REGION}:{ACCOUNT_ID}:{resource_type}/{id}")
}

/// RULE: for ONE missing record, the ARN-addressed tag operations announce the tagging
/// surface's own shape and its own typed reader announces the modelled one — the two
/// differ, and which one a 404 names follows the operation that was called.
#[tokio::test]
async fn one_missing_record_is_announced_per_operation_not_per_record() {
    let router = emulator();

    // A template this suite creates and then deletes: the ARN stays one this service
    // issued and the record behind it is gone, which is the state a call meets after a
    // delete.
    let (deleted_id, deleted_arn) = new_template(&router, "deleted template", &json!({})).await;
    let deleted = send(
        &router,
        "DELETE",
        &format!("/experimentTemplates/{deleted_id}"),
        None,
        FALLBACK_REGION,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "expected HTTP 200 from DeleteExperimentTemplate, got {} with {:?}",
        deleted.status, deleted.body
    );

    // Ids of the form this service mints that no call ever minted, one per kind of record
    // that carries caller tags.
    let unminted_template = template_id_shaped(&"Y".repeat(ID_SUFFIX_LEN));
    let unminted_experiment = format!("EXP{}", "Y".repeat(ID_SUFFIX_LEN));

    let rows = [
        Missing {
            case: "an experiment template that has since been deleted".to_string(),
            arn: deleted_arn,
            reader: "GetExperimentTemplate",
            reader_uri: format!("/experimentTemplates/{deleted_id}"),
            // The template reader names its own resource KIND and interpolates nothing.
            reader_sentence: "ExperimentTemplate not found.".to_string(),
        },
        Missing {
            case: "an experiment template id no call minted".to_string(),
            arn: arn_of("experiment-template", &unminted_template),
            reader: "GetExperimentTemplate",
            reader_uri: format!("/experimentTemplates/{unminted_template}"),
            reader_sentence: "ExperimentTemplate not found.".to_string(),
        },
        Missing {
            case: "an experiment id no call minted".to_string(),
            arn: arn_of("experiment", &unminted_experiment),
            reader: "GetExperiment",
            reader_uri: format!("/experiments/{unminted_experiment}"),
            // The experiment reader words the miss after the ID THE REQUEST named, so this
            // row also pins that the modelled sentence is built from the request.
            reader_sentence: format!("Experiment with ID {unminted_experiment} not found."),
        },
    ];

    for row in rows {
        // The ARN-addressed surface: every one of its operations, on the same ARN.
        for operation in operations() {
            let case = format!("{} / {}", operation.as_str(), row.case);
            let answer = operation.call_on(&router, &row.arn).await;
            assert_announced(&answer, 404, SURFACE_NOT_FOUND, NOT_FOUND, &case);
        }

        // The SAME record through its own typed reader, which models a not-found error.
        let reader_case = format!("{} / {}", row.reader, row.case);
        let read = call(&router, "GET", &row.reader_uri, None).await;
        assert_announced(
            &read,
            404,
            MODELLED_NOT_FOUND,
            &row.reader_sentence,
            &reader_case,
        );

        // The pairing: one record, two surfaces, two shapes — and the tag call still
        // announces the surface's shape after the reader has answered its own, so neither
        // answer is a function of what was asked before it.
        for operation in operations() {
            let case = format!("{} / after {} / {}", operation.as_str(), row.reader, row.case);
            let again = operation.call_on(&router, &row.arn).await;
            assert_announced(&again, 404, SURFACE_NOT_FOUND, NOT_FOUND, &case);
            assert_ne!(
                again.error_type, read.error_type,
                "case {case:?}: the ARN-addressed miss and the typed reader's miss must not \
                 share one shape, both answered {:?}",
                again.error_type
            );
        }
    }
}

/// The other side of the rule: a record that IS there is served by both surfaces, and a
/// served call announces no shape at all — so the header belongs to a refusal alone and the
/// shapes above are not something the tagging surface writes on every answer.
#[tokio::test]
async fn a_record_that_is_there_is_served_by_both_surfaces_naming_no_shape() {
    let router = emulator();
    let (id, arn) = new_template(&router, "live template", &json!({})).await;

    for operation in operations() {
        let case = format!("{} / a live experiment template", operation.as_str());
        let served = operation.call_on(&router, &arn).await;
        assert!(
            served.status < 400,
            "case {case:?}: expected the live record to be served, got {} with {:?}",
            served.status,
            served.body
        );
        assert_eq!(
            served.error_type, None,
            "case {case:?}: a served call names no error shape, got {:?}",
            served.error_type
        );
    }

    let read = call(&router, "GET", &format!("/experimentTemplates/{id}"), None).await;
    assert_eq!(
        read.status, 200,
        "expected HTTP 200 from GetExperimentTemplate for a live template, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.error_type, None,
        "a served read names no error shape, got {:?}",
        read.error_type
    );
}
