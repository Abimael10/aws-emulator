// SPDX-License-Identifier: CC0-1.0

//! WHEN SageMaker resolves the AI workload configuration a benchmark job references, against
//! when it resolves the IAM role that job would run as.
//!
//! The rule under test: `CreateAIBenchmarkJob` resolves the reference its
//! `AIWorkloadConfigIdentifier` makes to this service's own store BEFORE it asks this
//! account's IAM about the role the job would run as, so a request naming a configuration the
//! account does not hold is refused with the modelled `ResourceNotFound` — quoting the
//! identifier exactly as the request wrote it — whatever IAM would have answered about the
//! role; only a request whose reference resolves goes on to the role, which is refused 400
//! `ValidationException` with this family's own sentence, and a request that passes both is
//! accepted and stored.
//!
//! The rule spans the two resolutions, and the second one reaches ANOTHER service (IAM)
//! through the in-process callback, so a single-service router has nothing to ask: this suite
//! needs the whole router, which is why it lives here rather than in the SageMaker crate.
//! The sibling suite `sagemaker_execution_role_admission` drives the role rule itself over
//! every create family, with the reference seeded so it cannot mask the role; this one drives
//! the PRECEDENCE of the two, which is only observable when both would refuse.
//!
//! The rows span what the rule generalises over: whether the account holds the referenced
//! configuration, the form the reference takes (a bare name, a hyphenated name, the
//! configuration's ARN), and what IAM answers about the role (a role it holds that trusts
//! SageMaker, one it holds that trusts another service only, and one it holds nothing under).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

use crate::sagemaker_create_admission::{
    create_role, error_code, error_message, sagemaker, trust_policy, RoleFixture, ACCOUNT,
    BOOT_STACK_SIZE,
};

/// The region every request below is signed for, which is the one the ARNs carry.
const REGION: &str = "us-east-1";

/// The AI workload configuration this account DOES hold, created through the service's own
/// `CreateAIWorkloadConfig`.
const HELD_CONFIG: &str = "benchorderconfig";

/// The inference endpoint every `BenchmarkTarget` below names. `CreateAIBenchmarkJob`
/// resolves the target against the endpoint store, so it is stood up in the seed: what this
/// suite asserts is the precedence of the reference over the role, which a target the account
/// does not hold would mask.
const TARGET_ENDPOINT: &str = "benchorderendpoint";

/// The role IAM holds whose trust document admits the SageMaker service principal.
const TRUSTING_ROLE: &str = "benchorder-trusting";

/// The role IAM holds whose trust document admits another service only, so SageMaker cannot
/// assume it.
const OTHER_SERVICE_ROLE: &str = "benchorder-lambda-only";

/// A role name this account's IAM holds NOTHING under, which is the shape the recorded
/// missing-configuration call carries.
const ABSENT_ROLE: &str = "benchorder-never-created";

/// How this family words a `RoleArn` SageMaker cannot assume: fixed text carrying nothing
/// from the request (the sentence the recorded service answers a benchmark job's unassumable
/// role with).
const ROLE_REFUSAL: &str = "Unable to assume the provided RoleArn. Ensure the role exists and \
                            its trust policy allows this service to assume it.";

/// How a create of this family words a referenced AI workload configuration the account does
/// not hold: the entity, the identifier the REQUEST carried in single quotes, and the scope
/// the service searched.
fn config_not_found(identifier: &str) -> String {
    format!("AIWorkloadConfig '{identifier}' not found for this account.")
}

/// An IAM role ARN of this account.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The ARN form of an AI workload configuration reference, which
/// `AIWorkloadConfigIdentifier`'s modelled pattern admits beside a bare name.
fn config_arn(name: &str) -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:ai-workload-config/{name}")
}

/// What the emulator has to answer for one row.
#[derive(Debug, PartialEq, Eq)]
enum Expected {
    /// The modelled `ResourceNotFound`: the reference resolves to nothing.
    ReferenceNotFound,
    /// The role refusal: the reference resolved, and IAM withheld the role.
    RoleRefused,
    /// Accepted and stored.
    Created,
}

/// One `CreateAIBenchmarkJob` request: the reference it makes, the role it names, and what
/// the service has to answer.
struct Row {
    case: &'static str,
    /// The `AIWorkloadConfigIdentifier` the request carries, verbatim.
    identifier: String,
    /// The `RoleArn` the request carries.
    role_arn: String,
    expected: Expected,
}

/// The table. Rows vary along what the rule generalises over: whether the configuration the
/// reference names is held, the form of the reference (bare name, hyphenated name, ARN), and
/// what IAM answers about the role.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a configuration the account does not hold, with a role IAM holds nothing \
                   under",
            identifier: "benchordermissing".to_string(),
            role_arn: role_arn(ABSENT_ROLE),
            expected: Expected::ReferenceNotFound,
        },
        Row {
            case: "a configuration the account does not hold, named by ARN, with a role IAM \
                   holds nothing under",
            identifier: config_arn("benchordermissingbyarn"),
            role_arn: role_arn(ABSENT_ROLE),
            expected: Expected::ReferenceNotFound,
        },
        Row {
            case: "a hyphenated configuration name the account does not hold, with a role \
                   that trusts another service only",
            identifier: "bench-order-missing-9".to_string(),
            role_arn: role_arn(OTHER_SERVICE_ROLE),
            expected: Expected::ReferenceNotFound,
        },
        Row {
            case: "a configuration the account does not hold, with a role SageMaker may \
                   assume",
            identifier: "benchordermissingtoo".to_string(),
            role_arn: role_arn(TRUSTING_ROLE),
            expected: Expected::ReferenceNotFound,
        },
        Row {
            case: "a configuration the account holds, with a role IAM holds nothing under",
            identifier: HELD_CONFIG.to_string(),
            role_arn: role_arn(ABSENT_ROLE),
            expected: Expected::RoleRefused,
        },
        Row {
            case: "a configuration the account holds, named by ARN, with a role that trusts \
                   another service only",
            identifier: config_arn(HELD_CONFIG),
            role_arn: role_arn(OTHER_SERVICE_ROLE),
            expected: Expected::RoleRefused,
        },
        Row {
            case: "a configuration the account holds, with a role SageMaker may assume",
            identifier: HELD_CONFIG.to_string(),
            role_arn: role_arn(TRUSTING_ROLE),
            expected: Expected::Created,
        },
        Row {
            case: "a configuration the account holds, named by ARN, with a role SageMaker may \
                   assume",
            identifier: config_arn(HELD_CONFIG),
            role_arn: role_arn(TRUSTING_ROLE),
            expected: Expected::Created,
        },
    ]
}

/// Hold the two roles the rows name in this account's IAM, and stand up the AI workload
/// configuration and the inference endpoint the rows reference.
fn seed(router: &smithy_server_runtime::MultiServiceRouter, runtime: &tokio::runtime::Runtime) {
    for fixture in [
        RoleFixture {
            role_name: TRUSTING_ROLE,
            path: None,
            policy: trust_policy("Allow", "\"sts:AssumeRole\"", "\"sagemaker.amazonaws.com\""),
        },
        RoleFixture {
            role_name: OTHER_SERVICE_ROLE,
            path: None,
            policy: trust_policy("Allow", "\"sts:AssumeRole\"", "\"lambda.amazonaws.com\""),
        },
    ] {
        create_role(router, runtime, &fixture);
    }

    let trusting = role_arn(TRUSTING_ROLE);
    for (operation, body) in [
        (
            "CreateAIWorkloadConfig",
            json!({ "AIWorkloadConfigName": HELD_CONFIG }),
        ),
        (
            "CreateModel",
            json!({
                "ModelName": format!("{TARGET_ENDPOINT}-model"),
                "PrimaryContainer": {
                    "Image": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/infer:1"),
                },
                "ExecutionRoleArn": trusting,
            }),
        ),
        (
            "CreateEndpointConfig",
            json!({
                "EndpointConfigName": format!("{TARGET_ENDPOINT}-config"),
                "ProductionVariants": [{
                    "VariantName": "AllTraffic",
                    "ModelName": format!("{TARGET_ENDPOINT}-model"),
                    "InstanceType": "ml.m5.large",
                    "InitialInstanceCount": 1,
                }],
            }),
        ),
        (
            "CreateEndpoint",
            json!({
                "EndpointName": TARGET_ENDPOINT,
                "EndpointConfigName": format!("{TARGET_ENDPOINT}-config"),
            }),
        ),
    ] {
        let (status, answered) = sagemaker(router, runtime, operation, &body);
        assert!(
            status == 200,
            "setup: {operation}: expected 200, got {status} {answered}"
        );
    }
}

/// The request one row drives, under `name`.
fn create_body(name: &str, row: &Row) -> Value {
    json!({
        "AIBenchmarkJobName": name,
        "AIWorkloadConfigIdentifier": row.identifier,
        "RoleArn": row.role_arn,
        "BenchmarkTarget": { "Endpoint": { "Identifier": TARGET_ENDPOINT } },
        "OutputConfig": { "S3OutputLocation": "s3://bucket/benchmarks/" },
    })
}

/// RULE: the reference a benchmark job makes to an AI workload configuration is resolved
/// before the role the job would run as, so a configuration the account does not hold is
/// answered with the modelled `ResourceNotFound` quoting the identifier as sent, whatever IAM
/// answers about the role; a reference that resolves leaves the role to decide.
#[test]
fn a_missing_workload_configuration_is_answered_before_the_role() {
    std::thread::Builder::new()
        .name("sagemaker-benchmark-reference-before-role".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            seed(&router, &runtime);

            for (index, row) in rows().iter().enumerate() {
                let name = format!("benchorderjob{index}");
                let case = row.case;
                let (status, answered) =
                    sagemaker(&router, &runtime, "CreateAIBenchmarkJob", &create_body(&name, row));

                match row.expected {
                    Expected::ReferenceNotFound => {
                        assert_eq!(
                            (status, error_code(&answered).as_str()),
                            (400, "ResourceNotFound"),
                            "case {case:?}: an unresolvable AIWorkloadConfigIdentifier must be \
                             refused 400 ResourceNotFound, got {status} {answered}"
                        );
                        assert_eq!(
                            error_message(&answered),
                            config_not_found(&row.identifier),
                            "case {case:?}: expected the missing-configuration sentence quoting \
                             the identifier as sent, got {answered}"
                        );
                    }
                    Expected::RoleRefused => {
                        assert_eq!(
                            (status, error_code(&answered).as_str()),
                            (400, "ValidationException"),
                            "case {case:?}: a resolved reference leaves the role to refuse the \
                             request 400 ValidationException, got {status} {answered}"
                        );
                        assert_eq!(
                            error_message(&answered),
                            ROLE_REFUSAL,
                            "case {case:?}: expected this family's assume-role sentence, got \
                             {answered}"
                        );
                    }
                    Expected::Created => {
                        assert_eq!(
                            status, 200,
                            "case {case:?}: a resolved reference and an assumable role must be \
                             accepted, got {status} {answered}"
                        );
                        assert_eq!(
                            answered.get("AIBenchmarkJobArn").and_then(Value::as_str),
                            Some(
                                format!(
                                    "arn:aws:sagemaker:{REGION}:{ACCOUNT}:ai-benchmark-job/{name}"
                                )
                                .as_str()
                            ),
                            "case {case:?}: expected the job's own ARN, got {answered}"
                        );
                    }
                }

                // What the create did or did not store, read back through the service's own
                // reader: a refused create stores nothing, and an accepted one replays the
                // reference exactly as the request wrote it.
                let (read_status, read) = sagemaker(
                    &router,
                    &runtime,
                    "DescribeAIBenchmarkJob",
                    &json!({ "AIBenchmarkJobName": name }),
                );
                if row.expected == Expected::Created {
                    assert_eq!(
                        read_status, 200,
                        "case {case:?}: the accepted create must store the job, got \
                         {read_status} {read}"
                    );
                    assert_eq!(
                        read.get("AIWorkloadConfigIdentifier").and_then(Value::as_str),
                        Some(row.identifier.as_str()),
                        "case {case:?}: the stored job must carry the identifier as sent, got \
                         {read}"
                    );
                } else {
                    assert_eq!(
                        (read_status, error_code(&read).as_str()),
                        (400, "ResourceNotFound"),
                        "case {case:?}: a refused create must store nothing, got {read_status} \
                         {read}"
                    );
                }
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
