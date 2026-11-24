// SPDX-License-Identifier: CC0-1.0

//! Whether an object storage transfer location exists is a fact about this service's
//! STATE, never about the shape of the ARN a request carries.
//!
//! RULE: `DescribeLocationObjectStorage` resolves its `LocationArn` in the locations a
//! `CreateLocation*` stored under exactly that string. The one ARN a create minted reads
//! back; every other well-formed `LocationArn` — including one that differs from a held
//! ARN in a single character of its partition, region, account id or location id — is
//! refused with the service's one modelled client error (HTTP 400,
//! `InvalidRequestException`), carrying `LocationNotFound` in both code members and the
//! message that names the ARN the request supplied.
//!
//! Reading is also reporting only: describing a location, and being refused for ARNs it
//! does not hold, leave the account's locations exactly as they were.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct, so a response that is
//! not computed from persisted state cannot pass.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for.
const SIGNED_REGION: &str = "us-west-2";

/// The discriminator DataSync reports, in both `errorCode` and `datasyncErrorCode`, for a
/// request naming a location the account does not hold.
const LOCATION_NOT_FOUND: &str = "LocationNotFound";

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

/// A required string member of a response body.
fn string(body: &serde_json::Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// Assert `operation` refused `arn` with the whole envelope a location this account does
/// not hold earns: HTTP 400, the one modelled client error, the not-found class in both
/// code members, and the message that names the ARN the request supplied.
async fn assert_refused_as_absent(
    router: &MultiServiceRouter,
    operation: &str,
    arn: &str,
    case: &str,
) {
    let (status, parsed) = call(
        router,
        operation,
        serde_json::json!({ "LocationArn": arn }),
    )
    .await;
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
            string(&parsed, member, case),
            LOCATION_NOT_FOUND,
            "case {case:?}: expected {member} {LOCATION_NOT_FOUND:?} from {operation}, got {parsed}"
        );
    }
    assert_eq!(
        string(&parsed, "message", case),
        format!("Location {arn} is not found."),
        "case {case:?}: wrong refusal wording from {operation}, got {parsed}"
    );
}

/// One agent this account has really activated, by ARN.
///
/// A `CreateLocationObjectStorage` resolves every `AgentArn` its request names against
/// the agents the account has activated, so a location that reaches its server through
/// an agent needs that agent activated first.
async fn given_agent(router: &MultiServiceRouter, case: &str) -> String {
    let created = ok(
        router,
        "CreateAgent",
        serde_json::json!({ "ActivationKey": "AAAAA-11111-BBBBB-22222-CCCCC" }),
        case,
    )
    .await;
    string(&created, "AgentArn", case)
}

/// A create request for an object storage location, distinct per `bucket`, reaching its
/// server through `agent_arn`.
fn create_request(bucket: &str, agent_arn: &str) -> serde_json::Value {
    serde_json::json!({
        "ServerHostname": format!("{bucket}.example.com"),
        "BucketName": bucket,
        "ServerProtocol": "HTTPS",
        "Subdirectory": "/inbox",
        "AccessKey": "transfer-user",
        "SecretKey": "transfer-secret",
        "AgentArns": [agent_arn],
    })
}

/// The prefix the modelled `LocationArn` pattern gives a location's resource part.
const LOCATION_RESOURCE_PREFIX: &str = "location/loc-";

/// The length the modelled `LocationArn` pattern gives a location id after that prefix
/// (`location/loc-[0-9a-z]{17}`).
const LOCATION_ID_LENGTH: usize = 17;

/// The segments of a location ARN that a mutation can land in:
/// `arn:<partition>:datasync:<region>:<account>:location/loc-<id>`.
struct ArnParts {
    partition: String,
    region: String,
    account: String,
    id: String,
}

impl ArnParts {
    /// Split a location ARN this service minted into its segments.
    fn of(arn: &str, case: &str) -> Self {
        let fields: Vec<&str> = arn.split(':').collect();
        assert_eq!(
            fields.len(),
            6,
            "case {case:?}: expected a six-field location ARN, got {arn:?}"
        );
        let id = fields[5]
            .strip_prefix(LOCATION_RESOURCE_PREFIX)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {LOCATION_RESOURCE_PREFIX:?} resource, got {arn:?}"
                )
            });
        assert_eq!(
            id.len(),
            LOCATION_ID_LENGTH,
            "case {case:?}: expected a {LOCATION_ID_LENGTH}-character location id, got {id:?}"
        );
        Self {
            partition: fields[1].to_string(),
            region: fields[3].to_string(),
            account: fields[4].to_string(),
            id: id.to_string(),
        }
    }

    /// The ARN these segments spell.
    fn arn(&self) -> String {
        format!(
            "arn:{}:datasync:{}:{}:{LOCATION_RESOURCE_PREFIX}{}",
            self.partition, self.region, self.account, self.id
        )
    }
}

/// Flip one character of a location id to another the modelled pattern admits
/// (`loc-[0-9a-z]{17}`), so the result is a well-formed `LocationArn` that addresses a
/// different location. `offset` counts from the end of the id.
fn flip_id_char(id: &str, offset: usize) -> String {
    let mut characters: Vec<char> = id.chars().collect();
    let index = characters
        .len()
        .checked_sub(offset + 1)
        .expect("the offset is inside the id");
    characters[index] = match characters[index] {
        '0' => '1',
        'a' => 'b',
        _ => '0',
    };
    characters.into_iter().collect()
}

/// Every well-formed `LocationArn` that is NOT the one `held` names, built by changing
/// one segment of it at a time.
///
/// The partitions are the ones the modelled `LocationArn` pattern admits
/// (`^arn:(aws|aws-cn|aws-us-gov|aws-eusc|aws-iso|aws-iso-b):datasync:[a-z\-0-9]+:[0-9]{12}:location/loc-[0-9a-z]{17}$`),
/// so each row differs from a held ARN in exactly one place: the partition, the region,
/// the account id, or one character of the location id at either end or in the middle.
fn near_misses(held: &str, case: &str) -> Vec<(String, String)> {
    let parts = ArnParts::of(held, case);
    let mut rows = Vec::new();

    for partition in ["aws-cn", "aws-us-gov", "aws-eusc", "aws-iso", "aws-iso-b"] {
        rows.push((
            format!("another partition ({partition})"),
            ArnParts {
                partition: partition.to_string(),
                ..ArnParts::of(held, case)
            }
            .arn(),
        ));
    }

    rows.push((
        "another region".to_string(),
        ArnParts {
            region: "eu-west-1".to_string(),
            ..ArnParts::of(held, case)
        }
        .arn(),
    ));
    rows.push((
        "another account".to_string(),
        ArnParts {
            account: "123456789012".to_string(),
            ..ArnParts::of(held, case)
        }
        .arn(),
    ));

    for (where_in_id, offset) in [
        ("the last character of the id", 0),
        ("a character in the middle of the id", LOCATION_ID_LENGTH / 2),
        ("the first character of the id", LOCATION_ID_LENGTH - 1),
    ] {
        let mutated = flip_id_char(&parts.id, offset);
        assert_ne!(
            mutated, parts.id,
            "case {case:?}: the mutation at offset {offset} must change the id"
        );
        rows.push((
            format!("{where_in_id} ({mutated})"),
            ArnParts {
                id: mutated,
                ..ArnParts::of(held, case)
            }
            .arn(),
        ));
    }

    rows
}

/// RULE: the object storage read resolves the exact ARN string the create stored, so an
/// ARN one character away from a held one names no location and is refused as absent —
/// and the held location keeps reading back unchanged afterwards.
#[tokio::test]
async fn an_object_storage_read_resolves_only_the_exact_arn_the_create_stored() {
    let router = service();
    let case = "a held object storage location";

    let agent_arn = given_agent(&router, case).await;
    let created = ok(
        &router,
        "CreateLocationObjectStorage",
        create_request("near-miss-bucket", &agent_arn),
        case,
    )
    .await;
    let held = string(&created, "LocationArn", case);

    // The one ARN the create minted reads back, so every refusal below is about the ARN
    // naming nothing rather than about the operation refusing everything.
    let described = ok(
        &router,
        "DescribeLocationObjectStorage",
        serde_json::json!({ "LocationArn": &held }),
        case,
    )
    .await;
    assert_eq!(
        string(&described, "LocationArn", case),
        held,
        "case {case:?}: the read must report the location it was asked for, got {described}"
    );

    let rows = near_misses(&held, case);
    assert!(
        rows.len() >= 8,
        "case {case:?}: expected the near-miss table to span every segment, got {} rows",
        rows.len()
    );
    for (row, arn) in &rows {
        assert_ne!(
            arn, &held,
            "case {row:?}: a near miss must differ from the held ARN"
        );
        assert_refused_as_absent(&router, "DescribeLocationObjectStorage", arn, row).await;
    }

    // Reading is reporting: neither the successful read nor any of the refusals changed
    // what the account holds, so the held location still reports the same body.
    let read_again = ok(
        &router,
        "DescribeLocationObjectStorage",
        serde_json::json!({ "LocationArn": &held }),
        case,
    )
    .await;
    assert_eq!(
        read_again, described,
        "case {case:?}: repeated reads must report the same state, got {read_again}"
    );
}

/// RULE: existence is per location — each held ARN resolves to its own location only, and
/// deleting one makes exactly that ARN refuse while its siblings keep reading back.
#[tokio::test]
async fn each_object_storage_location_resolves_only_under_its_own_arn() {
    let router = service();
    let case = "three object storage locations";

    let mut held: Vec<(String, String)> = Vec::new();
    let agent_arn = given_agent(&router, case).await;
    for bucket in ["first-bucket", "second-bucket", "third-bucket"] {
        let created = ok(
            &router,
            "CreateLocationObjectStorage",
            create_request(bucket, &agent_arn),
            case,
        )
        .await;
        held.push((bucket.to_string(), string(&created, "LocationArn", case)));
    }

    // Distinct creates mint distinct ARNs, each reporting its own bucket's URI.
    for (bucket, arn) in &held {
        let described = ok(
            &router,
            "DescribeLocationObjectStorage",
            serde_json::json!({ "LocationArn": arn }),
            bucket,
        )
        .await;
        assert_eq!(
            string(&described, "LocationArn", bucket),
            *arn,
            "case {bucket:?}: the read must report the ARN it was asked for, got {described}"
        );
        assert!(
            string(&described, "LocationUri", bucket).contains(bucket),
            "case {bucket:?}: the read must report this location's own URI, got {described}"
        );
    }

    // Deleting one location is what makes its ARN refuse; the others are untouched.
    while let Some((bucket, arn)) = held.pop() {
        ok(
            &router,
            "DeleteLocation",
            serde_json::json!({ "LocationArn": &arn }),
            &bucket,
        )
        .await;
        assert_refused_as_absent(&router, "DescribeLocationObjectStorage", &arn, &bucket).await;

        for (survivor_bucket, survivor) in &held {
            let described = ok(
                &router,
                "DescribeLocationObjectStorage",
                serde_json::json!({ "LocationArn": survivor }),
                survivor_bucket,
            )
            .await;
            assert_eq!(
                &string(&described, "LocationArn", survivor_bucket),
                survivor,
                "case {survivor_bucket:?}: deleting {arn} must leave this location readable, \
                 got {described}"
            );
        }
    }

    // A create after the deletes mints a new ARN, and only that one resolves.
    let created = ok(
        &router,
        "CreateLocationObjectStorage",
        create_request("fourth-bucket", &agent_arn),
        case,
    )
    .await;
    let fresh = string(&created, "LocationArn", case);
    let described = ok(
        &router,
        "DescribeLocationObjectStorage",
        serde_json::json!({ "LocationArn": &fresh }),
        "a location created after the deletes",
    )
    .await;
    assert_eq!(
        string(&described, "LocationArn", case),
        fresh,
        "case {case:?}: the location created last must read back, got {described}"
    );
}
