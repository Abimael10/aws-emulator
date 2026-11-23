// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the two members of `CreateImagePipeline` that describe the CALL
//! rather than the pipeline — `dryRun` and `clientToken` — and so decide whether the create is
//! PERFORMED at all.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what a
//! signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * `dryRun` "[v]alidates the required permissions and request parameters without performing
//!   the operation. If validation succeeds, the operation returns a DryRunOperationException
//!   error response."
//!   (`smithy_query(query="doc",shape="CreateImagePipelineRequest",member="dryRun")`) — so a dry
//!   run that validates is answered with that modelled error (HTTP 412) and writes NOTHING: the
//!   ARN it would have minted resolves to no pipeline, and both the name and the client token it
//!   named are still free for the request that performs the create.
//! * `clientToken` is "[a] unique, case-sensitive identifier you provide to ensure that the
//!   operation runs no more than one time. If you retry a request with the same client token,
//!   Image Builder returns the original response without running the operation again"
//!   (`smithy_query(query="doc",shape="CreateImagePipelineRequest",member="clientToken")`) — so a
//!   retry of the same request answers the first response member for member, down to its
//!   `requestId`, while the same token carrying parameters that DIFFER is the modelled
//!   `IdempotentParameterMismatchException` (HTTP 400) and leaves the stored pipeline as it was.
//! * Neither member is part of the pipeline: a read of the created pipeline reports the resource
//!   the request described and carries neither of the two members that described the call.
//!
//! The rows below span what the rules generalise over: which optional members the dry-run
//! request carried beside the ones a create cannot omit (none, a schedule, tags, a status, the
//! test settings), and which parameter the retry changed (the description, the status, the tags,
//! the schedule, the test settings, the metadata flag, the name itself).
//!
//! The harness, the fixture, the pipeline's own read and the refusal wording are
//! [`super::image_pipeline_schedule`]'s: they are borrowed from that sibling rather than copied,
//! so one emulator, one create body, one `GetImagePipeline` URI and one refusal sentence serve
//! both suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::image_pipeline_schedule::{
    assert_ok, create_pipeline, emulator, error_code, error_message, get_pipeline,
    illegal_characters, pipeline_arn, refusal_message, text, Fixture,
};

/// The sentence a validated dry run is answered with: FIXED text naming the member as the API
/// spells it (`crate::state::dry_run_message`).
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but dryRun flag is set.";

/// The sentence a spent client token carrying different parameters is refused with
/// (`crate::state::idempotent_parameter_mismatch_message`).
const MISMATCH_MESSAGE: &str =
    "The request parameters do not match those in the original idempotent request.";

/// The kind token a missing-resource report names an image-pipeline ARN by: the shape name this
/// model gives the kind the ARN carries (`crate::state::arn_resource_type`).
const PIPELINE_KIND: &str = "ImagePipeline";

/// A `CreateImagePipeline` body for `name` carrying `extra`, with `dryRun` stated either way —
/// the member carries a default of `false`, so stating it is how a case asks for the validation
/// WITHOUT the create, and the same body with it `false` is the create itself.
fn create_body(
    fixture: &Fixture,
    name: &str,
    client_token: &str,
    extra: &Value,
    dry_run: bool,
) -> Value {
    let mut body = match fixture.create_body(name, client_token, extra.clone()) {
        Value::Object(body) => body,
        other => panic!("expected a create body object, got {other}"),
    };
    body.insert("dryRun".to_string(), json!(dry_run));
    Value::Object(body)
}

/// The pipeline stored under `arn`, or `None` when this Region holds none under it — in which
/// case the read is this service's missing-resource report, naming the kind the ARN identifies
/// and the ARN the request carried.
async fn stored_pipeline(case: &str, router: &MultiServiceRouter, arn: &str) -> Option<Value> {
    let (status, answer) = get_pipeline(router, arn).await;
    if status == 404 {
        let code = error_code(case, &answer);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
        );
        let expected =
            format!("The following required resource '{PIPELINE_KIND}' cannot be found: '{arn}'.");
        let message = error_message(case, &answer);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        return None;
    }
    let answer = assert_ok(case, status, answer);
    Some(
        answer
            .get("imagePipeline")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline in {answer}")),
    )
}

/// Assert the answer is the refusal `code` carrying `message`, with HTTP `expected`.
fn assert_refused(case: &str, status: u16, body: &Value, expected: u16, code: &str, message: &str) {
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected}, got {status} with {body}"
    );
    let actual_code = error_code(case, body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected {code}, got {actual_code:?} with {body}"
    );
    let actual_message = error_message(case, body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// The optional members a dry-run request carries beside the ones a create cannot omit: the
/// request the dry run validates is a WHOLE create, so the rows span the kinds of member one
/// carries — none at all, a schedule, both tag maps, a status, the test settings and the
/// metadata flag.
fn dry_run_rows() -> Vec<(&'static str, Value)> {
    vec![
        ("only the members a create cannot omit", json!({})),
        (
            "a schedule and a description",
            json!({
                "description": "validated, never created",
                "schedule": {"scheduleExpression": "cron(0 3 ? * SUN *)"},
            }),
        ),
        (
            "both tag maps",
            json!({
                "tags": {"Owner": "platform"},
                "imageTags": {"Environment": "dev"},
            }),
        ),
        ("a disabled pipeline", json!({"status": "DISABLED"})),
        (
            "the test settings and the metadata flag",
            json!({
                "imageTestsConfiguration": {"imageTestsEnabled": false, "timeoutMinutes": 90},
                "enhancedImageMetadataEnabled": false,
            }),
        ),
    ]
}

#[tokio::test]
async fn a_dry_run_validates_the_request_and_performs_nothing() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    for (index, (case, extra)) in dry_run_rows().into_iter().enumerate() {
        let name = format!("dry-run-pipeline-{index}");
        let token = format!("dry-run-token-{index}");
        let arn = pipeline_arn(&name);

        let (status, refused) =
            create_pipeline(&router, &create_body(&fixture, &name, &token, &extra, true)).await;
        assert_refused(
            case,
            status,
            &refused,
            412,
            "DryRunOperationException",
            DRY_RUN_MESSAGE,
        );
        for member in ["imagePipelineArn", "clientToken"] {
            assert!(
                refused.get(member).is_none(),
                "case {case:?}: expected no {member} on the dry run's answer, got {refused}"
            );
        }

        // Nothing was performed: the ARN the create would have minted resolves to no pipeline.
        let stored = stored_pipeline(case, &router, &arn).await;
        assert!(
            stored.is_none(),
            "case {case:?}: expected the dry run to store nothing under {arn}, got {stored:?}"
        );

        // And neither the name nor the token it named was spent: the SAME request, asking for
        // the create this time, performs it.
        let (status, created) =
            create_pipeline(&router, &create_body(&fixture, &name, &token, &extra, false)).await;
        let created = assert_ok(case, status, created);
        let created_arn = text(case, &created, "imagePipelineArn");
        assert_eq!(
            created_arn, arn,
            "case {case:?}: expected the create to mint {arn:?}, got {created_arn:?}"
        );
        let echoed = text(case, &created, "clientToken");
        assert_eq!(
            echoed, token,
            "case {case:?}: expected the request's own client token echoed, got {echoed:?}"
        );
        assert!(
            stored_pipeline(case, &router, &arn).await.is_some(),
            "case {case:?}: expected the create to store the pipeline under {arn}"
        );
    }
}

#[tokio::test]
async fn a_dry_run_whose_validation_fails_reports_that_failure_instead() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // "If validation SUCCEEDS, the operation returns a DryRunOperationException error response"
    // — so a dry run whose request would not have validated is answered by what the create
    // itself would have been answered with, and the rows span the two shapes that takes: a
    // reference naming nothing stored, and a schedule expression the service cannot read.
    let absent = "arn:aws:imagebuilder:us-west-2:000000000000:infrastructure-configuration/\
                  dry-run-nothing-stored";
    let case = "a dry run naming an infrastructure configuration nothing stored";
    let body = create_body(
        &fixture,
        "dry-run-missing-reference",
        "dry-run-missing-token",
        &json!({"infrastructureConfigurationArn": absent}),
        true,
    );
    let (status, refused) = create_pipeline(&router, &body).await;
    assert_refused(
        case,
        status,
        &refused,
        404,
        "ResourceNotFoundException",
        &format!("The following required resource 'InfrastructureConfiguration' cannot be found: '{absent}'."),
    );

    let case = "a dry run carrying a schedule expression the service cannot read";
    let body = create_body(
        &fixture,
        "dry-run-bad-schedule",
        "dry-run-schedule-token",
        &json!({"schedule": {"scheduleExpression": "cron(not valid)"}}),
        true,
    );
    let (status, refused) = create_pipeline(&router, &body).await;
    assert_refused(
        case,
        status,
        &refused,
        400,
        "InvalidParameterValueException",
        // The parser's own statement of what stopped it, in the one sentence this service
        // states every parameter refusal in — the sibling suite's wording, borrowed so the two
        // cannot drift.
        &refusal_message(&illegal_characters(0, "NOT")),
    );

    // Neither dry run performed anything either.
    for (case, name) in [
        (
            "the dry run of the missing reference",
            "dry-run-missing-reference",
        ),
        ("the dry run of the bad schedule", "dry-run-bad-schedule"),
    ] {
        let arn = pipeline_arn(name);
        let stored = stored_pipeline(case, &router, &arn).await;
        assert!(
            stored.is_none(),
            "case {case:?}: expected nothing stored under {arn}, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn a_retried_create_answers_the_original_response() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    let extra = json!({
        "description": "as created",
        "status": "ENABLED",
        "tags": {"Owner": "platform"},
        "schedule": {"scheduleExpression": "rate(1 day)"},
    });
    let case = "the create a retry repeats";
    let body = create_body(&fixture, "retried-create", "one-token", &extra, false);
    let (status, first) = create_pipeline(&router, &body).await;
    let first = assert_ok(case, status, first);
    let arn = text(case, &first, "imagePipelineArn");
    let created = stored_pipeline(case, &router, &arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the created pipeline under {arn}"));

    // Every retry of that request answers the ORIGINAL response, member for member — down to
    // its `requestId`, which is what tells the caller the operation did not run again.
    for case in ["the first retry", "the second retry"] {
        let (status, repeat) = create_pipeline(&router, &body).await;
        let repeat = assert_ok(case, status, repeat);
        assert_eq!(
            repeat, first,
            "case {case:?}: expected the original response {first}, got {repeat}"
        );
        let read = stored_pipeline(case, &router, &arn)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the pipeline still under {arn}"));
        assert_eq!(
            read, created,
            "case {case:?}: expected the retry to leave the pipeline as it was, got {read}"
        );
    }

    // A FRESH token names a request of its own, so it performs a second create — under its own
    // name, with a request id of its own.
    let case = "a fresh token";
    let (status, second) = create_pipeline(
        &router,
        &create_body(&fixture, "retried-create-2", "another-token", &extra, false),
    )
    .await;
    let second = assert_ok(case, status, second);
    assert_eq!(
        text(case, &second, "imagePipelineArn"),
        pipeline_arn("retried-create-2"),
        "case {case:?}: expected the second create's own ARN, got {second}"
    );
    assert_ne!(
        text(case, &second, "requestId"),
        text(case, &first, "requestId"),
        "case {case:?}: expected a request id of its own, got {second}"
    );
}

#[tokio::test]
async fn a_spent_token_carrying_different_parameters_is_a_mismatch() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // The request one token answered for, and the pipeline it created.
    let token = "spent-token";
    let name = "spent-token-pipeline";
    let original = json!({
        "description": "as created",
        "status": "ENABLED",
        "tags": {"Owner": "platform"},
        "schedule": {"scheduleExpression": "rate(1 day)"},
        "imageTestsConfiguration": {"imageTestsEnabled": true, "timeoutMinutes": 720},
        "enhancedImageMetadataEnabled": true,
    });
    let case = "the create the token answered for";
    let (status, first) = create_pipeline(
        &router,
        &create_body(&fixture, name, token, &original, false),
    )
    .await;
    let first = assert_ok(case, status, first);
    let arn = text(case, &first, "imagePipelineArn");
    let created = stored_pipeline(case, &router, &arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the created pipeline under {arn}"));

    // (case, the member the retry changed, the value it carried) — one row per creation
    // parameter the request describes the pipeline with, whatever its type.
    let rows: [(&str, &str, Value); 6] = [
        ("a changed description", "description", json!("as retried")),
        ("a changed status", "status", json!("DISABLED")),
        (
            "changed tags",
            "tags",
            json!({"Owner": "platform", "Environment": "dev"}),
        ),
        (
            "a changed schedule",
            "schedule",
            json!({"scheduleExpression": "rate(2 days)"}),
        ),
        (
            "changed test settings",
            "imageTestsConfiguration",
            json!({"imageTestsEnabled": false, "timeoutMinutes": 90}),
        ),
        (
            "a changed metadata flag",
            "enhancedImageMetadataEnabled",
            json!(false),
        ),
    ];

    for (case, member, value) in rows {
        let mut extra = match original.clone() {
            Value::Object(members) => members,
            other => panic!("expected an object of creation parameters, got {other}"),
        };
        extra.insert(member.to_string(), value);
        let (status, refused) = create_pipeline(
            &router,
            &create_body(&fixture, name, token, &Value::Object(extra), false),
        )
        .await;
        assert_refused(
            case,
            status,
            &refused,
            400,
            "IdempotentParameterMismatchException",
            MISMATCH_MESSAGE,
        );

        // The pipeline the token's own request created is untouched by the mismatch.
        let read = stored_pipeline(case, &router, &arn)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the pipeline still under {arn}"));
        assert_eq!(
            read, created,
            "case {case:?}: expected the refused retry to replace nothing, got {read}"
        );
    }

    // The NAME is a creation parameter too: the same token asking for a different name is the
    // same mismatch, and creates no pipeline under that other name.
    let case = "a changed name";
    let other = "spent-token-other-name";
    let (status, refused) = create_pipeline(
        &router,
        &create_body(&fixture, other, token, &original, false),
    )
    .await;
    assert_refused(
        case,
        status,
        &refused,
        400,
        "IdempotentParameterMismatchException",
        MISMATCH_MESSAGE,
    );
    let stored = stored_pipeline(case, &router, &pipeline_arn(other)).await;
    assert!(
        stored.is_none(),
        "case {case:?}: expected no pipeline under the other name, got {stored:?}"
    );
}

#[tokio::test]
async fn the_members_that_describe_the_call_are_not_members_of_the_pipeline() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // `clientToken` is echoed by the create's own response and `dryRun` describes the call, so
    // neither is a member of the resource a read reports.
    const NOT_PIPELINE_MEMBERS: &[&str] = &["clientToken", "dryRun", "requestId"];
    for (index, (case, extra)) in dry_run_rows().into_iter().enumerate() {
        let name = format!("call-members-pipeline-{index}");
        let token = format!("call-members-token-{index}");
        let (status, created) =
            create_pipeline(&router, &create_body(&fixture, &name, &token, &extra, false)).await;
        let created = assert_ok(case, status, created);

        // The create's own response carries exactly the three members the output models.
        let mut answered: Vec<&str> = match &created {
            Value::Object(members) => members.keys().map(String::as_str).collect(),
            other => panic!("case {case:?}: expected an object, got {other}"),
        };
        answered.sort_unstable();
        assert_eq!(
            answered,
            ["clientToken", "imagePipelineArn", "requestId"],
            "case {case:?}: expected the modelled output members, got {created}"
        );

        let arn = text(case, &created, "imagePipelineArn");
        let pipeline = stored_pipeline(case, &router, &arn)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the pipeline under {arn}"));
        for member in NOT_PIPELINE_MEMBERS {
            assert!(
                pipeline.get(*member).is_none(),
                "case {case:?}: expected no {member} on the stored pipeline, got {pipeline}"
            );
        }
        // What the pipeline does report is the resource the request described: its own name and
        // the ARN the name normalizes to.
        assert_eq!(
            pipeline.get("name"),
            Some(&json!(name)),
            "case {case:?}: expected the name the create carried, got {pipeline}"
        );
        assert_eq!(
            pipeline.get("arn"),
            Some(&json!(pipeline_arn(&name))),
            "case {case:?}: expected the ARN of {name}, got {pipeline}"
        );
    }
}
