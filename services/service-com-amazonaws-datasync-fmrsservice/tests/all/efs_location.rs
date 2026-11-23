// SPDX-License-Identifier: CC0-1.0

//! Amazon EFS transfer locations, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct, so a response that is
//! not computed from persisted state cannot pass.
//!
//! The cases at the end of this suite cover what DataSync proves about the FILE SYSTEM
//! before it stores such a location, against a router that mounts this emulator's own
//! Amazon EFS beside this service on ONE engine — so every file system they name is one
//! that service really minted, through its own API.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for. It is deliberately NOT the
/// emulator's default, so an ARN that carries it proves the region came from the
/// request's SigV4 credential scope.
const SIGNED_REGION: &str = "us-west-2";

/// The canonical account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `EfsInTransitEncryption` value that encrypts the mount, from the modelled enum
/// (`smithy_query(query="shape",shape="EfsInTransitEncryption")` -> `NONE | TLS1_2`).
const TLS: &str = "TLS1_2";

/// The other value that enum admits: no Transport Layer Security on the mount.
const NO_TLS: &str = "NONE";

/// An IAM role that lets DataSync reach a file system, as the modelled `IamRoleArn`
/// spells one.
const FILE_SYSTEM_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/datasync-efs-access";

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

/// A call that must be refused with HTTP 400 `InvalidRequestException` whose message
/// contains `expected_message`.
async fn invalid_request(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
    expected_message: &str,
    case: &str,
) {
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
    let message = parsed["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(expected_message),
        "case {case:?}: expected the refusal to say {expected_message:?}, got {message:?}"
    );
}

/// A required string member of a response body.
fn string(body: &serde_json::Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// The modelled `LocationArn` shape: `…:location/loc-[0-9a-z]{17}`.
fn assert_location_arn(arn: &str, case: &str) {
    let prefix = format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:location/loc-");
    let id = arn.strip_prefix(&prefix).unwrap_or_default();
    assert!(
        id.len() == 17 && id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
        "case {case:?}: expected a {prefix}<17 lowercase alphanumerics> ARN, got {arn:?}"
    );
}

/// The modelled `EfsFilesystemArn` for a file system in `region`
/// (`…:elasticfilesystem:<region>:<account>:file-system/fs-[0-9a-f]{8,40}`).
fn filesystem_arn(region: &str, filesystem_id: &str) -> String {
    format!("arn:aws:elasticfilesystem:{region}:{ACCOUNT}:file-system/{filesystem_id}")
}

/// The modelled `Ec2SubnetArn` for a subnet in `region`.
fn subnet_arn(region: &str, subnet_id: &str) -> String {
    format!("arn:aws:ec2:{region}:{ACCOUNT}:subnet/{subnet_id}")
}

/// The modelled `Ec2SecurityGroupArn`s for `ids`, in `region`.
fn security_group_arns(region: &str, ids: &[&str]) -> Vec<String> {
    ids.iter()
        .map(|id| format!("arn:aws:ec2:{region}:{ACCOUNT}:security-group/{id}"))
        .collect()
}

/// The modelled `EfsAccessPointArn` for an access point in `region`
/// (`…:elasticfilesystem:<region>:<account>:access-point/fsap-[0-9a-f]{8,40}`).
fn access_point_arn(region: &str, access_point_id: &str) -> String {
    format!("arn:aws:elasticfilesystem:{region}:{ACCOUNT}:access-point/{access_point_id}")
}

/// One `CreateLocationEfs` request and what the stored location must report.
struct Row {
    case: &'static str,
    /// The Region of the file system, its subnet and its security groups — not the
    /// Region the request is signed for.
    region: &'static str,
    filesystem_id: &'static str,
    subnet_id: &'static str,
    security_group_ids: &'static [&'static str],
    subdirectory: Option<&'static str>,
    access_point_id: Option<&'static str>,
    file_system_access_role_arn: Option<&'static str>,
    in_transit_encryption: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
    expected_uri: &'static str,
}

/// The request body a row asks for.
fn request_of(row: &Row) -> serde_json::Value {
    let mut request = serde_json::json!({
        "EfsFilesystemArn": filesystem_arn(row.region, row.filesystem_id),
        "Ec2Config": {
            "SubnetArn": subnet_arn(row.region, row.subnet_id),
            "SecurityGroupArns": security_group_arns(row.region, row.security_group_ids),
        },
    });
    if let Some(subdirectory) = row.subdirectory {
        request["Subdirectory"] = serde_json::json!(subdirectory);
    }
    if let Some(access_point_id) = row.access_point_id {
        request["AccessPointArn"] = serde_json::json!(access_point_arn(row.region, access_point_id));
    }
    if let Some(role) = row.file_system_access_role_arn {
        request["FileSystemAccessRoleArn"] = serde_json::json!(role);
    }
    if let Some(in_transit_encryption) = row.in_transit_encryption {
        request["InTransitEncryption"] = serde_json::json!(in_transit_encryption);
    }
    if !row.tags.is_empty() {
        request["Tags"] = serde_json::json!(
            row.tags
                .iter()
                .map(|(key, value)| serde_json::json!({ "Key": key, "Value": value }))
                .collect::<Vec<_>>()
        );
    }
    request
}

/// The rows every success case in this suite spans.
///
/// They differ along what the rule generalises over: the `Subdirectory` spellings that
/// denote one mount path (absent, `/`, with and without surrounding slashes, nested),
/// the Region and id of the file system the URI is assembled from, the number of
/// security groups the `Ec2Config` carries (the modelled list is `len 1..5`, so one and
/// five are its bounds), a restricted mount reached through an access point or an IAM
/// role (either of which requires `TLS1_2`), an explicit `NONE` encryption, and tags
/// versus none.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "file system root, nothing optional declared",
            region: "us-east-2",
            filesystem_id: "fs-08e04cd4",
            subnet_id: "subnet-1234567890abcdef1",
            security_group_ids: &["sg-1234567890abcdef2"],
            subdirectory: None,
            access_point_id: None,
            file_system_access_role_arn: None,
            in_transit_encryption: None,
            tags: &[],
            expected_uri: "efs://us-east-2.fs-08e04cd4/",
        },
        Row {
            case: "the root spelled as a single slash",
            region: "us-east-2",
            filesystem_id: "fs-08e04cd4",
            subnet_id: "subnet-1234567890abcdef1",
            security_group_ids: &["sg-1234567890abcdef2"],
            subdirectory: Some("/"),
            access_point_id: None,
            file_system_access_role_arn: None,
            in_transit_encryption: Some(NO_TLS),
            tags: &[("Name", "efs-root")],
            expected_uri: "efs://us-east-2.fs-08e04cd4/",
        },
        Row {
            case: "a mount path with a leading slash",
            region: "eu-west-1",
            filesystem_id: "fs-0123456789abcdef0",
            subnet_id: "subnet-00ff11ee22dd33cc4",
            security_group_ids: &["sg-00ff11ee22dd33cc5", "sg-00ff11ee22dd33cc6"],
            subdirectory: Some("/path/to/folder"),
            access_point_id: None,
            file_system_access_role_arn: None,
            in_transit_encryption: None,
            tags: &[],
            expected_uri: "efs://eu-west-1.fs-0123456789abcdef0/path/to/folder/",
        },
        Row {
            case: "the same mount path spelled with a trailing slash",
            region: "eu-west-1",
            filesystem_id: "fs-0123456789abcdef0",
            subnet_id: "subnet-00ff11ee22dd33cc4",
            security_group_ids: &["sg-00ff11ee22dd33cc5"],
            subdirectory: Some("path/to/folder/"),
            access_point_id: None,
            file_system_access_role_arn: None,
            in_transit_encryption: None,
            tags: &[("Name", "efs-folder"), ("Stage", "test")],
            expected_uri: "efs://eu-west-1.fs-0123456789abcdef0/path/to/folder/",
        },
        Row {
            case: "a restricted mount through an access point",
            region: "ap-southeast-2",
            filesystem_id: "fs-abcdef0123456789abcdef0123456789abcdef01",
            subnet_id: "subnet-0a1b2c3d4e5f60718",
            security_group_ids: &[
                "sg-0a1b2c3d4e5f60719",
                "sg-0a1b2c3d4e5f6071a",
                "sg-0a1b2c3d4e5f6071b",
                "sg-0a1b2c3d4e5f6071c",
                "sg-0a1b2c3d4e5f6071d",
            ],
            subdirectory: Some("/restricted"),
            access_point_id: Some("fsap-0123456789abcdef0"),
            file_system_access_role_arn: None,
            in_transit_encryption: Some(TLS),
            tags: &[("Name", "efs-restricted")],
            expected_uri:
                "efs://ap-southeast-2.fs-abcdef0123456789abcdef0123456789abcdef01/restricted/",
        },
        Row {
            case: "a restricted mount through an IAM role",
            region: "us-west-2",
            filesystem_id: "fs-deadbeef",
            subnet_id: "subnet-0f1e2d3c4b5a69788",
            security_group_ids: &["sg-0f1e2d3c4b5a69789"],
            subdirectory: None,
            access_point_id: None,
            file_system_access_role_arn: Some(FILE_SYSTEM_ACCESS_ROLE_ARN),
            in_transit_encryption: Some(TLS),
            tags: &[],
            expected_uri: "efs://us-west-2.fs-deadbeef/",
        },
        Row {
            case: "an access point and an IAM role together",
            region: "us-west-2",
            filesystem_id: "fs-deadbeef",
            subnet_id: "subnet-0f1e2d3c4b5a69788",
            security_group_ids: &["sg-0f1e2d3c4b5a69789", "sg-0f1e2d3c4b5a6978a"],
            subdirectory: Some("shared/inbox"),
            access_point_id: Some("fsap-abcdef0123456789"),
            file_system_access_role_arn: Some(FILE_SYSTEM_ACCESS_ROLE_ARN),
            in_transit_encryption: Some(TLS),
            tags: &[("Name", "efs-both")],
            expected_uri: "efs://us-west-2.fs-deadbeef/shared/inbox/",
        },
    ]
}

/// `CreateLocationEfs` stores the location it mints, and `DescribeLocationEfs` reports
/// it from state.
#[tokio::test]
async fn created_efs_locations_are_reported_from_state() {
    let router = service();
    let mut arns: Vec<String> = Vec::new();

    for row in &rows() {
        let case = row.case;
        let created = ok(&router, "CreateLocationEfs", request_of(row), case).await;
        let arn = string(&created, "LocationArn", case);
        assert_location_arn(&arn, case);
        assert!(
            !arns.contains(&arn),
            "case {case:?}: every created location gets its own ARN, got {arn:?} again"
        );
        assert_eq!(
            created.as_object().map(|members| members.len()),
            Some(1),
            "case {case:?}: the response carries only the minted ARN, got {created}"
        );
        arns.push(arn.clone());

        let described = ok(
            &router,
            "DescribeLocationEfs",
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;

        assert_eq!(
            string(&described, "LocationArn", case),
            arn,
            "case {case:?}: the describe must report the ARN it was asked about, got {described}"
        );
        assert_eq!(
            string(&described, "LocationUri", case),
            row.expected_uri,
            "case {case:?}: expected the file system and mount path the request named, \
             got {described}"
        );
        assert_eq!(
            described["Ec2Config"],
            serde_json::json!({
                "SubnetArn": subnet_arn(row.region, row.subnet_id),
                "SecurityGroupArns": security_group_arns(row.region, row.security_group_ids),
            }),
            "case {case:?}: expected the subnet and security groups the request named, \
             got {described}"
        );

        let expected_access_point = row
            .access_point_id
            .map(|id| serde_json::json!(access_point_arn(row.region, id)))
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            described["AccessPointArn"], expected_access_point,
            "case {case:?}: expected the access point the request named, and none otherwise, \
             got {described}"
        );
        assert_eq!(
            described["FileSystemAccessRoleArn"],
            row.file_system_access_role_arn
                .map(|role| serde_json::json!(role))
                .unwrap_or(serde_json::Value::Null),
            "case {case:?}: expected the IAM role the request named, and none otherwise, \
             got {described}"
        );
        assert_eq!(
            described["InTransitEncryption"],
            row.in_transit_encryption
                .map(|value| serde_json::json!(value))
                .unwrap_or(serde_json::Value::Null),
            "case {case:?}: expected the encryption the request named, and none otherwise, \
             got {described}"
        );

        let creation_time = described["CreationTime"].as_f64().unwrap_or_default();
        assert!(
            creation_time > 0.0,
            "case {case:?}: expected the creation time the location was stored with, \
             got {described}"
        );

        // A read reports; it never changes what the next reader sees.
        let again = ok(
            &router,
            "DescribeLocationEfs",
            serde_json::json!({ "LocationArn": arn }),
            case,
        )
        .await;
        assert_eq!(
            again, described,
            "case {case:?}: two reads of one location must agree, got {again} then {described}"
        );
    }
}

/// Two requests carrying the same file system address two independent locations, each
/// reporting its own ARN: nothing about a location is derived from a hash of its request.
#[tokio::test]
async fn duplicate_efs_configurations_are_independent_locations() {
    let router = service();
    let mut arns: Vec<String> = Vec::new();

    for row in &rows() {
        let case = row.case;
        for attempt in 0..3 {
            let created = ok(&router, "CreateLocationEfs", request_of(row), case).await;
            let arn = string(&created, "LocationArn", case);
            assert_location_arn(&arn, case);
            assert!(
                !arns.contains(&arn),
                "case {case:?}: attempt {attempt} must mint a new location, got {arn:?} again"
            );
            arns.push(arn.clone());

            let described = ok(
                &router,
                "DescribeLocationEfs",
                serde_json::json!({ "LocationArn": arn }),
                case,
            )
            .await;
            assert_eq!(
                string(&described, "LocationArn", case),
                arn,
                "case {case:?}: attempt {attempt} must report its own ARN, got {described}"
            );
            assert_eq!(
                string(&described, "LocationUri", case),
                row.expected_uri,
                "case {case:?}: attempt {attempt} must report its own file system, \
                 got {described}"
            );
        }
    }
}

/// A restricted mount is refused unless the request asks for TLS 1.2.
///
/// `InTransitEncryption`'s own documentation states the rule: an `AccessPointArn` or a
/// `FileSystemAccessRoleArn` makes `TLS1_2` the only accepted value, so each way of
/// leaving it unset or setting it to `NONE` is a client error.
#[tokio::test]
async fn a_restricted_efs_mount_requires_tls_in_transit() {
    let router = service();
    let region = "us-east-1";
    let rule = "you must set InTransitEncryption to TLS1_2";

    let base = serde_json::json!({
        "EfsFilesystemArn": filesystem_arn(region, "fs-01234567"),
        "Ec2Config": {
            "SubnetArn": subnet_arn(region, "subnet-0123456789abcdef0"),
            "SecurityGroupArns": security_group_arns(region, &["sg-0123456789abcdef1"]),
        },
    });

    let restricted: [(&str, serde_json::Value); 2] = [
        (
            "access point",
            serde_json::json!(access_point_arn(region, "fsap-0123456789abcdef0")),
        ),
        (
            "IAM role",
            serde_json::json!(FILE_SYSTEM_ACCESS_ROLE_ARN),
        ),
    ];

    for (named, value) in restricted {
        let member = if named == "access point" {
            "AccessPointArn"
        } else {
            "FileSystemAccessRoleArn"
        };
        for encryption in [None, Some(NO_TLS)] {
            let case = format!("{named} with encryption {encryption:?}");
            let mut request = base.clone();
            request[member] = value.clone();
            if let Some(encryption) = encryption {
                request["InTransitEncryption"] = serde_json::json!(encryption);
            }
            invalid_request(&router, "CreateLocationEfs", request, rule, &case).await;
        }

        // The same request with TLS 1.2 is accepted, so the refusal is about the
        // encryption and nothing else.
        let case = format!("{named} with encryption TLS1_2");
        let mut request = base.clone();
        request[member] = value.clone();
        request["InTransitEncryption"] = serde_json::json!(TLS);
        let created = ok(&router, "CreateLocationEfs", request, &case).await;
        assert_location_arn(&string(&created, "LocationArn", &case), &case);
    }
}

/// A location is reported by the describe of its own kind, and by no other; an ARN this
/// service never minted resolves to nothing.
#[tokio::test]
async fn an_efs_location_is_resolved_by_arn_and_by_kind() {
    let router = service();
    let case = "kind separation";
    let region = "us-east-1";

    let never_minted =
        format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:location/loc-ffffffffffffffffa");
    invalid_request(
        &router,
        "DescribeLocationEfs",
        serde_json::json!({ "LocationArn": never_minted }),
        &format!("Location {never_minted} is not found."),
        "unknown location",
    )
    .await;

    let efs = string(
        &ok(
            &router,
            "CreateLocationEfs",
            serde_json::json!({
                "EfsFilesystemArn": filesystem_arn(region, "fs-0abcdef1"),
                "Ec2Config": {
                    "SubnetArn": subnet_arn(region, "subnet-0abcdef1234567890"),
                    "SecurityGroupArns": security_group_arns(region, &["sg-0abcdef1234567891"]),
                },
            }),
            case,
        )
        .await,
        "LocationArn",
        case,
    );
    let s3 = string(
        &ok(
            &router,
            "CreateLocationS3",
            serde_json::json!({
                "S3BucketArn": "arn:aws:s3:::other-kind-bucket",
                "S3Config": { "BucketAccessRoleArn": "arn:aws:iam::000000000000:role/datasync" },
            }),
            case,
        )
        .await,
        "LocationArn",
        case,
    );

    invalid_request(
        &router,
        "DescribeLocationEfs",
        serde_json::json!({ "LocationArn": s3 }),
        &format!("Location {s3} is not an Amazon EFS location."),
        "an S3 location read as Amazon EFS",
    )
    .await;
    invalid_request(
        &router,
        "DescribeLocationS3",
        serde_json::json!({ "LocationArn": efs }),
        &format!("Location {efs} is not an Amazon S3 location."),
        "an Amazon EFS location read as S3",
    )
    .await;
}

/// A transfer task can be built from Amazon EFS locations: `CreateTask` resolves a
/// stored EFS location like any other kind.
#[tokio::test]
async fn efs_locations_can_be_both_ends_of_a_task() {
    let case = "efs to efs";
    let router = service();
    let region = "us-west-2";

    let location = |filesystem_id: &'static str, subdirectory: &'static str| {
        serde_json::json!({
            "EfsFilesystemArn": filesystem_arn(region, filesystem_id),
            "Subdirectory": subdirectory,
            "Ec2Config": {
                "SubnetArn": subnet_arn(region, "subnet-0123456789abcdef0"),
                "SecurityGroupArns": security_group_arns(region, &["sg-0123456789abcdef1"]),
            },
        })
    };

    let source = string(
        &ok(
            &router,
            "CreateLocationEfs",
            location("fs-aaaa1111", "/outbox"),
            case,
        )
        .await,
        "LocationArn",
        case,
    );
    let destination = string(
        &ok(
            &router,
            "CreateLocationEfs",
            location("fs-bbbb2222", "/inbox"),
            case,
        )
        .await,
        "LocationArn",
        case,
    );

    let created = ok(
        &router,
        "CreateTask",
        serde_json::json!({
            "SourceLocationArn": source,
            "DestinationLocationArn": destination,
            "Name": "efs-task",
        }),
        case,
    )
    .await;
    let task_arn = string(&created, "TaskArn", case);

    let described = ok(
        &router,
        "DescribeTask",
        serde_json::json!({ "TaskArn": task_arn }),
        case,
    )
    .await;
    assert_eq!(
        string(&described, "SourceLocationArn", case),
        source,
        "case {case:?}: expected the Amazon EFS source the task was created with, got {described}"
    );
    assert_eq!(
        string(&described, "DestinationLocationArn", case),
        destination,
        "case {case:?}: expected the Amazon EFS destination the task was created with, \
         got {described}"
    );
}

// ---------------------------------------------------------------------------------------
// What DataSync proves about the FILE SYSTEM before it stores an Amazon EFS location.
// ---------------------------------------------------------------------------------------

/// A second Region, so the rule is exercised where the file system does NOT live in the
/// Region the call was signed for: the ARN names the Region holding it.
const OTHER_REGION: &str = "eu-west-1";

/// An account that is not this emulator's, for a reference no Amazon EFS of this emulator
/// is asked about.
const FOREIGN_ACCOUNT: &str = "123456789012";

/// The class discriminator both code members of a refused file system reference carry.
const EFS_FILESYSTEM_INVALID: &str = "EfsFilesystemInvalid";

/// This service and this emulator's own Amazon EFS — the owner of file systems — mounted on
/// ONE engine, with the in-process callback the DataSync handler resolves a reference over.
struct WithAmazonEfs {
    router: MultiServiceRouter,
}

impl WithAmazonEfs {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_datasync_fmrsservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// An Amazon EFS client for `region` over the router's own in-process callback — the very
    /// seam the DataSync handler resolves a file system through. No socket is opened: the
    /// endpoint URL is only the address that transport routes by.
    fn efs(&self, region: &str) -> ::aws_sdk_efs::Client {
        let callback =
            smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone());
        let config = ::aws_sdk_efs::Config::builder()
            .behavior_version(::aws_sdk_efs::config::BehaviorVersion::latest())
            .http_client(callback.http_client())
            .credentials_provider(callback.aws_credentials_provider())
            .region(::aws_sdk_efs::config::Region::new(region.to_string()))
            .endpoint_url(format!("https://elasticfilesystem.{region}.amazonaws.com"))
            .retry_config(::aws_sdk_efs::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_efs::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_efs::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_efs::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        ::aws_sdk_efs::Client::from_conf(config)
    }

    /// One file system Amazon EFS mints in `region`, reported as the two values that service
    /// answered with: its ARN and its id. Nothing here derives either.
    async fn create_file_system(&self, region: &str, token: &str) -> (String, String) {
        let created = self
            .efs(region)
            .create_file_system()
            .creation_token(token)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("Amazon EFS must create the file system {token}: {error:?}")
            });
        (
            created
                .file_system_arn()
                .unwrap_or_else(|| panic!("Amazon EFS must report the ARN of {token}"))
                .to_string(),
            created.file_system_id().to_string(),
        )
    }

    /// Remove a file system from Amazon EFS again.
    async fn delete_file_system(&self, region: &str, file_system_id: &str) {
        self.efs(region)
            .delete_file_system()
            .file_system_id(file_system_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("Amazon EFS must remove the file system {file_system_id}: {error:?}")
            });
    }

    /// Whether Amazon EFS still reports the file system `file_system_id` in `region`.
    async fn holds_file_system(&self, region: &str, file_system_id: &str) -> bool {
        self.efs(region)
            .describe_file_systems()
            .file_system_id(file_system_id)
            .send()
            .await
            .is_ok()
    }

    /// The location ARNs DataSync now holds, in creation order.
    async fn held_locations(&self, case: &str) -> Vec<String> {
        let listed = ok(&self.router, "ListLocations", serde_json::json!({}), case).await;
        listed["Locations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["LocationArn"].as_str().map(ToString::to_string))
            .collect()
    }
}

/// The `CreateLocationEfs` body for `arn`, with the required `Ec2Config` every case carries
/// unchanged — so the only thing a row varies is the file system reference.
fn create_request(arn: &str) -> serde_json::Value {
    serde_json::json!({
        "EfsFilesystemArn": arn,
        "Ec2Config": {
            "SubnetArn": subnet_arn(SIGNED_REGION, "subnet-0123456789abcdef0"),
            "SecurityGroupArns": security_group_arns(SIGNED_REGION, &["sg-0123456789abcdef1"]),
        },
    })
}

/// An `EfsFilesystemArn` of `account` in `region` for `file_system_id`, for the references
/// no file system was ever created under. The form is the member's own `@pattern`
/// (`^arn:(aws|aws-cn|…):elasticfilesystem:[a-z\-0-9]+:[0-9]{12}:file-system/fs-[0-9a-f]{8,40}$`).
fn filesystem_arn_of(region: &str, account: &str, file_system_id: &str) -> String {
    format!("arn:aws:elasticfilesystem:{region}:{account}:file-system/{file_system_id}")
}

/// How DataSync words a refusal of a file system reference Amazon EFS does not hold.
fn absent_file_system_message(arn: &str) -> String {
    format!("Invalid EFS filesystem, ARN: {arn}, reason: EFS filesystem doesn't exist")
}

/// What the service must answer one `CreateLocationEfs` request.
enum Expected {
    /// The request is accepted and a location is stored.
    Accepted,
    /// The request is refused, storing nothing.
    Refused,
}

/// RULE: an Amazon EFS location is a transfer endpoint on a file system of Amazon EFS, so
/// `CreateLocationEfs` resolves the file system its `EfsFilesystemArn` names against that
/// service — in the Region the ARN itself names — and refuses the request with
/// `InvalidRequestException` (400), storing nothing, when Amazon EFS answers that this
/// account holds no such file system. The refusal carries `EfsFilesystemInvalid` in BOTH
/// `errorCode` and `datasyncErrorCode` and the sentence "Invalid EFS filesystem, ARN: <arn>,
/// reason: EFS filesystem doesn't exist". A reference Amazon EFS was not asked about is not
/// refused on these grounds.
///
/// The rows differ along what the rule generalises over: two different file systems Amazon
/// EFS holds, so the rule is not keyed to one id; a file system held in a Region other than
/// the signed one; id widths at both ends of the modelled `fs-[0-9a-f]{8,40}` for references
/// nothing was created under, in either Region; a file system that was created and then
/// deleted; and a file system of another account, which is not this emulator's to hold.
#[tokio::test]
async fn create_location_efs_resolves_the_file_system_in_amazon_efs() {
    let aws = WithAmazonEfs::new();

    let (held_arn, held_id) = aws.create_file_system(SIGNED_REGION, "preflight-held").await;
    let (second_held_arn, _) = aws
        .create_file_system(SIGNED_REGION, "preflight-second")
        .await;
    let (other_region_arn, _) = aws
        .create_file_system(OTHER_REGION, "preflight-other-region")
        .await;
    let (deleted_arn, deleted_id) = aws
        .create_file_system(SIGNED_REGION, "preflight-deleted")
        .await;

    assert!(
        aws.holds_file_system(SIGNED_REGION, &held_id).await,
        "setup: Amazon EFS must report the file system it just minted, {held_id}"
    );
    aws.delete_file_system(SIGNED_REGION, &deleted_id).await;
    assert!(
        !aws.holds_file_system(SIGNED_REGION, &deleted_id).await,
        "setup: Amazon EFS must stop reporting the deleted file system {deleted_id}"
    );

    let account = engine::emulator_account_id();
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "a file system Amazon EFS holds",
            held_arn,
            Expected::Accepted,
        ),
        (
            "a second file system Amazon EFS holds",
            second_held_arn,
            Expected::Accepted,
        ),
        (
            "a file system held in a Region other than the signed one",
            other_region_arn,
            Expected::Accepted,
        ),
        (
            "the shortest id nothing was created under",
            filesystem_arn_of(SIGNED_REGION, account, "fs-0b52a0f5"),
            Expected::Refused,
        ),
        (
            "a 17-character id nothing was created under",
            filesystem_arn_of(SIGNED_REGION, account, "fs-0b52a0f59862d10c7"),
            Expected::Refused,
        ),
        (
            "the longest id nothing was created under",
            filesystem_arn_of(
                SIGNED_REGION,
                account,
                "fs-abcdef0123456789abcdef0123456789abcdef01",
            ),
            Expected::Refused,
        ),
        (
            "an id nothing was created under, in another Region",
            filesystem_arn_of(OTHER_REGION, account, "fs-00ff11ee"),
            Expected::Refused,
        ),
        (
            "a file system that was created and then deleted",
            deleted_arn,
            Expected::Refused,
        ),
        (
            // Another account's Amazon EFS is not this emulator's to ask, so nothing is
            // established about the file system and nothing is refused.
            "a file system of another account",
            filesystem_arn_of(SIGNED_REGION, FOREIGN_ACCOUNT, "fs-0b52a0f5"),
            Expected::Accepted,
        ),
    ];

    let mut accepted: Vec<String> = Vec::new();
    for (case, arn, expected) in rows {
        match expected {
            Expected::Accepted => {
                let created = ok(&aws.router, "CreateLocationEfs", create_request(&arn), case).await;
                let location_arn = string(&created, "LocationArn", case);
                assert_location_arn(&location_arn, case);
                accepted.push(location_arn);
            }
            Expected::Refused => {
                let (status, parsed) =
                    call(&aws.router, "CreateLocationEfs", create_request(&arn)).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected CreateLocationEfs to answer 400, got {status} \
                     with {parsed}"
                );
                let wire_type = parsed["__type"].as_str().unwrap_or_default();
                assert!(
                    wire_type.contains("InvalidRequestException"),
                    "case {case:?}: expected an InvalidRequestException, got {parsed}"
                );
                for member in ["errorCode", "datasyncErrorCode"] {
                    assert_eq!(
                        parsed[member].as_str(),
                        Some(EFS_FILESYSTEM_INVALID),
                        "case {case:?}: expected {member} to carry {EFS_FILESYSTEM_INVALID:?}, \
                         got {parsed}"
                    );
                }
                let expected_message = absent_file_system_message(&arn);
                assert_eq!(
                    parsed["message"].as_str(),
                    Some(expected_message.as_str()),
                    "case {case:?}: expected the refusal to say {expected_message:?}, \
                     got {parsed}"
                );
            }
        }
    }

    // Every refused create stored nothing: the listing holds exactly the accepted locations.
    let held = aws.held_locations("listing").await;
    assert_eq!(
        held, accepted,
        "case \"listing\": expected only the accepted locations, got {held:?}"
    );
}

/// RULE: an accepted reference is the one the location KEEPS, so a location created on a
/// file system Amazon EFS holds reads back through `DescribeLocationEfs` reporting that very
/// file system.
#[tokio::test]
async fn an_accepted_file_system_is_what_the_stored_location_reports() {
    let case = "round trip";
    let aws = WithAmazonEfs::new();
    let (arn, file_system_id) = aws
        .create_file_system(SIGNED_REGION, "preflight-roundtrip")
        .await;

    let created = ok(&aws.router, "CreateLocationEfs", create_request(&arn), case).await;
    let location_arn = string(&created, "LocationArn", case);

    let described = ok(
        &aws.router,
        "DescribeLocationEfs",
        serde_json::json!({ "LocationArn": location_arn }),
        case,
    )
    .await;
    let expected_uri = format!("efs://{SIGNED_REGION}.{file_system_id}/");
    assert_eq!(
        string(&described, "LocationUri", case),
        expected_uri,
        "case {case:?}: expected the file system the request named, got {described}"
    );
}

/// RULE: a refusal has to be grounded in what the OWNER of the resource answered, so an
/// emulator with no Amazon EFS mounted refuses nothing — the very references the cases above
/// are refused for are accepted here, because nothing was asked about them.
#[tokio::test]
async fn nothing_is_refused_when_amazon_efs_cannot_be_asked() {
    let router = service();
    let account = engine::emulator_account_id();

    // One reference per id width the rows above refuse, in both Regions.
    let arns = [
        filesystem_arn_of(SIGNED_REGION, account, "fs-0b52a0f5"),
        filesystem_arn_of(SIGNED_REGION, account, "fs-0b52a0f59862d10c7"),
        filesystem_arn_of(OTHER_REGION, account, "fs-00ff11ee"),
    ];
    for arn in arns {
        let case = format!("no Amazon EFS mounted, {arn}");
        let created = ok(&router, "CreateLocationEfs", create_request(&arn), &case).await;
        assert_location_arn(&string(&created, "LocationArn", &case), &case);
    }
}
