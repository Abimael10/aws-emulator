// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::ApiGateway::RestApi` the owning service REFUSED reports on its
//! `CREATE_FAILED` event, end to end over the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its own SDK
//! raised — the owning service's sentence followed by that SDK's diagnostic of the call, in
//! the shape THAT provider's SDK words (the api's provider counts the attempts the SDK made)
//! — and classifies it under the `HandlerErrorCode` its mapping puts the answered error code
//! in: a code the owning service's model does not declare at all is an exception the provider
//! has no mapping for, so it is reported under the class for a service that FAILED rather than
//! as a request the provider found fault with. A refusal the provider makes ITSELF, before it
//! calls that service, carries no SDK diagnostic and is a refusal of the request.**
//!
//! Why it matters: tooling triages a rolled-back deployment by `HandlerErrorCode` — it decides
//! whether a failure is worth retrying and whether the template or the service is at fault —
//! and reads the owning service's own sentence out of the quoted message to report WHAT was
//! wrong. A reason that classifies a service exception as a client-input error sends that
//! triage down the wrong path, and one whose quoted message lists an enum's members in an
//! order the service never states breaks the reason matching a client writes against it.
//!
//! The rows differ along what the rule generalises over: WHICH member of the create the
//! declaration breaks (the api's key source and its endpoint type, two different enums, one
//! travelling as a body member and one as an element of a list), and WHO refused it (the
//! owning service's constraint layer, and the provider itself for a declaration it cannot
//! act on).
//!
//! `apigateway_refused_resource_sdk_diagnostic.rs` owns the same question for the api's
//! DEPLOYMENT, whose provider words the bare envelope and maps that service's own modelled
//! errors; this suite is about the api itself.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::harness::{failed_resource_reason, quoted_and_class, settled_stack_status, REGION};
use crate::stack_lifecycle::{create_stack, router, wait_for_the_transition};

/// The status a resource its provider could not create records, and the status the stack
/// settles in once the create that failed has been rolled back.
const CREATE_FAILED: &str = "CREATE_FAILED";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The SDK's own name for Amazon API Gateway, as the exception its SDK raises states it: that
/// model's `awsService.cloudFormationName`.
const API_GATEWAY_SERVICE: &str = "ApiGateway";

/// The `HandlerErrorCode` classes these rows fall in, both values of the model's own
/// `HandlerErrorCode` enum: the class a provider reports an exception it has no mapping for
/// under, and the class of a request the provider itself refused.
const GENERAL_SERVICE_EXCEPTION: &str = "GeneralServiceException";
const INVALID_REQUEST: &str = "InvalidRequest";

/// One row: the property document the api is declared with, and what its failed event must
/// report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Properties` of the declared `AWS::ApiGateway::RestApi`.
    properties: fn(&str) -> Value,
    /// A fragment of the owning service's OWN sentence the quoted message must carry, or
    /// `None` for a refusal the provider made itself.
    sentence: Option<&'static str>,
    /// The HTTP status the SDK diagnostic must state, for a row the owning service refused.
    status: Option<u16>,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

/// An api whose declared `ApiKeySourceType` is outside the value set that member publishes:
/// the owning service's constraint layer refuses the create, naming the member and listing
/// the set in the order that service states it.
fn off_enum_key_source(stack: &str) -> Value {
    json!({ "Name": format!("{stack}-api"), "ApiKeySourceType": "BOGUS" })
}

/// The same class of refusal at another member, and at an ELEMENT of a list rather than at a
/// body member: an endpoint type outside `EndpointType`'s own value set.
fn off_enum_endpoint_type(stack: &str) -> Value {
    json!({
        "Name": format!("{stack}-api"),
        "EndpointConfiguration": { "Types": ["SATELLITE"] },
    })
}

/// A declaration the provider refuses ITSELF: the api's definition is held in Amazon S3, which
/// is a document this emulator cannot read, so nothing is asked of the owning service at all.
fn definition_in_s3(stack: &str) -> Value {
    json!({
        "Name": format!("{stack}-api"),
        "BodyS3Location": { "Bucket": "definitions", "Key": "openapi.json" },
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "an api key source outside the value set that member publishes",
        stack: "apigw-restapi-reason-key-source",
        properties: off_enum_key_source,
        sentence: Some("at 'createRestApiInput.apiKeySource'"),
        status: Some(400),
        class: GENERAL_SERVICE_EXCEPTION,
    },
    Row {
        case: "an endpoint type outside the value set its element publishes",
        stack: "apigw-restapi-reason-endpoint-type",
        properties: off_enum_endpoint_type,
        sentence: Some("at 'createRestApiInput.endpointConfiguration.types'"),
        status: Some(400),
        class: GENERAL_SERVICE_EXCEPTION,
    },
    Row {
        case: "a definition the provider itself cannot read",
        stack: "apigw-restapi-reason-s3-definition",
        properties: definition_in_s3,
        sentence: None,
        status: None,
        class: INVALID_REQUEST,
    },
];

/// The value set the api's key source publishes, in the order that service STATES it — which
/// is not the order the public model declares the enum in. Source: the recorded
/// `ResourceStatusReason` of an `AWS::ApiGateway::RestApi` whose declared `ApiKeySourceType`
/// is outside the set.
const API_KEY_SOURCE_SET: &str = "[AUTHORIZER, HEADER]";

/// The value set an endpoint type publishes, in the order that service states it.
const ENDPOINT_TYPE_SET: &str = "[PRIVATE, EDGE, REGIONAL]";

/// The template one row deploys: the declared api, and nothing else.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": (row.properties)(row.stack),
            },
        },
    })
    .to_string()
}

/// Hold the quoted message of a row the OWNING SERVICE refused to the shape that provider's
/// SDK words: the service's own sentence, the service as its SDK names it, the status it
/// answered, the id of the call — and the number of attempts the SDK made at it, which is
/// what this provider's shape carries past the envelope.
///
/// The envelope itself is the harness's ([`crate::harness::assert_sdk_diagnostic`]), which
/// holds a diagnostic that closes AT the call's id; this suite's provider words one more
/// field, so the count is split off first and the envelope ahead of it is held to that same
/// reader.
fn assert_counted_sdk_diagnostic(case: &str, quoted: &str, status: u16) {
    const COUNT: &str = " (SDK Attempt Count: ";
    let (envelope, count) = quoted.rsplit_once(COUNT).unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK's attempt count past the envelope, got {quoted:?}")
    });
    let count = count.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the attempt count to close, got {quoted:?}")
    });
    assert!(
        count.parse::<u32>().is_ok_and(|attempts| attempts >= 1),
        "case {case:?}: expected the attempts the SDK made at the call, got {count:?}"
    );
    crate::harness::assert_sdk_diagnostic(case, envelope, API_GATEWAY_SERVICE, status);
}

/// Whether API Gateway holds an api NAMED `name`: a page of that service's own collection read,
/// which reports no `item` member at all for a page that holds none.
async fn api_exists(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, page) = apigateway(router, "GET", "/restapis").await;
    assert_eq!(
        status, 200,
        "case {name:?}: GetRestApis must answer 200, got {status} {page}"
    );
    page.get("item")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|api| api.get("name").and_then(Value::as_str) == Some(name))
        })
}

#[tokio::test]
async fn a_refused_rest_api_reports_the_exception_its_provider_raised() {
    for row in ROWS {
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {created}",
            row.case
        );
        // The create's own terminal status, and the rollback's: both are on the stack's clock.
        wait_for_the_transition().await;
        wait_for_the_transition().await;

        let reason =
            failed_resource_reason(row.case, &router, REGION, row.stack, "Api", CREATE_FAILED)
                .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        assert_eq!(
            class, row.class,
            "case {:?}: expected the {} class, got {class:?} in {reason:?}",
            row.case, row.class
        );
        match (row.sentence, row.status) {
            (Some(sentence), Some(status)) => {
                assert!(
                    quoted.contains(sentence),
                    "case {:?}: expected the owning service's own sentence carrying \
                     {sentence:?}, got {quoted:?}",
                    row.case
                );
                assert_counted_sdk_diagnostic(row.case, &quoted, status);
            }
            _ => assert!(
                !quoted.contains("(Service: "),
                "case {:?}: expected the provider's own refusal with no SDK diagnostic, got \
                 {quoted:?}",
                row.case
            ),
        }

        // The stack rolled the failed create back, and the api the declaration asked for
        // exists in the owning service nowhere: a refused create leaves nothing behind.
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {:?}: expected the failed create to roll back, got {settled:?}",
            row.case
        );
        let name = format!("{}-api", row.stack);
        assert!(
            !api_exists(&router, &name).await,
            "case {:?}: expected API Gateway to hold no api named {name:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_refused_rest_api_lists_the_value_set_as_the_service_states_it() {
    // The two enums above, each quoted in the order the owning service lists its values: the
    // order is a fact about the enum, not about one request, so both are asserted whole.
    let rows: &[(&str, &Row, &str)] = &[
        ("the api key source", &ROWS[0], API_KEY_SOURCE_SET),
        ("an endpoint type", &ROWS[1], ENDPOINT_TYPE_SET),
    ];
    for (case, row, set) in rows {
        let router = router();
        let stack = format!("{}-set", row.stack);
        let (status_code, created) = create_stack(&router, &stack, &template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
        );
        wait_for_the_transition().await;
        wait_for_the_transition().await;
        let reason =
            failed_resource_reason(case, &router, REGION, &stack, "Api", CREATE_FAILED).await;
        let clause = format!("Member must satisfy enum value set: {set}");
        assert!(
            reason.contains(&clause),
            "case {case:?}: expected the quoted message to state {clause:?}, got {reason:?}"
        );
    }
}
