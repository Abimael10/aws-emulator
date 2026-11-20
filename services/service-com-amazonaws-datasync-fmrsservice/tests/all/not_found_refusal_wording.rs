// SPDX-License-Identifier: CC0-1.0

//! How this service identifies a resource it could not resolve.
//!
//! RULE: every client failure of this service is the one modelled `InvalidRequestException`
//! (400), so the CLASS of a refusal travels in that error's two code members — `errorCode`
//! and `datasyncErrorCode`, always the same value — and a resource this service holds no
//! record of has its own class per resource kind: `LocationNotFound` for a transfer
//! location, `SyncSetNotFound` for a transfer task and `AgentNotFound` for an agent. The
//! message identifies the resource the
//! way the SURFACE that raised the refusal addresses one: an operation of the task surface
//! by the bare resource id, an operation of the location surface by the ARN the request
//! carried, an operation of the agent surface by the bare `agent-…` id and the two reasons
//! it resolves to nothing. None of them names the request member that held the value.
//!
//! A refusal is about STATE, not about the shape of an ARN: a resource this service minted
//! resolves, and the same ARN stops resolving once the resource is deleted.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for.
const SIGNED_REGION: &str = "us-west-2";

/// The class a refusal naming a transfer location this account does not hold carries.
const LOCATION_NOT_FOUND: &str = "LocationNotFound";

/// The class a refusal naming a transfer task this account does not hold carries. It is the
/// name the surface holding tasks has for one, not the name the caller used.
const SYNC_SET_NOT_FOUND: &str = "SyncSetNotFound";

/// The class a refusal naming an agent this account does not hold carries.
const AGENT_NOT_FOUND: &str = "AgentNotFound";

/// A router with just this service mounted on a fresh engine.
fn service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_datasync_fmrsservice::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call, returning the HTTP status and the parsed response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("FmrsService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDDATASYNCTEST/20260101/{SIGNED_REGION}\
                 /datasync/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// A call that must succeed, returning its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A call that must be refused with the whole envelope a not-found class carries: HTTP 400,
/// the one modelled client error, that class in BOTH code members, and the whole message.
async fn refused_as_absent(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    class: &str,
    message: &str,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} with {parsed}"
    );
    assert!(
        parsed["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("InvalidRequestException"),
        "case {case:?}: expected an InvalidRequestException from {operation}, got {parsed}"
    );
    for member in ["errorCode", "datasyncErrorCode"] {
        assert_eq!(
            parsed[member].as_str(),
            Some(class),
            "case {case:?}: expected {member} {class:?} from {operation}, got {parsed}"
        );
    }
    assert_eq!(
        parsed["message"].as_str(),
        Some(message),
        "case {case:?}: wrong refusal wording from {operation}, got {parsed}"
    );
}

/// A well-formed ARN of one of this service's resources that this service never minted:
/// `resource` is the ARN's resource part.
fn never_minted(resource: &str) -> String {
    format!(
        "arn:aws:datasync:{SIGNED_REGION}:{account}:{resource}",
        account = engine::emulator_account_id()
    )
}

/// The bare resource id an ARN addresses: what a refusal raised by the task surface
/// identifies a resource by.
fn resource_id(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// An Amazon S3 transfer location for `bucket`, created and returned by ARN.
async fn given_location(router: &MultiServiceRouter, bucket: &str, case: &str) -> String {
    let created = ok(
        router,
        "CreateLocationS3",
        json!({
            "S3BucketArn": format!("arn:aws:s3:::{bucket}"),
            "S3Config": {
                "BucketAccessRoleArn": format!(
                    "arn:aws:iam::{account}:role/datasync-bucket-access",
                    account = engine::emulator_account_id()
                ),
            },
        }),
        case,
    )
    .await;
    created["LocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a LocationArn, got {created}"))
        .to_string()
}

/// RULE: an operation of the TASK surface that cannot resolve the task a request names
/// refuses with the `SyncSetNotFound` class and identifies the task by its bare resource id.
///
/// The rows differ along what the rule generalises over: which operation of that surface was
/// called — a read, a delete, a start and a list — and which unheld task id it named.
#[tokio::test]
async fn the_task_surface_names_an_absent_task_by_its_resource_id() {
    let router = service();
    let absent = never_minted("task/task-0abcdef1234567890");
    let other_absent = never_minted("task/task-1234567890abcdef0");

    // (case, operation, the member carrying the task ARN, the ARN it carried)
    let rows: [(&str, &str, &str, &String); 5] = [
        ("a read", "DescribeTask", "TaskArn", &absent),
        ("a delete", "DeleteTask", "TaskArn", &absent),
        ("a start", "StartTaskExecution", "TaskArn", &absent),
        ("a list of its runs", "ListTaskExecutions", "TaskArn", &absent),
        ("another unheld task", "DescribeTask", "TaskArn", &other_absent),
    ];

    for (case, operation, member, arn) in rows {
        refused_as_absent(
            &router,
            operation,
            json!({ member: arn }),
            SYNC_SET_NOT_FOUND,
            &format!("Task {} is not found.", resource_id(arn)),
            case,
        )
        .await;
    }
}

/// RULE: a task's `LocationArn` members are references, so the TASK surface states a
/// location it cannot resolve by the bare resource id too — and names neither which member
/// carried it nor the ARN itself.
///
/// The rows differ along which member carried the unheld location and whether the other
/// member names one this account holds, so one refusal covers both sides of a task.
#[tokio::test]
async fn the_task_surface_names_an_absent_referenced_location_by_its_resource_id() {
    let router = service();
    let held = given_location(&router, "reference-held", "setup").await;
    let absent = never_minted("location/loc-0abcdef1234567890");
    let other_absent = never_minted("location/loc-1234567890abcdef0");

    // (case, the source the task names, the destination it names, the unheld location)
    let rows: [(&str, &String, &String, &String); 4] = [
        ("an unheld source", &absent, &held, &absent),
        ("an unheld destination", &held, &absent, &absent),
        ("two unheld locations", &absent, &other_absent, &absent),
        (
            "another unheld source",
            &other_absent,
            &held,
            &other_absent,
        ),
    ];

    for (case, source, destination, unheld) in rows {
        refused_as_absent(
            &router,
            "CreateTask",
            json!({
                "SourceLocationArn": source,
                "DestinationLocationArn": destination,
                "Name": case,
            }),
            LOCATION_NOT_FOUND,
            &format!("Location {} is not found.", resource_id(unheld)),
            case,
        )
        .await;
    }
}

/// RULE: an operation of the LOCATION surface — one whose own addressed resource is the
/// location — refuses an unheld location with the same class and identifies it by the ARN the
/// request carried.
///
/// The rows differ along which operation of that surface was called and which unheld
/// location it named.
#[tokio::test]
async fn the_location_surface_names_an_absent_location_by_the_arn_it_was_given() {
    let router = service();
    let absent = never_minted("location/loc-0abcdef1234567890");
    let other_absent = never_minted("location/loc-1234567890abcdef0");

    // (case, operation, the ARN the request carried)
    let rows: [(&str, &str, &String); 4] = [
        ("a read of an S3 location", "DescribeLocationS3", &absent),
        (
            "a read of an object storage location",
            "DescribeLocationObjectStorage",
            &absent,
        ),
        ("a delete", "DeleteLocation", &absent),
        (
            "another unheld location",
            "DescribeLocationS3",
            &other_absent,
        ),
    ];

    for (case, operation, arn) in rows {
        refused_as_absent(
            &router,
            operation,
            json!({ "LocationArn": arn }),
            LOCATION_NOT_FOUND,
            &format!("Location {arn} is not found."),
            case,
        )
        .await;
    }
}

/// RULE: the refusal reports STATE: a task and a location this service minted resolve, and
/// the same ARNs stop resolving — each under its own class and its own wording — once the
/// resources are deleted.
#[tokio::test]
async fn a_resource_resolves_until_it_is_deleted() {
    let router = service();
    let source = given_location(&router, "lifecycle-source", "setup").await;
    let destination = given_location(&router, "lifecycle-destination", "setup").await;
    let created = ok(
        &router,
        "CreateTask",
        json!({
            "SourceLocationArn": source,
            "DestinationLocationArn": destination,
            "Name": "lifecycle-task",
        }),
        "setup",
    )
    .await;
    let task_arn = created["TaskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"setup\": expected a TaskArn, got {created}"))
        .to_string();

    // Both resources resolve while this service holds them.
    let described = ok(
        &router,
        "DescribeTask",
        json!({ "TaskArn": &task_arn }),
        "the task it minted",
    )
    .await;
    assert_eq!(
        described["TaskArn"].as_str(),
        Some(task_arn.as_str()),
        "case \"the task it minted\": expected the task to read back, got {described}"
    );
    let described = ok(
        &router,
        "DescribeLocationS3",
        json!({ "LocationArn": &source }),
        "the location it minted",
    )
    .await;
    assert_eq!(
        described["LocationArn"].as_str(),
        Some(source.as_str()),
        "case \"the location it minted\": expected the location to read back, got {described}"
    );

    // Deleting them is what makes the two refusals due, each in its surface's own words.
    ok(
        &router,
        "DeleteTask",
        json!({ "TaskArn": &task_arn }),
        "deleting the task",
    )
    .await;
    refused_as_absent(
        &router,
        "DescribeTask",
        json!({ "TaskArn": &task_arn }),
        SYNC_SET_NOT_FOUND,
        &format!("Task {} is not found.", resource_id(&task_arn)),
        "the deleted task",
    )
    .await;

    ok(
        &router,
        "DeleteLocation",
        json!({ "LocationArn": &source }),
        "deleting the location",
    )
    .await;
    refused_as_absent(
        &router,
        "DescribeLocationS3",
        json!({ "LocationArn": &source }),
        LOCATION_NOT_FOUND,
        &format!("Location {source} is not found."),
        "the deleted location",
    )
    .await;
}

/// A call that must be refused as naming an agent this account does not hold: HTTP 400,
/// the one modelled client error, the agent surface's whole message, and the unknown-agent
/// class in BOTH code members.
async fn refused_as_absent_agent(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    agent_arn: &str,
    case: &str,
) {
    refused_as_absent(
        router,
        operation,
        body,
        AGENT_NOT_FOUND,
        &format!(
            "{agent} does not exist or has been deleted.",
            agent = resource_id(agent_arn)
        ),
        case,
    )
    .await;
}

/// An agent activated through `CreateAgent`, returned by ARN.
async fn given_agent(router: &MultiServiceRouter, activation_key: &str, case: &str) -> String {
    let created = ok(
        router,
        "CreateAgent",
        json!({ "ActivationKey": activation_key, "AgentName": case }),
        case,
    )
    .await;
    created["AgentArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an AgentArn, got {created}"))
        .to_string()
}

/// RULE: an operation of the AGENT surface — one whose own addressed resource is the
/// agent — refuses an `AgentArn` this account holds no agent for with the modelled client
/// error (400) and the `AgentNotFound` class in both of that error's code members,
/// identifying the agent by the bare `agent-…` id the request carried and stating the two
/// reasons it resolves to nothing.
///
/// The rows differ along which operation of that surface was called — a read, a rename and
/// a delete — and which unheld agent id it named, including one of another Region.
#[tokio::test]
async fn the_agent_surface_refuses_an_agent_it_never_activated() {
    let router = service();
    let absent = never_minted("agent/agent-0abcdef1234567890");
    let other_absent = never_minted("agent/agent-1234567890abcdef0");
    let other_region =
        format!("arn:aws:datasync:eu-central-1:{account}:agent/agent-abcdef01234567890", account = engine::emulator_account_id());

    // (case, operation, the extra members it carries, the ARN it named)
    let rows: [(&str, &str, Value, &String); 5] = [
        ("a read", "DescribeAgent", json!({}), &absent),
        ("a rename", "UpdateAgent", json!({ "Name": "renamed" }), &absent),
        ("a delete", "DeleteAgent", json!({}), &absent),
        ("another unheld agent", "DescribeAgent", json!({}), &other_absent),
        (
            "an unheld agent of another Region",
            "DescribeAgent",
            json!({}),
            &other_region,
        ),
    ];

    for (case, operation, extra, arn) in rows {
        let mut body = json!({ "AgentArn": arn });
        if let Some(members) = extra.as_object() {
            for (member, value) in members {
                body[member] = value.clone();
            }
        }
        refused_as_absent_agent(&router, operation, body, arn, case).await;
    }
}

/// RULE: the agent refusal reports STATE: an agent this service activated resolves and
/// reads back as what the activation stored, the rename it was given shows up on the next
/// read, and the SAME ARN stops resolving — in the same words an ARN that never named an
/// agent earns — once the delete removes it.
///
/// The two activations differ along what the request states: one names only its activation
/// key, the other a name and the VPC service endpoint members, which is what decides the
/// agent's `EndpointType` and whether it reports a `PrivateLinkConfig`.
#[tokio::test]
async fn an_activated_agent_resolves_until_it_is_deleted() {
    let router = service();

    // (case, the activation request, the EndpointType it earns, whether it reports a
    // PrivateLinkConfig)
    let rows: [(&str, Value, &str, bool); 2] = [
        (
            "a public activation",
            json!({ "ActivationKey": "AAAAA-BBBBB-CCCCC-DDDDD-EEEEE", "AgentName": "first" }),
            "PUBLIC",
            false,
        ),
        (
            "an activation over a VPC service endpoint",
            json!({
                "ActivationKey": "FFFFF-GGGGG-HHHHH-IIIII-JJJJJ",
                "AgentName": "second",
                "VpcEndpointId": "vpce-0123456789abcdef0",
                "SubnetArns": ["arn:aws:ec2:us-west-2:000000000000:subnet/subnet-01234567"],
                "SecurityGroupArns": [
                    "arn:aws:ec2:us-west-2:000000000000:security-group/sg-01234567",
                ],
            }),
            "PRIVATE_LINK",
            true,
        ),
    ];

    for (case, activation, endpoint_type, private_link) in rows {
        let name = activation["AgentName"].as_str().unwrap_or_default().to_string();
        let created = ok(&router, "CreateAgent", activation.clone(), case).await;
        let agent_arn = created["AgentArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an AgentArn, got {created}"))
            .to_string();

        let described = ok(
            &router,
            "DescribeAgent",
            json!({ "AgentArn": &agent_arn }),
            case,
        )
        .await;
        assert_eq!(
            described["AgentArn"].as_str(),
            Some(agent_arn.as_str()),
            "case {case:?}: expected the agent to read back under its own ARN, got {described}"
        );
        assert_eq!(
            described["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the stored name {name:?}, got {described}"
        );
        assert_eq!(
            described["Status"].as_str(),
            Some("ONLINE"),
            "case {case:?}: an activated agent is ONLINE, got {described}"
        );
        assert_eq!(
            described["EndpointType"].as_str(),
            Some(endpoint_type),
            "case {case:?}: expected EndpointType {endpoint_type:?}, got {described}"
        );
        assert_eq!(
            described["PrivateLinkConfig"]["VpcEndpointId"].as_str(),
            activation["VpcEndpointId"].as_str(),
            "case {case:?}: expected the activation's VPC endpoint reported back, got \
             {described}"
        );
        assert_eq!(
            described["PrivateLinkConfig"].is_object(),
            private_link,
            "case {case:?}: expected a PrivateLinkConfig reported = {private_link}, got \
             {described}"
        );
        assert!(
            described["CreationTime"].is_number() && described["LastConnectionTime"].is_number(),
            "case {case:?}: an activated agent reports when it was activated and last \
             heard from, got {described}"
        );

        // A rename is what the next read reports, and it resolves the same ARN.
        ok(
            &router,
            "UpdateAgent",
            json!({ "AgentArn": &agent_arn, "Name": "renamed" }),
            case,
        )
        .await;
        let renamed = ok(
            &router,
            "DescribeAgent",
            json!({ "AgentArn": &agent_arn }),
            case,
        )
        .await;
        assert_eq!(
            renamed["Name"].as_str(),
            Some("renamed"),
            "case {case:?}: expected the rename to be what the read reports, got {renamed}"
        );

        // Deleting the agent is what makes the refusal due, in the agent surface's one
        // wording — and a second delete of the same ARN is refused the same way.
        ok(
            &router,
            "DeleteAgent",
            json!({ "AgentArn": &agent_arn }),
            case,
        )
        .await;
        for operation in ["DescribeAgent", "DeleteAgent"] {
            refused_as_absent_agent(
                &router,
                operation,
                json!({ "AgentArn": &agent_arn }),
                &agent_arn,
                case,
            )
            .await;
        }
    }
}

/// RULE: agents are a store of their own, so one activation's agent is not another's: each
/// ARN resolves to the agent it names, and deleting one leaves the other resolving.
#[tokio::test]
async fn each_activated_agent_resolves_to_its_own_record() {
    let router = service();
    let first = given_agent(&router, "AAAAA-BBBBB-CCCCC-DDDDD-EEEEE", "first").await;
    let second = given_agent(&router, "KKKKK-LLLLL-MMMMM-NNNNN-OOOOO", "second").await;
    assert_ne!(
        first, second,
        "case \"two activations\": each activation mints its own agent, got {first} twice"
    );

    ok(&router, "DeleteAgent", json!({ "AgentArn": &first }), "first").await;
    refused_as_absent_agent(
        &router,
        "DescribeAgent",
        json!({ "AgentArn": &first }),
        &first,
        "the deleted agent",
    )
    .await;
    let described = ok(
        &router,
        "DescribeAgent",
        json!({ "AgentArn": &second }),
        "the agent that stayed",
    )
    .await;
    assert_eq!(
        described["AgentArn"].as_str(),
        Some(second.as_str()),
        "case \"the agent that stayed\": deleting one agent leaves the other resolving, \
         got {described}"
    );
}

/// RULE: the class token of a refusal is the error CLASS, not a value read off the request,
/// so the unknown-agent class reports the same fixed literal in `errorCode` and in
/// `datasyncErrorCode` whatever ARN the request carried — while the message's only variable
/// part is the `agent-…` id parsed off that ARN.
///
/// The rows differ along what the rule generalises over: which operation of the agent
/// surface was called, and the partition, Region, account and agent-id characters of the
/// unheld ARN it named. Every ARN matches the modelled `AgentArn` pattern (member `AgentArn`
/// of `DescribeAgentRequest`: `^arn:(aws|aws-cn|aws-us-gov|aws-eusc|aws-iso|aws-iso-b)
/// :datasync:[a-z\-0-9]+:[0-9]{12}:agent/agent-[0-9a-z]{17}$`), so each is refused for its
/// STATE and not for its shape.
#[tokio::test]
async fn the_unknown_agent_class_is_one_token_in_both_code_members() {
    let router = service();

    // (case, operation, the extra members the operation carries, the unheld ARN it named)
    let rows: [(&str, &str, Value, String); 7] = [
        (
            "a read, digits only",
            "DescribeAgent",
            json!({}),
            never_minted("agent/agent-0123456789abcdefg"),
        ),
        (
            "a rename, letters only",
            "UpdateAgent",
            json!({ "Name": "renamed" }),
            never_minted("agent/agent-abcdefghijklmnopq"),
        ),
        (
            "a delete, mixed id",
            "DeleteAgent",
            json!({}),
            never_minted("agent/agent-0a1b2c3d4e5f6g7h8"),
        ),
        (
            "a read in another Region",
            "DescribeAgent",
            json!({}),
            format!(
                "arn:aws:datasync:eu-west-3:{account}:agent/agent-9z8y7x6w5v4u3t2s1",
                account = engine::emulator_account_id()
            ),
        ),
        (
            "a read in the China partition",
            "DescribeAgent",
            json!({}),
            format!(
                "arn:aws-cn:datasync:cn-north-1:{account}:agent/agent-cn0123456789abcde",
                account = engine::emulator_account_id()
            ),
        ),
        (
            "a read in the GovCloud partition",
            "DescribeAgent",
            json!({}),
            "arn:aws-us-gov:datasync:us-gov-west-1:123456789012:agent/agent-gov0123456789abcd"
                .to_string(),
        ),
        (
            "a read for another account",
            "DescribeAgent",
            json!({}),
            "arn:aws:datasync:us-west-2:210987654321:agent/agent-zyxwvutsrqponmlkj".to_string(),
        ),
    ];

    for (case, operation, extra, arn) in rows {
        let mut body = json!({ "AgentArn": &arn });
        if let Some(members) = extra.as_object() {
            for (member, value) in members {
                body[member] = value.clone();
            }
        }
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {operation} to answer 400, got {status} with {parsed}"
        );
        assert_eq!(
            parsed["errorCode"].as_str(),
            Some(AGENT_NOT_FOUND),
            "case {case:?}: expected errorCode {AGENT_NOT_FOUND:?} from {operation}, got \
             {parsed}"
        );
        assert_eq!(
            parsed["datasyncErrorCode"].as_str(),
            parsed["errorCode"].as_str(),
            "case {case:?}: the two code members carry one token, got {parsed}"
        );
        assert_eq!(
            parsed["message"].as_str(),
            Some(
                format!(
                    "{agent} does not exist or has been deleted.",
                    agent = resource_id(&arn)
                )
                .as_str()
            ),
            "case {case:?}: wrong refusal wording from {operation}, got {parsed}"
        );
    }

    // The token classifies the refusal, so an agent this account DOES hold is reported
    // without one: the class members belong to the error, not to every response.
    let held = given_agent(&router, "PPPPP-QQQQQ-RRRRR-SSSSS-TTTTT", "a held agent").await;
    let described = ok(
        &router,
        "DescribeAgent",
        json!({ "AgentArn": &held }),
        "a held agent",
    )
    .await;
    for member in ["errorCode", "datasyncErrorCode"] {
        assert!(
            described[member].is_null(),
            "case \"a held agent\": a success carries no {member}, got {described}"
        );
    }
}
