// SPDX-License-Identifier: CC0-1.0

//! Who may MOUNT the Amazon EFS file system a function configures: `CreateFileSystem`,
//! `CreateMountTarget` and `CreateAccessPoint` on Amazon EFS, `CreateRole`/`PutRolePolicy` on
//! IAM, `CreateVpc`/`CreateSubnet`/`CreateSecurityGroup` on Amazon EC2, then `CreateFunction`
//! with `FileSystemConfigs` and `Invoke`
//! (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! RULE: the mount an execution environment performs is a call an NFS client makes on the file
//! system, so it is authorized against the function's OWN execution role at the moment the
//! environment is built — a role the policies in force on it withhold
//! `elasticfilesystem:ClientMount` from mounts nothing, and the invocation is refused with the
//! modelled `EFSMountFailureException` at HTTP 403 rather than served by a run that reached the
//! file system anyway (Amazon EFS User Guide, "Using IAM to control file system data access";
//! `smithy_query(query="operation", operation="Invoke", model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`
//! binds that error at 403). The question is asked at every invocation, so a grant taken off a
//! role stops its mounts from then on and one put back heals them.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with Amazon
//! EFS, IAM and Amazon EC2 mounted beside Lambda on ONE engine: every file system, access
//! point and mount target is one Amazon EFS minted through its own API, every role and policy
//! one IAM persisted, and every VPC, subnet and security group one Amazon EC2 minted.
//!
//! The rows differ along what the rule generalises over: HOW the role states the grant (on the
//! file system's own ARN, on every resource, under an action wildcard), how it fails to state
//! it (another call of the same namespace, no statement of the namespace at all, the grant
//! scoped to ANOTHER file system, the grant overturned by an explicit `Deny`), WHICH access
//! point the refused mount names, and whether the function configures a file system at all.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::event_source_mapping_role_access::{allow, given_role, iam, logs_only};
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;
use crate::vpc_attachment_interfaces::{ec2_client, network};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where Amazon EFS binds the three creates this suite drives.
const FILE_SYSTEMS_URI: &str = "/2015-02-01/file-systems";
const MOUNT_TARGETS_URI: &str = "/2015-02-01/mount-targets";
const ACCESS_POINTS_URI: &str = "/2015-02-01/access-points";

/// The `Runtime` every function of this suite is configured with.
const RUNTIME: &str = "python3.13";

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it covers this test binary's own scheduling,
/// not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How long a poll loop here waits for a state the CLOCK owes before reporting that it never
/// arrived. Generous: it covers this test binary's own scheduling and never a span under test,
/// because nothing here asserts on how long a transition took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often those loops look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The identity the access points of this suite enforce (`PosixUser$Uid` / `$Gid`).
const ACCESS_POINT_UID: i64 = 1001;
const ACCESS_POINT_GID: i64 = 1001;

/// The error shape `Invoke` answers a mount it could not perform with, and the status the model
/// binds it at.
const MOUNT_FAILURE: &str = "EFSMountFailureException";
const MOUNT_FAILURE_STATUS: u16 = 403;

/// The IAM action namespace the calls an NFS client of an Amazon EFS file system makes are named
/// in, which is Amazon EFS's own.
const EFS_NAMESPACE: &str = "elasticfilesystem";

/// The name the one inline policy of every role here is written under, so a row that REPLACES a
/// role's grants writes over the document the role already carried.
const POLICY_NAME: &str = "efs-client-calls";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The modelled error shape this answer names, without the shape id's namespace — the
    /// `restJson1` envelope's `__type`, or the `x-amzn-errortype` header it resolves one from.
    fn code(&self) -> String {
        let raw = self.json()["__type"]
            .as_str()
            .map(str::to_string)
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_default();
        raw.rsplit('#').next().unwrap_or_default().to_string()
    }

    /// The `message` member of a refusal.
    fn message(&self) -> String {
        let document = self.json();
        document["message"]
            .as_str()
            .or_else(|| document["Message"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// A SigV4 credential scope naming a signing name in `us-east-1`. Nothing in it is verified:
/// the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/us-east-1/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Lambda, Amazon EFS, IAM and Amazon EC2 on ONE engine — exactly what `core/router`
/// assembles.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Dispatch one request at `host`, signed for `signing_name`.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("{signing_name}.us-east-1.amazonaws.com");
    let payload = body
        .map(|document| serde_json::to_vec(document).expect("the body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One Lambda request.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    send(router, "lambda", method, uri, body).await
}

/// One Amazon EFS request.
async fn efs(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    send(router, "elasticfilesystem", method, uri, body).await
}

/// Look again until `holds` answers, or until [`SETTLE_DEADLINE`] has passed — which fails the
/// suite, because the state it waits for is one the clock owes.
async fn poll_until<F, Fut>(what: &str, mut holds: F)
where
    F: FnMut() -> Fut,
    Fut: ::std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if holds().await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected that {what}, which never happened"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `available` Amazon EFS file system with a mount target in `subnet_id`, answering the id
/// and the ARN Amazon EFS minted for it.
async fn available_file_system(
    router: &MultiServiceRouter,
    token: &str,
    subnet_id: &str,
    group_id: &str,
) -> (String, String) {
    let created = efs(
        router,
        "POST",
        FILE_SYSTEMS_URI,
        Some(&json!({ "CreationToken": token })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFileSystem {token:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let answered = created.json();
    let file_system_id = answered["FileSystemId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFileSystem must report the id it minted: {}", created.text()))
        .to_string();
    let file_system_arn = answered["FileSystemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFileSystem must report the ARN it minted: {}", created.text()))
        .to_string();
    poll_until(
        &format!("file system {file_system_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("{FILE_SYSTEMS_URI}?FileSystemId={file_system_id}"),
                None,
            )
            .await;
            read.json()["FileSystems"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    let mounted = efs(
        router,
        "POST",
        MOUNT_TARGETS_URI,
        Some(&json!({
            "FileSystemId": file_system_id,
            "SubnetId": subnet_id,
            "SecurityGroups": [group_id],
        })),
    )
    .await;
    assert_eq!(
        mounted.status,
        200,
        "CreateMountTarget in {subnet_id:?} must succeed, got {} {}",
        mounted.status,
        mounted.text()
    );
    let mount_target_id = mounted.json()["MountTargetId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("CreateMountTarget must report the id it minted: {}", mounted.text())
        })
        .to_string();
    poll_until(
        &format!("mount target {mount_target_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("{MOUNT_TARGETS_URI}?MountTargetId={mount_target_id}"),
                None,
            )
            .await;
            read.json()["MountTargets"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    (file_system_id, file_system_arn)
}

/// One `available` access point of `file_system_id`, rooted at `root`, answering the ARN Amazon
/// EFS minted for it.
async fn available_access_point(
    router: &MultiServiceRouter,
    file_system_id: &str,
    token: &str,
    root: &str,
) -> String {
    let created = efs(
        router,
        "POST",
        ACCESS_POINTS_URI,
        Some(&json!({
            "ClientToken": token,
            "FileSystemId": file_system_id,
            "PosixUser": { "Uid": ACCESS_POINT_UID, "Gid": ACCESS_POINT_GID },
            "RootDirectory": {
                "Path": root,
                "CreationInfo": {
                    "OwnerUid": ACCESS_POINT_UID,
                    "OwnerGid": ACCESS_POINT_GID,
                    "Permissions": "0750",
                },
            },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "CreateAccessPoint {token:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let answered = created.json();
    let access_point_id = answered["AccessPointId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("CreateAccessPoint must report the id it minted: {}", created.text())
        })
        .to_string();
    poll_until(
        &format!("access point {access_point_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("{ACCESS_POINTS_URI}?AccessPointId={access_point_id}"),
                None,
            )
            .await;
            read.json()["AccessPoints"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    answered["AccessPointArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("CreateAccessPoint must report the ARN it minted: {}", created.text())
        })
        .to_string()
}

/// The statements every role here carries whatever it says about Amazon EFS: the log writes a
/// function makes, and the network-interface calls Lambda makes as the role to stand the
/// function's VPC attachment up. Neither says anything about mounting a file system.
fn attached_in_a_vpc() -> Vec<Value> {
    vec![
        logs_only(),
        allow(
            "ec2",
            &[
                "CreateNetworkInterface",
                "DescribeNetworkInterfaces",
                "DeleteNetworkInterface",
            ],
            "*",
        ),
    ]
}

/// The one inline policy document of a role: the statements above, plus the Amazon EFS ones the
/// row states.
fn role_statements(efs_statements: Vec<Value>) -> Value {
    let mut statements = attached_in_a_vpc();
    statements.extend(efs_statements);
    Value::Array(statements)
}

/// Replace the one inline policy of `role_name` with `efs_statements` beside the statements
/// every role here carries, which is how a row takes a grant off a role and puts it back.
async fn put_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    efs_statements: Vec<Value>,
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": role_statements(efs_statements),
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy on {role_name:?} must answer 200, got {status}: {written}"
    );
}

/// Create the probe function `name` as `role_arn`, mounting `access_point` at `mount_path` when
/// the row names one, and wait until Lambda reports it `Active`.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    mount: Option<(&str, &str)>,
    network: (&str, &str),
) {
    let (subnet_id, group_id) = network;
    let mut body = json!({
        "FunctionName": name,
        "Role": role_arn,
        "Runtime": RUNTIME,
        "Handler": "index.handler",
        "Timeout": FUNCTION_TIMEOUT_SECONDS,
        "MemorySize": 256,
        "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(&[(
            "index.py",
            PROBE_SOURCE,
        )])) },
    });
    if let Some((access_point, mount_path)) = mount {
        body["VpcConfig"] = json!({ "SubnetIds": [subnet_id], "SecurityGroupIds": [group_id] });
        body["FileSystemConfigs"] =
            json!([{ "Arn": access_point, "LocalMountPath": mount_path }]);
        body["Environment"] = json!({ "Variables": { "MOUNT_PATH": mount_path } });
    }
    let created = lambda(router, "POST", FUNCTIONS_URI, Some(&body)).await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    poll_until(&format!("function {name} is active"), || async {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        read.json()["State"].as_str() == Some("Active")
    })
    .await;
}

/// One `Invoke` of `name` with `event`, re-driven while the answer reports a bound this whole
/// test binary shares rather than what the invocation came to — `None` when every attempt did.
async fn invoked(
    router: &MultiServiceRouter,
    name: &str,
    event: &Value,
) -> Option<Answer> {
    let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
    let mut answer = lambda(router, "POST", &uri, Some(event)).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = lambda(router, "POST", &uri, Some(event)).await;
    }
    (!is_host_bound(answer.status, &answer.text())).then_some(answer)
}

/// The sentence a refused mount is worded with: AWS's fixed text, with the access point the
/// function's own `FileSystemConfigs` named substituted.
fn refusal_message(access_point_arn: &str) -> String {
    format!(
        "The function couldn't mount the Amazon EFS file system with access point \
         {access_point_arn}."
    )
}

/// Assert that `answer` is the modelled mount failure naming `access_point_arn`.
fn assert_mount_refused(case: &str, answer: &Answer, access_point_arn: &str) {
    assert_eq!(
        answer.status,
        MOUNT_FAILURE_STATUS,
        "case {case:?}: expected HTTP {MOUNT_FAILURE_STATUS}, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.code(),
        MOUNT_FAILURE,
        "case {case:?}: expected the error code {MOUNT_FAILURE}, got {:?} in {}",
        answer.code(),
        answer.text()
    );
    assert_eq!(
        answer.message(),
        refusal_message(access_point_arn),
        "case {case:?}: expected the message {:?}, got {:?}",
        refusal_message(access_point_arn),
        answer.message()
    );
    assert!(
        answer.header("x-amz-function-error").is_none(),
        "case {case:?}: no handler ran, so nothing reports a function error, got {:?}",
        answer.header("x-amz-function-error")
    );
}

/// RULE: a function mounts the access point it configured only while the policies in force on
/// its execution role grant `elasticfilesystem:ClientMount` on the file system behind it; one
/// they withhold it from mounts nothing and is refused the modelled mount failure at 403.
///
/// The rows span how a role can state that grant and how it can fail to.
#[tokio::test]
async fn only_a_role_granted_client_mount_mounts_the_access_point() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let net = network(&ec2, "the file system's own VPC", 41, 1).await;
    let subnet = net.subnet_ids.first().expect("one subnet").clone();
    let (file_system, file_system_arn) =
        available_file_system(&router, "efs-mount-grant", &subnet, &net.group_id).await;
    // A SECOND file system, whose ARN is what a grant scoped to the wrong file system names.
    let (_elsewhere, elsewhere_arn) =
        available_file_system(&router, "efs-mount-grant-other", &subnet, &net.group_id).await;

    // Two access points of the file system under test, so a refusal names the access point the
    // refused function itself configured and not one value this suite holds.
    let shared = available_access_point(&router, &file_system, "ap-grant-shared", "/export/shared")
        .await;
    let private =
        available_access_point(&router, &file_system, "ap-grant-private", "/export/private").await;

    // (case, the Amazon EFS statements of the role, the access point the function mounts,
    //  whether the mount is performed).
    let rows: Vec<(&str, Vec<Value>, &String, bool)> = vec![
        (
            "ClientMount and ClientWrite on the file system's own ARN",
            vec![allow(
                EFS_NAMESPACE,
                &["ClientMount", "ClientWrite"],
                &file_system_arn,
            )],
            &shared,
            true,
        ),
        (
            "ClientMount alone, on every resource",
            vec![allow(EFS_NAMESPACE, &["ClientMount"], "*")],
            &shared,
            true,
        ),
        (
            "the grant under an action wildcard",
            vec![allow(EFS_NAMESPACE, &["Client*"], &file_system_arn)],
            &shared,
            true,
        ),
        (
            "ClientWrite only, with ClientMount absent",
            vec![allow(EFS_NAMESPACE, &["ClientWrite"], &file_system_arn)],
            &shared,
            false,
        ),
        (
            "no statement of the Amazon EFS namespace at all",
            Vec::new(),
            &shared,
            false,
        ),
        (
            "ClientMount granted on ANOTHER file system",
            vec![allow(
                EFS_NAMESPACE,
                &["ClientMount", "ClientWrite"],
                &elsewhere_arn,
            )],
            &private,
            false,
        ),
        (
            "ClientMount granted and explicitly denied",
            vec![
                allow(
                    EFS_NAMESPACE,
                    &["ClientMount", "ClientWrite"],
                    &file_system_arn,
                ),
                json!({
                    "Effect": "Deny",
                    "Action": [format!("{EFS_NAMESPACE}:ClientMount")],
                    "Resource": file_system_arn,
                }),
            ],
            &private,
            false,
        ),
    ];

    let mut read = 0;
    for (index, (case, statements, access_point, mounts)) in rows.into_iter().enumerate() {
        let role_name = format!("efs-mount-grant-{index}");
        let role = given_role(&router, case, &role_name, Some(role_statements(statements))).await;
        let function = format!("efs-mount-grant-fn-{index}");
        create_function(
            &router,
            &function,
            &role,
            Some((access_point, "/mnt/shared")),
            (&subnet, &net.group_id),
        )
        .await;
        let Some(answer) = invoked(&router, &function, &json!({ "case": "report" })).await else {
            continue;
        };
        read += 1;
        if mounts {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected the invocation to be served, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.json(),
                json!({
                    "mount": "/mnt/shared",
                    "ismount": true,
                    "isdir": true,
                    "uid": ACCESS_POINT_UID,
                    "gid": ACCESS_POINT_GID,
                }),
                "case {case:?}: expected the access point to be mounted as its own PosixUser, \
                 got {}",
                answer.text()
            );
        } else {
            assert_mount_refused(case, &answer, access_point);
        }
    }
    assert!(
        read > 0,
        "expected at least one row to be answered by an invocation, got {read}: every one \
         reported a bound this whole test binary shares, so nothing of the rule was read"
    );
}

/// RULE, the other half: the grant is read at every invocation, so taking
/// `elasticfilesystem:ClientMount` off one function's role refuses THAT function's next
/// invocation while another function of the same file system keeps mounting it, and putting the
/// grant back heals it — the bytes the mount holds are still the file system's.
#[tokio::test]
async fn the_mount_follows_the_grant_a_role_holds_at_the_invocation() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let net = network(&ec2, "the shared file system's VPC", 43, 1).await;
    let subnet = net.subnet_ids.first().expect("one subnet").clone();
    let (file_system, file_system_arn) =
        available_file_system(&router, "efs-mount-revoke", &subnet, &net.group_id).await;
    let shared =
        available_access_point(&router, &file_system, "ap-revoke-shared", "/export/lambda").await;

    let mounting = vec![allow(
        EFS_NAMESPACE,
        &["ClientMount", "ClientWrite"],
        &file_system_arn,
    )];
    let writing_only = vec![allow(EFS_NAMESPACE, &["ClientWrite"], &file_system_arn)];

    // Two functions of ONE access point, each with its own execution role: the role of the
    // second is the one the grant is taken off, so the first is the control.
    let keeper_role = given_role(&router, "the role that keeps the grant", "efs-keeper",
        Some(role_statements(mounting.clone()))).await;
    // The subject's grants are written by [`put_role_policy`] alone, so each leg's document
    // REPLACES the one before it rather than being read beside it.
    let subject_role =
        given_role(&router, "the role the grant is taken off", "efs-subject", None).await;
    put_role_policy(&router, "the role as created", "efs-subject", mounting.clone()).await;
    create_function(
        &router,
        "efs-keeper-fn",
        &keeper_role,
        Some((&shared, "/mnt/shared")),
        (&subnet, &net.group_id),
    )
    .await;
    create_function(
        &router,
        "efs-subject-fn",
        &subject_role,
        Some((&shared, "/mnt/data")),
        (&subnet, &net.group_id),
    )
    .await;

    // The bytes under test are written through the mount, so what the legs below read is the
    // FILE SYSTEM's and not a value this suite put anywhere.
    let text = "the bytes one function wrote\n";
    let Some(written) = invoked(
        &router,
        "efs-keeper-fn",
        &json!({ "case": "write", "name": "shared.txt", "text": text }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        written.status,
        200,
        "the write must be served, got {} {}",
        written.status,
        written.text()
    );

    // (case, the Amazon EFS statements the subject's role carries from here on, what its next
    //  invocation must come to).
    let legs: Vec<(&str, Vec<Value>, bool)> = vec![
        ("the grant as created", mounting.clone(), true),
        ("ClientMount taken off the role", writing_only, false),
        ("the grant put back", mounting, true),
    ];
    let mut read = 0;
    for (case, statements, mounts) in legs {
        put_role_policy(&router, case, "efs-subject", statements).await;
        let read_event = json!({ "case": "read", "name": "shared.txt" });
        let Some(answer) = invoked(&router, "efs-subject-fn", &read_event).await else {
            continue;
        };
        read += 1;
        if mounts {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected the invocation to be served, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.json(),
                json!({ "text": text, "ismount": true }),
                "case {case:?}: expected the bytes the other function wrote, got {}",
                answer.text()
            );
        } else {
            assert_mount_refused(case, &answer, &shared);
        }
        // The CONTROL, in the same window: the function whose role kept the grant still mounts
        // the same file system and reads the same bytes.
        let Some(control) = invoked(
            &router,
            "efs-keeper-fn",
            &json!({ "case": "read", "name": "shared.txt" }),
        )
        .await
        else {
            continue;
        };
        assert_eq!(
            control.json(),
            json!({ "text": text, "ismount": true }),
            "case {case:?}: the function whose role kept the grant still reads the file \
             system, got {} {}",
            control.status,
            control.text()
        );
    }
    assert!(
        read > 0,
        "expected at least one leg to be answered by an invocation, got {read}: every one \
         reported a bound this whole test binary shares, so nothing of the rule was read"
    );
}

/// RULE: the mount is what the grant governs, so a function that configures NO file system is
/// served whatever its role says about Amazon EFS.
#[tokio::test]
async fn a_function_that_mounts_nothing_needs_no_mount_grant() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let net = network(&ec2, "a VPC nothing of this case mounts from", 45, 1).await;
    let subnet = net.subnet_ids.first().expect("one subnet").clone();
    let case = "a role with no Amazon EFS grant, and a function that mounts nothing";
    let role = given_role(&router, case, "efs-unmounted", Some(role_statements(Vec::new()))).await;
    create_function(
        &router,
        "efs-unmounted-fn",
        &role,
        None,
        (&subnet, &net.group_id),
    )
    .await;
    // This case has one row, so skipping it would be a test that asserted nothing: the
    // invocation is made again for as long as [`SETTLE_DEADLINE`] allows while the answer
    // reports a bound this whole binary shares, and only a deadline reached with no run at all
    // fails it.
    let mut answered = None;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    while answered.is_none() {
        answered = invoked(&router, "efs-unmounted-fn", &json!({ "case": "ping" })).await;
        assert!(
            answered.is_some() || Instant::now() < deadline,
            "case {case:?}: every invocation reported a bound this whole test binary shares, \
             so nothing of the rule was read"
        );
    }
    let answer = answered.expect("the loop ends with an answer or fails");
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the invocation to be served, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.json(),
        json!({ "ran": true }),
        "case {case:?}: expected the handler's own answer, got {}",
        answer.text()
    );
}

/// The probe every row drives: its handler reports the mount its own configuration named, and
/// reads and writes under it.
const PROBE_SOURCE: &str = r#"
import os

MOUNT = os.environ.get("MOUNT_PATH", "")


def handler(event, context):
    case = event["case"]
    if case == "ping":
        return {"ran": True}
    if case == "report":
        return {
            "mount": MOUNT,
            "ismount": os.path.ismount(MOUNT),
            "isdir": os.path.isdir(MOUNT),
            "uid": os.getuid(),
            "gid": os.getgid(),
        }
    path = os.path.join(MOUNT, event["name"])
    if case == "write":
        with open(path, "w") as stream:
            stream.write(event["text"])
        return {"written": True, "ismount": os.path.ismount(MOUNT)}
    if case == "read":
        with open(path) as stream:
            return {"text": stream.read(), "ismount": os.path.ismount(MOUNT)}
    return {"unknown": case}
"#;
