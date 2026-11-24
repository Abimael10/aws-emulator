// SPDX-License-Identifier: CC0-1.0

//! Deleting a DataSync transfer location, what a read of a location the account does not
//! hold answers, and when a delete is refused because a transfer task still references
//! the location, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct, so a response that is
//! not computed from persisted state cannot pass.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for.
const SIGNED_REGION: &str = "us-west-2";

/// The canonical account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The discriminator DataSync reports, in both `errorCode` and `datasyncErrorCode`, for
/// a request that names a location the account does not hold.
const LOCATION_NOT_FOUND: &str = "LocationNotFound";

/// The discriminator DataSync reports, in both `errorCode` and `datasyncErrorCode`, for
/// a delete of a location a transfer task still references.
const LOCATION_IN_USE: &str = "LocationInUse";

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
async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
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
        serde_json::json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or_else(|_| {
            serde_json::json!({ "__unparsed": String::from_utf8_lossy(&payload) })
        })
    };
    (status, parsed)
}

/// A call that must succeed, returning its body.
async fn ok(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A call that must be refused with HTTP 400 `InvalidRequestException`, returning the
/// refusal body so its message and discriminators can be asserted.
async fn refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} with {parsed}"
    );
    let wire_type = parsed["__type"].as_str().unwrap_or_default();
    assert!(
        wire_type.contains("InvalidRequestException"),
        "case {case:?}: expected an InvalidRequestException from {operation}, got {parsed}"
    );
    parsed
}

/// A required string member of a response body.
fn string(body: &serde_json::Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// Assert the whole refusal envelope a request naming an absent location earns: the
/// message that names it, and the one discriminator in both code members.
fn assert_location_not_found(refusal: &serde_json::Value, location_arn: &str, case: &str) {
    assert_eq!(
        string(refusal, "message", case),
        format!("Location {location_arn} is not found."),
        "case {case:?}: the refusal must name the location it could not find, got {refusal}"
    );
    for member in ["errorCode", "datasyncErrorCode"] {
        assert_eq!(
            string(refusal, member, case),
            LOCATION_NOT_FOUND,
            "case {case:?}: expected {member} {LOCATION_NOT_FOUND:?}, got {refusal}"
        );
    }
}

/// An agent ARN in the modelled `AgentArn` shape.
fn agent_arn(tail: char) -> String {
    format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:agent/agent-0123456789abcdef{tail}")
}

/// One kind of transfer location: the create that mints it, and the describe that
/// reports it.
struct Kind {
    case: &'static str,
    create_operation: &'static str,
    describe_operation: &'static str,
    request: fn() -> serde_json::Value,
}

/// Every kind of location an implemented `CreateLocation*` mints, so the rule is
/// asserted over the whole set rather than the one kind the report cited. The members
/// of each request are the ones that operation models as required, plus what the
/// matching suite in this crate already exercises.
fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            case: "an object storage location",
            create_operation: "CreateLocationObjectStorage",
            describe_operation: "DescribeLocationObjectStorage",
            request: || {
                serde_json::json!({
                    "ServerHostname": "store.example.com",
                    "BucketName": "transfer-bucket",
                    "ServerProtocol": "HTTPS",
                    "Subdirectory": "/inbox",
                    "AccessKey": "transfer-user",
                    "SecretKey": "transfer-secret",
                    "AgentArns": [agent_arn('0')],
                })
            },
        },
        Kind {
            case: "an Amazon S3 location",
            create_operation: "CreateLocationS3",
            describe_operation: "DescribeLocationS3",
            request: || {
                serde_json::json!({
                    "S3BucketArn": "arn:aws:s3:::transfer-bucket",
                    "S3Config": {
                        "BucketAccessRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/datasync"),
                    },
                })
            },
        },
        Kind {
            case: "an Azure Blob Storage location",
            create_operation: "CreateLocationAzureBlob",
            describe_operation: "DescribeLocationAzureBlob",
            request: || {
                serde_json::json!({
                    "ContainerUrl": "https://hotstore.blob.core.windows.net/transfer-in",
                    "AuthenticationType": "SAS",
                    "SasConfiguration": { "Token": "sv=2026-01-01&sig=abc" },
                    "AgentArns": [agent_arn('1')],
                })
            },
        },
        Kind {
            case: "an Amazon EFS location",
            create_operation: "CreateLocationEfs",
            describe_operation: "DescribeLocationEfs",
            request: || {
                serde_json::json!({
                    "EfsFilesystemArn": format!(
                        "arn:aws:elasticfilesystem:{SIGNED_REGION}:{ACCOUNT}:file-system/fs-08e04cd4"
                    ),
                    "Ec2Config": {
                        "SubnetArn": format!(
                            "arn:aws:ec2:{SIGNED_REGION}:{ACCOUNT}:subnet/subnet-1234567890abcdef1"
                        ),
                        "SecurityGroupArns": [format!(
                            "arn:aws:ec2:{SIGNED_REGION}:{ACCOUNT}:security-group/sg-1234567890abcdef2"
                        )],
                    },
                })
            },
        },
    ]
}

/// `DeleteLocation` removes the location its ARN addresses, whatever kind it is, and
/// afterwards that ARN resolves to nothing: the matching `DescribeLocation*` refuses it
/// as a location the account does not hold, and so does a second delete.
#[tokio::test]
async fn deleting_a_location_of_any_kind_leaves_nothing_to_read() {
    let router = service();

    for kind in kinds() {
        let case = kind.case;
        let created = ok(&router, kind.create_operation, (kind.request)(), case).await;
        let arn = string(&created, "LocationArn", case);

        // The location is readable while it is held, so the refusal below is the
        // delete's doing and not the describe's.
        let described = ok(
            &router,
            kind.describe_operation,
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_eq!(
            string(&described, "LocationArn", case),
            arn,
            "case {case:?}: the describe must report the location it was asked for, got {described}"
        );

        let deleted = ok(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_eq!(
            deleted,
            serde_json::json!({}),
            "case {case:?}: DeleteLocation reports no members, got {deleted}"
        );

        let refusal = refused(
            &router,
            kind.describe_operation,
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_location_not_found(&refusal, &arn, case);

        // Nothing is left to delete either: the second delete is refused in the same
        // words, so the first one really removed the record.
        let second = refused(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_location_not_found(&second, &arn, case);
    }
}

/// A delete removes ONLY the location its ARN addresses: every other location the
/// account holds is still reported from state afterwards.
#[tokio::test]
async fn deleting_one_location_leaves_every_other_location_readable() {
    let router = service();
    let case = "four locations, one deleted at a time";

    let kinds = kinds();
    let mut held: Vec<(usize, String)> = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        let created = ok(&router, kind.create_operation, (kind.request)(), case).await;
        held.push((index, string(&created, "LocationArn", case)));
    }

    while let Some((index, arn)) = held.pop() {
        ok(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        let refusal = refused(
            &router,
            kinds[index].describe_operation,
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_location_not_found(&refusal, &arn, case);

        for (survivor_index, survivor) in &held {
            let described = ok(
                &router,
                kinds[*survivor_index].describe_operation,
                serde_json::json!({ "LocationArn": survivor }),
                case,
            )
            .await;
            assert_eq!(
                &string(&described, "LocationArn", case),
                survivor,
                "case {case:?}: deleting {arn} must leave {survivor} readable, got {described}"
            );
        }
    }
}

/// A well-formed location ARN the account never held resolves to nothing: every
/// operation that resolves a `LocationArn` refuses it with the same envelope.
///
/// The ARNs span what the modelled `LocationArn` pattern generalises over — each
/// partition the pattern admits
/// (`^arn:(aws|aws-cn|aws-us-gov|aws-eusc|aws-iso|aws-iso-b):datasync:[a-z\-0-9]+:[0-9]{12}:location/loc-[0-9a-z]{17}$`),
/// a region segment other than the signed one, another account's id, and a location id
/// of digits only, letters only and both.
#[tokio::test]
async fn a_location_the_account_never_held_is_refused_by_every_reader() {
    let router = service();

    let absent = [
        format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:location/loc-0abcdef1234567890"),
        format!("arn:aws:datasync:eu-west-1:{ACCOUNT}:location/loc-00000000000000001"),
        "arn:aws-cn:datasync:cn-north-1:123456789012:location/loc-abcdefghijklmnopq".to_string(),
        "arn:aws-us-gov:datasync:us-gov-west-1:210987654321:location/loc-12345678901234567"
            .to_string(),
        "arn:aws-iso-b:datasync:us-isob-east-1:555555555555:location/loc-0a1b2c3d4e5f60718"
            .to_string(),
    ];

    for arn in &absent {
        for operation in [
            "DeleteLocation",
            "DescribeLocationObjectStorage",
            "DescribeLocationS3",
            "DescribeLocationAzureBlob",
            "DescribeLocationEfs",
        ] {
            let case = format!("{operation} on {arn}");
            let refusal = refused(
                &router,
                operation,
                serde_json::json!({ "LocationArn": arn }),
                &case,
            )
            .await;
            assert_location_not_found(&refusal, arn, &case);
        }

        // An update resolves the same ARN, so it owes the same refusal.
        let case = format!("UpdateLocationObjectStorage on {arn}");
        let refusal = refused(
            &router,
            "UpdateLocationObjectStorage",
            serde_json::json!({ "LocationArn": arn, "AccessKey": "someone" }),
            &case,
        )
        .await;
        assert_location_not_found(&refusal, arn, &case);
    }
}

/// A task cannot be created against a location that was deleted: `CreateTask` resolves
/// both of its location ARNs in state and reports the same discriminator for one that
/// names nothing.
#[tokio::test]
async fn create_task_refuses_a_deleted_location() {
    let router = service();
    let case = "a task whose source location was deleted";
    let kinds = kinds();

    let mut arns: Vec<String> = Vec::new();
    for kind in kinds.iter().take(2) {
        let created = ok(&router, kind.create_operation, (kind.request)(), case).await;
        arns.push(string(&created, "LocationArn", case));
    }

    ok(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": arns[0] }),
        case,
    )
    .await;

    let refusal = refused(
        &router,
        "CreateTask",
        serde_json::json!({
            "SourceLocationArn": arns[0],
            "DestinationLocationArn": arns[1],
        }),
        case,
    )
    .await;
    for member in ["errorCode", "datasyncErrorCode"] {
        assert_eq!(
            string(&refusal, member, case),
            LOCATION_NOT_FOUND,
            "case {case:?}: expected {member} {LOCATION_NOT_FOUND:?}, got {refusal}"
        );
    }
    // The location is identified by its own resource id under either wording of this
    // refusal, so the assertion holds whichever one CreateTask reports.
    let location_id = arns[0].rsplit('/').next().unwrap_or(arns[0].as_str());
    assert!(
        string(&refusal, "message", case).contains(location_id),
        "case {case:?}: the refusal must name the location it could not find, got {refusal}"
    );

    // The destination survived, so the refusal is about the deleted location only.
    ok(
        &router,
        kinds[1].describe_operation,
        serde_json::json!({ "LocationArn": arns[1] }),
        case,
    )
    .await;
}

/// The bare resource id of a DataSync ARN — `loc-…`, `task-…`: how this service names a
/// resource in a refusal message.
fn resource_id(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// A transfer task between two locations, created and returned by ARN.
async fn create_task(
    router: &MultiServiceRouter,
    source: &str,
    destination: &str,
    name: &str,
    case: &str,
) -> String {
    let created = ok(
        router,
        "CreateTask",
        serde_json::json!({
            "SourceLocationArn": source,
            "DestinationLocationArn": destination,
            "Name": name,
        }),
        case,
    )
    .await;
    string(&created, "TaskArn", case)
}

/// Assert the whole refusal envelope a delete of a still-referenced location earns: the
/// one in-use discriminator in both code members, and the sentence that names ONLY the
/// location the request addressed.
///
/// The sentence is fixed text plus one variable part, the bare resource id of the
/// `LocationArn` the request carried. The tasks that hold the location are passed in so
/// the assertion can also prove none of them is named: which resource holds the location
/// is state the caller did not ask about, so it is no part of the message.
fn assert_location_in_use(
    refusal: &serde_json::Value,
    location_arn: &str,
    task_arns: &[&str],
    case: &str,
) {
    for member in ["errorCode", "datasyncErrorCode"] {
        assert_eq!(
            string(refusal, member, case),
            LOCATION_IN_USE,
            "case {case:?}: expected {member} {LOCATION_IN_USE:?}, got {refusal}"
        );
    }
    let message = string(refusal, "message", case);
    assert_eq!(
        message,
        format!("Location {} is in use.", resource_id(location_arn)),
        "case {case:?}: the refusal must name only the location it would not delete, \
         got {refusal}"
    );
    for task_arn in task_arns {
        assert!(
            !message.contains(resource_id(task_arn)),
            "case {case:?}: the refusal must not name the holding task {task_arn:?}, \
             got {refusal}"
        );
    }
}

/// Read a task's status back through `DescribeTask` until it is `want`, or give up after
/// a deadline far longer than the transition the clock owes.
///
/// The wait is for the CONDITION, never for an elapsed time, so a loaded host only makes
/// the loop go round more often.
async fn poll_task_status(router: &MultiServiceRouter, task_arn: &str, want: &str, case: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let described = ok(
            router,
            "DescribeTask",
            serde_json::json!({ "TaskArn": task_arn }),
            case,
        )
        .await;
        let status = string(&described, "Status", case);
        if status == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the task to reach {want:?}, last saw {status:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// RULE: a transfer location a transfer task still names as an endpoint cannot be
/// deleted — `DeleteLocation` refuses it with the in-use discriminator in both code
/// members and removes nothing, and serves the same delete once no task references the
/// location any more.
///
/// The rows differ along what the rule generalises over: which endpoint member of the
/// task holds the ARN (source and destination are both asserted per row) and which KIND
/// of location each endpoint is, so no pairing of kinds is privileged.
#[tokio::test]
async fn a_location_a_task_references_cannot_be_deleted_until_the_task_is_gone() {
    let kinds = kinds();

    for source_index in 0..kinds.len() {
        let destination_index = (source_index + 1) % kinds.len();
        let source_kind = &kinds[source_index];
        let destination_kind = &kinds[destination_index];
        let case = format!(
            "{source} as source, {destination} as destination",
            source = source_kind.case,
            destination = destination_kind.case
        );
        // One account per row, so each row's two locations are the only ones held.
        let router = service();

        let source = string(
            &ok(
                &router,
                source_kind.create_operation,
                (source_kind.request)(),
                &case,
            )
            .await,
            "LocationArn",
            &case,
        );
        let destination = string(
            &ok(
                &router,
                destination_kind.create_operation,
                (destination_kind.request)(),
                &case,
            )
            .await,
            "LocationArn",
            &case,
        );
        let task = create_task(&router, &source, &destination, "in-use", &case).await;

        // Both endpoints are in use, and the refusal leaves each location readable.
        for (endpoint, arn, kind) in [
            ("source", &source, source_kind),
            ("destination", &destination, destination_kind),
        ] {
            let case = format!("{case}, deleting the {endpoint}");
            let refusal = refused(
                &router,
                "DeleteLocation",
                serde_json::json!({ "LocationArn": arn }),
                &case,
            )
            .await;
            assert_location_in_use(&refusal, arn, &[task.as_str()], &case);

            let described = ok(
                &router,
                kind.describe_operation,
                serde_json::json!({ "LocationArn": arn }),
                &case,
            )
            .await;
            assert_eq!(
                &string(&described, "LocationArn", &case),
                arn,
                "case {case:?}: the refused delete must leave the location held, got {described}"
            );
        }

        // Teardown in the order the refusal asks for: the task first, then the two
        // locations it referenced — each an empty 200, because the refusal above kept
        // them.
        ok(
            &router,
            "DeleteTask",
            serde_json::json!({ "TaskArn": task }),
            &case,
        )
        .await;

        for (endpoint, arn, kind) in [
            ("source", &source, source_kind),
            ("destination", &destination, destination_kind),
        ] {
            let case = format!("{case}, deleting the {endpoint} after the task");
            let deleted = ok(
                &router,
                "DeleteLocation",
                serde_json::json!({ "LocationArn": arn }),
                &case,
            )
            .await;
            assert_eq!(
                deleted,
                serde_json::json!({}),
                "case {case:?}: DeleteLocation reports no members, got {deleted}"
            );
            let gone = refused(
                &router,
                kind.describe_operation,
                serde_json::json!({ "LocationArn": arn }),
                &case,
            )
            .await;
            assert_location_not_found(&gone, arn, &case);
        }
    }
}

/// RULE (the same one, over the COUNT of references): a location is deletable only once
/// NO task names it, so a location two tasks share survives the delete of either one of
/// them and goes only after both.
#[tokio::test]
async fn a_location_two_tasks_share_is_in_use_until_both_tasks_are_gone() {
    let router = service();
    let case = "one location shared by two tasks";
    let kinds = kinds();

    let mut arns: Vec<String> = Vec::new();
    for kind in kinds.iter().take(3) {
        let created = ok(&router, kind.create_operation, (kind.request)(), case).await;
        arns.push(string(&created, "LocationArn", case));
    }
    // The middle location is the destination of one task and the source of the other.
    let first = create_task(&router, &arns[0], &arns[1], "shared-first", case).await;
    let second = create_task(&router, &arns[1], &arns[2], "shared-second", case).await;

    let refusal = refused(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": arns[1] }),
        case,
    )
    .await;
    assert_location_in_use(&refusal, &arns[1], &[first.as_str(), second.as_str()], case);

    // One task gone, one still referencing: still in use, and the refusal reads the same
    // whichever task is the one that remains.
    ok(
        &router,
        "DeleteTask",
        serde_json::json!({ "TaskArn": first }),
        case,
    )
    .await;
    let case_after_first = format!("{case}, after the first task was deleted");
    let refusal = refused(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": arns[1] }),
        &case_after_first,
    )
    .await;
    assert_location_in_use(&refusal, &arns[1], &[second.as_str()], &case_after_first);

    ok(
        &router,
        "DeleteTask",
        serde_json::json!({ "TaskArn": second }),
        case,
    )
    .await;
    let case_after_both = format!("{case}, after both tasks were deleted");
    let deleted = ok(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": arns[1] }),
        &case_after_both,
    )
    .await;
    assert_eq!(
        deleted,
        serde_json::json!({}),
        "case {case_after_both:?}: DeleteLocation reports no members, got {deleted}"
    );

    // That delete is what removed the location: a second one finds nothing, and the two
    // locations the deleted tasks also referenced are deletable as well.
    let second_delete = refused(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": arns[1] }),
        &case_after_both,
    )
    .await;
    assert_location_not_found(&second_delete, &arns[1], &case_after_both);
    for arn in [&arns[0], &arns[2]] {
        ok(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": arn }),
            &case_after_both,
        )
        .await;
    }
}

/// RULE (the same one, over the task's lifecycle status): a task references its two
/// locations from the moment it is created, so the in-use refusal does not depend on the
/// status the task has reached.
#[tokio::test]
async fn a_location_is_in_use_whatever_status_the_referencing_task_has() {
    let router = service();
    let kinds = kinds();

    let source = string(
        &ok(
            &router,
            kinds[0].create_operation,
            (kinds[0].request)(),
            "setup",
        )
        .await,
        "LocationArn",
        "setup",
    );
    let destination = string(
        &ok(
            &router,
            kinds[1].create_operation,
            (kinds[1].request)(),
            "setup",
        )
        .await,
        "LocationArn",
        "setup",
    );
    let task = create_task(&router, &source, &destination, "status-span", "setup").await;

    // Straight after the create, while the task is still reaching its first status.
    let case = "immediately after CreateTask";
    let refusal = refused(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": source }),
        case,
    )
    .await;
    assert_location_in_use(&refusal, &source, &[task.as_str()], case);

    // And once the task has settled into the status it waits in.
    poll_task_status(&router, &task, "AVAILABLE", "waiting for the task").await;
    let case = "after the task reached AVAILABLE";
    let refusal = refused(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": source }),
        case,
    )
    .await;
    assert_location_in_use(&refusal, &source, &[task.as_str()], case);

    // The same delete, once nothing references the location.
    ok(
        &router,
        "DeleteTask",
        serde_json::json!({ "TaskArn": task }),
        case,
    )
    .await;
    ok(
        &router,
        "DeleteLocation",
        serde_json::json!({ "LocationArn": source }),
        case,
    )
    .await;
}

/// Which endpoint member of each holding task carries the location whose delete is
/// refused.
#[derive(Debug, Clone, Copy)]
enum HeldAs {
    Source,
    Destination,
    /// Half the holding tasks each way, so the location is held from both members at
    /// once.
    Alternating,
}

/// One row of the in-use wording table: the kind of location whose delete is refused,
/// which endpoint member holds it, and how many tasks hold it.
#[derive(Debug)]
struct WordingCase {
    case: &'static str,
    kind_index: usize,
    held_as: HeldAs,
    holders: usize,
}

/// RULE (the WORDING of that refusal): the in-use refusal's message is fixed text with
/// exactly ONE variable part, the bare resource id of the `LocationArn` the request
/// carried — so no identifier the emulator reads out of its own state reaches the
/// sentence, and in particular the tasks that hold the location are not named.
///
/// The rows differ along everything the rule generalises over and the sentence therefore
/// must NOT vary with: the kind of location being deleted, which endpoint member of a
/// task holds it, and how many tasks hold it (one, two, three, from either member or
/// both). Every row holds its own freshly minted location, so the one variable part is
/// also shown to track the request rather than being fixed text.
#[tokio::test]
async fn the_in_use_refusal_names_only_the_location_the_request_addressed() {
    let router = service();
    let kinds = kinds();

    let rows = [
        WordingCase {
            case: "one task holding an object storage location as its source",
            kind_index: 0,
            held_as: HeldAs::Source,
            holders: 1,
        },
        WordingCase {
            case: "one task holding an Amazon S3 location as its destination",
            kind_index: 1,
            held_as: HeldAs::Destination,
            holders: 1,
        },
        WordingCase {
            case: "two tasks holding an Azure Blob Storage location as their destination",
            kind_index: 2,
            held_as: HeldAs::Destination,
            holders: 2,
        },
        WordingCase {
            case: "three tasks holding an Amazon EFS location from both endpoints",
            kind_index: 3,
            held_as: HeldAs::Alternating,
            holders: 3,
        },
    ];

    for row in &rows {
        let case = row.case;
        let kind = &kinds[row.kind_index];
        let held = string(
            &ok(&router, kind.create_operation, (kind.request)(), case).await,
            "LocationArn",
            case,
        );

        let mut holders: Vec<String> = Vec::new();
        for holder in 0..row.holders {
            // Each holding task needs a second endpoint of its own: a task copies one
            // location onto another, so the pair cannot be the same location.
            let other_kind = &kinds[(row.kind_index + 1 + holder) % kinds.len()];
            let other = string(
                &ok(
                    &router,
                    other_kind.create_operation,
                    (other_kind.request)(),
                    case,
                )
                .await,
                "LocationArn",
                case,
            );
            let held_as_source = match row.held_as {
                HeldAs::Source => true,
                HeldAs::Destination => false,
                HeldAs::Alternating => holder % 2 == 0,
            };
            let (source, destination) = if held_as_source {
                (&held, &other)
            } else {
                (&other, &held)
            };
            holders.push(
                create_task(
                    &router,
                    source,
                    destination,
                    &format!("wording-holder-{holder}"),
                    case,
                )
                .await,
            );
        }

        let refusal = refused(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": held }),
            case,
        )
        .await;
        let holder_arns: Vec<&str> = holders.iter().map(String::as_str).collect();
        assert_location_in_use(&refusal, &held, &holder_arns, case);

        // Nothing of the holding tasks is in the sentence — neither an id nor the word
        // for the resource that holds the location.
        let message = string(&refusal, "message", case);
        assert!(
            !message.to_ascii_lowercase().contains("task"),
            "case {case:?}: the refusal must not mention the holding task at all, \
             got {message:?}"
        );
    }
}
