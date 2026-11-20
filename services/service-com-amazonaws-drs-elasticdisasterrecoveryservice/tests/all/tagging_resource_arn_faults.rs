// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for which of its two refusals the DRS tagging route
//! (`/tags/{resourceArn}`, by method) answers a `resourceArn` it cannot serve with.
//!
//! Every test drives the service the way the router does — one buffered `http::Request`
//! per call through [`MultiServiceRouter::dispatch`] — so what is asserted is the wire
//! behaviour (status, error code, JSON body). The harness and the per-operation requests
//! are the ones `resource_tags` builds; this suite is about the ARN, so its rows are ARNs
//! rather than tag maps.
//!
//! The rule under test, asserted over a TABLE of ARNs rather than one recorded request:
//!
//! 1. The route reads the resource type out of the ARN's resource part, and only the
//!    resource types it addresses can be named there. An ARN it does NOT address — one
//!    naming another service's resource, one naming a resource type of this service the
//!    route does not address, and one whose resource part is not `<type>/<id>` at all — is
//!    a fault in the value the request supplied: HTTP 400, the error code
//!    `BadRequestException` and the message `Invalid input resource arn`, answered BEFORE
//!    any resource is looked up, so whether such a resource exists makes no difference.
//! 2. An ARN of a resource type the route DOES address, naming no resource this service
//!    holds, is the other class: HTTP 404, the error code `NotFoundException` and the
//!    message `Resource was not found`. So the class is decided by the resource type the
//!    ARN names, not by whether a resource happens to be there.
//! 3. Every operation of the route — the read (`GET`), the write (`POST`) and the removal
//!    (`DELETE`) — answers the same ARN the same way, and neither refusal changes
//!    anything: a resource whose ARN the route refuses keeps reporting the tags it
//!    carries through its OWN reader.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{
    call, drs_with_engine, items, Answer, REGION,
};
use crate::resource_tags::{
    launch_template_with_tags, list_tags, plan_with_tags, reported_tags, tag_resource, text,
    untag_resource,
};

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The error code and the whole message the route answers a `resourceArn` it does not
/// address with — written out as the literals a client reads rather than taken from the
/// implementation's constants.
const BAD_REQUEST_CODE: &str = "BadRequestException";
const INVALID_RESOURCE_ARN: &str = "Invalid input resource arn";

/// The other class: the code and message for an ARN of a type the route addresses that
/// resolves to no resource.
const NOT_FOUND_CODE: &str = "NotFoundException";
const NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// One refusal of the route, as a client sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refusal {
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// The refusal of an ARN the route does not address.
const NOT_ADDRESSED: Refusal = Refusal {
    status: 400,
    code: BAD_REQUEST_CODE,
    message: INVALID_RESOURCE_ARN,
};

/// The refusal of an ARN of an addressed type that names no held resource.
const NO_SUCH_RESOURCE: Refusal = Refusal {
    status: 404,
    code: NOT_FOUND_CODE,
    message: NOT_FOUND_MESSAGE,
};

/// Assert that every operation of the route answers `arn` with `expected`, and that none
/// of them reports a tag set.
async fn assert_every_operation_refuses(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    expected: Refusal,
) {
    let answers = [
        ("ListTagsForResource", list_tags(router, REGION, arn).await),
        (
            "TagResource",
            tag_resource(router, REGION, arn, &json!({"env": "prod"})).await,
        ),
        (
            "UntagResource",
            untag_resource(router, REGION, arn, &["env"]).await,
        ),
    ];
    for (operation, answer) in &answers {
        let case = format!("{operation} on {case}");
        assert_eq!(
            answer.status, expected.status,
            "case {case:?}: expected {} for {arn}, got {answer:?}",
            expected.status
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some(expected.code),
            "case {case:?}: expected {}, got {answer:?}",
            expected.code
        );
        assert_eq!(
            text(answer, "message"),
            expected.message,
            "case {case:?}: expected the message {:?}, got {answer:?}",
            expected.message
        );
        assert!(
            reported_tags(answer).is_none(),
            "case {case:?}: a refusal must report no tags, got {answer:?}"
        );
    }
}

/// The tags `DescribeLaunchConfigurationTemplates` reports for the template `id`, which is
/// the launch configuration template's OWN reader — the one place its tags are readable,
/// because the tagging route does not address that resource type.
async fn launch_template_tags(router: &MultiServiceRouter, id: &str) -> Value {
    let answer: Answer = call(
        router,
        "DescribeLaunchConfigurationTemplates",
        &json!({"launchConfigurationTemplateIDs": [id]}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "describing {id}: expected 200, got {answer:?}"
    );
    items(&answer)
        .first()
        .and_then(|item| item.get("tags"))
        .cloned()
        .unwrap_or_else(|| panic!("expected {id} to be reported with its tags, got {answer:?}"))
}

/// An ARN the route does not address is refused as a bad request by every operation of the
/// route alike, whatever the reason it is not addressed — and whether or not the resource
/// it names exists.
#[tokio::test]
async fn an_arn_the_route_does_not_address_is_a_bad_request() {
    let (router, _engine) = drs_with_engine();

    // A launch configuration template this service HOLDS, carrying tags: the route still
    // does not address that resource type, so its ARN is refused exactly like one nothing
    // minted. That is what makes this a fault in the value rather than a lookup miss.
    let held_tags = json!({"env": "prod"});
    let held_launch_template = launch_template_with_tags(&router, &held_tags).await;
    let held_id = held_launch_template
        .rsplit_once('/')
        .expect("a launch-configuration-template ARN")
        .1
        .to_string();

    // Every ARN below satisfies the modelled `ARN` (length 20..2048, pattern
    // `^arn:.{16,2044}$`), so each reaches the handler rather than the boundary validator.
    let rows: Vec<(&str, String)> = vec![
        (
            "a launch configuration template this service holds",
            held_launch_template.clone(),
        ),
        (
            "a launch configuration template id nothing minted",
            format!(
                "arn:aws:drs:{REGION}:{ACCOUNT}:launch-configuration-template/\
                 lct-00000000000000000"
            ),
        ),
        (
            "another service's resource",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-0123456789abcdef0"),
        ),
        (
            "another service's resource whose resource part looks like one of this \
             service's",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:source-server/s-00000000000000001"),
        ),
        (
            "a resource part naming no resource type at all",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:not-a-drs-type/x-00000000000000001"),
        ),
        (
            "a resource part carrying no id",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/"),
        ),
        (
            "a resource part nesting a further id",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/s-00000000000000001/disk-0"),
        ),
        (
            "a value that is not laid out as an ARN at all",
            "arn:not-an-arn-but-long-enough-to-pass-the-boundary".to_string(),
        ),
    ];

    for (case, arn) in &rows {
        assert_every_operation_refuses(&router, case, arn, NOT_ADDRESSED).await;
    }

    // Nothing any of those refusals addressed was written: the held template still
    // reports the tag set it was created with, through its own reader.
    assert_eq!(
        launch_template_tags(&router, &held_id).await,
        held_tags,
        "the held template must still carry {held_tags}"
    );
}

/// An ARN of a resource type the route DOES address, naming no resource this service
/// holds, is the route's own not-found instead — so the two classes are told apart by the
/// resource type the ARN names, not by whether the resource is there.
#[tokio::test]
async fn an_addressed_type_with_no_such_resource_is_not_found() {
    let (router, _engine) = drs_with_engine();
    let tags = json!({"env": "prod"});
    let plan = plan_with_tags(&router, REGION, Some(&tags)).await;
    let plan_id = plan.rsplit_once('/').expect("a recovery-plan ARN").1.to_string();

    let rows: Vec<(&str, String)> = vec![
        (
            "a recovery plan id nothing minted",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-plan/plan-00000000000000000"),
        ),
        (
            "a stored plan's id under another account",
            format!("arn:aws:drs:{REGION}:123456789012:recovery-plan/{plan_id}"),
        ),
        (
            "a stored plan's id under another partition",
            format!("arn:aws-us-gov:drs:{REGION}:{ACCOUNT}:recovery-plan/{plan_id}"),
        ),
        (
            "a replication configuration template id nothing minted",
            format!(
                "arn:aws:drs:{REGION}:{ACCOUNT}:replication-configuration-template/\
                 rct-00000000000000000"
            ),
        ),
        (
            "a source server nothing registered",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/s-00000000000000009"),
        ),
        (
            "a job, a resource type this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:job/drsjob-00000000000000000"),
        ),
        (
            "a recovery instance, another type this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-instance/i-0123456789abcdef0"),
        ),
        (
            "a source network, another type this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-network/sn-00000000000000000"),
        ),
    ];

    for (case, arn) in &rows {
        assert_every_operation_refuses(&router, case, arn, NO_SUCH_RESOURCE).await;
    }

    // None of those refusals touched the resource that IS held.
    let read = list_tags(&router, REGION, &plan).await;
    assert_eq!(
        (read.status, reported_tags(&read)),
        (200, Some(&tags)),
        "the stored plan's tags must survive every refusal, got {read:?}"
    );
}
