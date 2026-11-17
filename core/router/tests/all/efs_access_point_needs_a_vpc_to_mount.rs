// SPDX-License-Identifier: CC0-1.0

//! What it takes for LAMBDA to mount an access point EFS minted: the network the file
//! system sits on.
//!
//! RULE: an Amazon EFS file system is reachable only through a mount target inside a VPC,
//! so Lambda stores a function configuration that mounts an EFS access point only when
//! that same configuration puts the function inside a VPC — naming a subnet for its
//! network interface and a security group to govern it. A configuration that mounts one
//! without that attachment is refused with the modelled `InvalidParameterValueException`
//! (HTTP 400) and the sentence naming the access point out of reach, and NOTHING is
//! persisted: a following `GetFunction` answers `ResourceNotFoundException`, and an update
//! refused this way leaves the stored configuration exactly as it was. An access point of
//! an Amazon S3 file system is reached over its own service endpoint instead
//! (`FileSystemConfig$S3FilesConfig`), so it needs no attachment and is accepted without
//! one.
//!
//! The rule spans two services, which is why this suite lives here: every access point is
//! created through EFS's own `CreateAccessPoint` on the same engine that serves Lambda, so
//! what `CreateFunction` is handed is the ARN EFS really minted — one whose `fsap-` body
//! carries the 17 hex digits the consuming pattern pins
//! (`smithy_query(query="shape",shape="FileSystemConfig",model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`)
//! — and not a copy written here.
//!
//! The rows vary what the rule generalises over: which half of the attachment the
//! configuration names (neither, the subnets, the groups, both), how many of each, how the
//! request spells "no attachment" (an absent member, empty lists), where the function
//! mounts the access point, which service's access point it mounts, and whether the pair
//! arrives in one create or is assembled over two updates.
//!
//! The clock-owed halves — EFS's `creating` -> `available` move and Lambda's `Pending` ->
//! `Active` move — are reached by polling for the condition and by bringing the stored due
//! instant forward, so nothing here waits on the wall clock.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

use crate::efs_access_point_mounted_by_lambda::{
    access_point_arn, available_file_system, emulator_on, lambda, member, Answer,
};
use crate::lambda_signing_name_routing::{ACCOUNT, REGION, ROLE, ZIP_FILE};

/// The wire code of the modelled 400 a configuration Lambda will not store is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The wire code of the modelled 404 a read of a function the account does not hold answers.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// An access point of an Amazon S3 file system, the OTHER form `FileSystemConfig$Arn`
/// admits: `^arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}/access-point/fsap-[0-9a-f]{17,40}$`
/// (`smithy_query(query="shape",shape="FileSystemConfig",model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`).
/// It names no EFS resource, so no mount target and no VPC stand between the function and
/// it.
const S3_FILES_ACCESS_POINT: &str = "arn:aws:s3files:us-east-1:000000000000:file-system\
     /fs-0123456789abcdef0/access-point/fsap-0123456789abcdef0";

/// How Lambda words the refusal of a configuration that mounts an access point the function
/// has no network path to: fixed text apart from the ARN the request itself named.
fn refusal(access_point_arn: &str) -> String {
    format!(
        "Function must be configured to execute in a VPC to reference access point \
         {access_point_arn}. Please update the function configuration to include VPC subnets \
         and security groups."
    )
}

/// The unqualified ARN of a function of `name`, which a refused create's following read must
/// name.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The error sentence of an answer, whichever casing the wire carries it under.
fn message(answer: &Answer) -> String {
    let body = &answer.body;
    let worded = member(body, "message");
    if worded.is_empty() {
        member(body, "Message")
    } else {
        worded
    }
}

/// A `VpcConfig` naming `subnets` subnets and `groups` security groups. Zero of either is
/// the empty list, which is how a request spells "attach the function to nothing".
fn vpc_config(subnets: usize, groups: usize) -> Value {
    json!({
        "SubnetIds": (0..subnets)
            .map(|index| format!("subnet-{index:017x}"))
            .collect::<Vec<String>>(),
        "SecurityGroupIds": (0..groups)
            .map(|index| format!("sg-{index:017x}"))
            .collect::<Vec<String>>(),
    })
}

/// The `CreateFunction` body of a function named `name` mounting `arn` at `path`, with the
/// VPC configuration `vpc` (`None`: the request carries no such member at all).
fn create_body(name: &str, arn: &str, path: &str, vpc: Option<Value>) -> Value {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": ZIP_FILE },
        "FileSystemConfigs": [{ "Arn": arn, "LocalMountPath": path }],
    });
    if let Some(vpc) = vpc {
        body["VpcConfig"] = vpc;
    }
    body
}

/// `CreateFunction`.
async fn create_function(router: &MultiServiceRouter, body: Value) -> Answer {
    lambda(router, "POST", "/2015-03-31/functions", Some(body)).await
}

/// `GetFunction` on the function `name`.
async fn get_function(router: &MultiServiceRouter, name: &str) -> Answer {
    lambda(router, "GET", &format!("/2015-03-31/functions/{name}"), None).await
}

/// `GetFunctionConfiguration` on the function `name`.
async fn get_configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        None,
    )
    .await
}

/// `UpdateFunctionConfiguration` on the function `name`.
async fn update_configuration(router: &MultiServiceRouter, name: &str, patch: Value) -> Answer {
    lambda(
        router,
        "PUT",
        &format!("/2015-03-31/functions/{name}/configuration"),
        Some(patch),
    )
    .await
}

/// Bring every stored function's clock-owed instants forward, so the next call sees the
/// settled record a caller that waited would see: "During this time, you can't modify the
/// function" (`CreateFunction`). This is what waiting would do, without waiting.
fn make_modifiable(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
        if function.last_update_due.is_some() {
            function.last_update_due = Some(past);
        }
    }
}

/// One row of the create table: how the request spells the attachment, which access point it
/// mounts, where it mounts it, and whether Lambda stores it.
struct Row {
    case: &'static str,
    /// The VPC configuration the request carries, if any.
    vpc: Option<Value>,
    /// Whether the mount is the access point EFS minted; otherwise the Amazon S3 one.
    efs_mount: bool,
    path: &'static str,
    stored: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: an EFS access point mounted by a function the request
            // never puts in a VPC.
            case: "an EFS access point with no VpcConfig member at all",
            vpc: None,
            efs_mount: true,
            path: "/mnt/data",
            stored: false,
        },
        Row {
            case: "an EFS access point with both lists empty, which attaches nothing",
            vpc: Some(vpc_config(0, 0)),
            efs_mount: true,
            path: "/mnt/shared",
            stored: false,
        },
        Row {
            case: "an EFS access point with two subnets and no security group",
            vpc: Some(vpc_config(2, 0)),
            efs_mount: true,
            path: "/mnt/data-1",
            stored: false,
        },
        Row {
            case: "an EFS access point with a security group and no subnet",
            vpc: Some(vpc_config(0, 1)),
            efs_mount: true,
            path: "/mnt/x.y-z_1",
            stored: false,
        },
        Row {
            case: "an EFS access point with one subnet and one group",
            vpc: Some(vpc_config(1, 1)),
            efs_mount: true,
            path: "/mnt/data",
            stored: true,
        },
        Row {
            case: "an EFS access point with three subnets and two groups",
            vpc: Some(vpc_config(3, 2)),
            efs_mount: true,
            path: "/mnt/shared",
            stored: true,
        },
        Row {
            case: "an Amazon S3 file system access point with no VpcConfig member at all",
            vpc: None,
            efs_mount: false,
            path: "/mnt/objects",
            stored: true,
        },
        Row {
            case: "an Amazon S3 file system access point with both lists empty",
            vpc: Some(vpc_config(0, 0)),
            efs_mount: false,
            path: "/mnt/objects",
            stored: true,
        },
    ]
}

#[tokio::test]
async fn a_create_mounting_an_efs_access_point_needs_the_function_in_a_vpc() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    let file_system = available_file_system(&router, "lambda-mount-reachability").await;
    let minted = access_point_arn(&router, "the mounted access point", &file_system, "reach-1").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let arn = if row.efs_mount {
            minted.clone()
        } else {
            S3_FILES_ACCESS_POINT.to_string()
        };
        let name = format!("efs-reach-{index}");
        let configs = json!([{ "Arn": arn, "LocalMountPath": row.path }]);
        let created = create_function(
            &router,
            create_body(&name, &arn, row.path, row.vpc.clone()),
        )
        .await;

        if row.stored {
            assert_eq!(
                created.status, 201,
                "case {case:?}: expected CreateFunction to answer 201, got {} {:?} {}",
                created.status, created.code, created.body
            );
            assert_eq!(
                created.body["FileSystemConfigs"], configs,
                "case {case:?}: expected the mount {configs} to be stored, got {:?}",
                created.body["FileSystemConfigs"]
            );
            // ROUND TRIP: an independent read reports the same mount.
            let read = get_configuration(&router, &name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetFunctionConfiguration to answer 200, got {} {:?} {}",
                read.status, read.code, read.body
            );
            assert_eq!(
                read.body["FileSystemConfigs"], configs,
                "case {case:?}: expected the read to report the stored mount, got {:?}",
                read.body["FileSystemConfigs"]
            );
            continue;
        }

        assert_eq!(
            created.status, 400,
            "case {case:?}: expected CreateFunction to be refused with 400, got {} {:?} {}",
            created.status, created.code, created.body
        );
        assert!(
            created.code.contains(INVALID_PARAMETER),
            "case {case:?}: expected {INVALID_PARAMETER}, got {:?} in {}",
            created.code,
            created.body
        );
        let worded = message(&created);
        assert_eq!(
            worded,
            refusal(&arn),
            "case {case:?}: expected the sentence {:?}, got {worded:?}",
            refusal(&arn)
        );
        // The refusal is COMPLETE: no function was stored, so the read of it answers the
        // modelled 404 naming the ARN the request addressed.
        let read = get_function(&router, &name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create must store no function, got {} {:?} {}",
            read.status, read.code, read.body
        );
        assert!(
            read.code.contains(NOT_FOUND),
            "case {case:?}: expected {NOT_FOUND}, got {:?} in {}",
            read.code,
            read.body
        );
        let expected = format!("Function not found: {}", function_arn(&name));
        let worded = message(&read);
        assert_eq!(
            worded, expected,
            "case {case:?}: expected the sentence {expected:?}, got {worded:?}"
        );
    }
}

#[tokio::test]
async fn an_update_is_judged_on_the_pair_the_function_would_run_with() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    let file_system = available_file_system(&router, "lambda-mount-updates").await;
    let minted = access_point_arn(&router, "the updated access point", &file_system, "update-1").await;
    let mount = json!([{ "Arn": minted, "LocalMountPath": "/mnt/shared" }]);
    let attached = vpc_config(2, 1);

    // A function that mounts nothing and is in no VPC: a configuration Lambda stores.
    let name = "efs-update-reach";
    let created = create_function(
        &router,
        json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": ZIP_FILE },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected CreateFunction to answer 201, got {} {:?} {}",
        created.status, created.code, created.body
    );

    // (case, the patch, whether Lambda stores it)
    let steps: Vec<(&str, Value, bool)> = vec![
        (
            "the mount added to a function that is in no VPC",
            json!({ "FileSystemConfigs": mount }),
            false,
        ),
        (
            "the mount and the attachment in one request",
            json!({ "VpcConfig": attached, "FileSystemConfigs": mount }),
            true,
        ),
        (
            "the attachment taken away from a function that still mounts it",
            json!({ "VpcConfig": vpc_config(0, 0) }),
            false,
        ),
        (
            "the group taken away, leaving the subnets",
            json!({ "VpcConfig": vpc_config(2, 0) }),
            false,
        ),
        (
            "the mount and the attachment both taken away",
            json!({ "VpcConfig": vpc_config(0, 0), "FileSystemConfigs": [] }),
            true,
        ),
    ];

    for (case, patch, stored) in steps {
        make_modifiable(&engine);
        let before = get_configuration(&router, name).await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: expected the baseline read to answer 200, got {} {:?} {}",
            before.status, before.code, before.body
        );
        let updated = update_configuration(&router, name, patch).await;
        if stored {
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected UpdateFunctionConfiguration to answer 200, got {} {:?} \
                 {}",
                updated.status, updated.code, updated.body
            );
            continue;
        }
        assert_eq!(
            updated.status, 400,
            "case {case:?}: expected UpdateFunctionConfiguration to be refused with 400, got {} \
             {:?} {}",
            updated.status, updated.code, updated.body
        );
        assert!(
            updated.code.contains(INVALID_PARAMETER),
            "case {case:?}: expected {INVALID_PARAMETER}, got {:?} in {}",
            updated.code,
            updated.body
        );
        let worded = message(&updated);
        assert_eq!(
            worded,
            refusal(&minted),
            "case {case:?}: expected the sentence {:?}, got {worded:?}",
            refusal(&minted)
        );
        // The refusal is COMPLETE: the stored configuration is exactly what it was before.
        let after = get_configuration(&router, name).await;
        assert_eq!(
            (after.status, &after.body),
            (200, &before.body),
            "case {case:?}: a refused update must leave the configuration alone, got {} {}",
            after.status,
            after.body
        );
    }

    // What the accepted steps left behind: the function mounts nothing, which is what the
    // last patch asked for, and the read says so rather than the create's answer.
    make_modifiable(&engine);
    let settled = get_configuration(&router, name).await;
    assert_eq!(
        settled.body["FileSystemConfigs"],
        Value::Null,
        "expected the function to report no mount at all, got {}",
        settled.body["FileSystemConfigs"]
    );
}
