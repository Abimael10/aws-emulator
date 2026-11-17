// SPDX-License-Identifier: CC0-1.0

//! What `CreateAutoMLJobV2` does with the execution role the job it would create RUNS AS.
//!
//! THE RULE UNDER TEST: a SageMaker create whose resource runs as a caller-supplied
//! execution role resolves that role against THIS account's IAM before it stores anything,
//! and refuses the call — storing nothing — exactly when IAM ANSWERED that SageMaker cannot
//! assume the role: this account holds no role under the name the ARN carries, or the trust
//! document of the role it does hold withholds `sts:AssumeRole` from the SageMaker service
//! principal. The refusal is HTTP 400 `ValidationException`, worded as the AutoML family
//! words that fact and naming the role ARN as the request sent it, and the job's own reader
//! then reports the name as not found. Any other answer is accepted and the job is stored
//! around the role it was given — including a role of ANOTHER account, whose IAM this
//! emulator is not the authority on, so nothing is learned about it and nothing is refused
//! for it.
//!
//! The rows span what the rule generalises over: whether IAM holds the role at all, which
//! account the ARN names, how the ARN addresses a held role (under its bare name or under a
//! path), the punctuation a role name carries, and what the stored trust document states —
//! its effect, the shape it states the action and the principal in (one string or a list),
//! the action it allows and the principal it allows it for.
//!
//! The role is a resource of ANOTHER service, reached through the in-process callback, so a
//! single-service router has nothing to ask and this suite needs the whole router. The
//! sibling suites drive the same rule over the other families: `sagemaker_create_admission`
//! over the job creates, and `sagemaker_execution_role_admission` over the families whose
//! recorded refusal is worded differently, the creates taking several roles, and the create
//! that answers on the resource instead. This one drives the AutoML family, which neither of
//! them drove, asserting against the very sentence the job family's suite states so the two
//! cannot drift apart.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

use crate::sagemaker_create_admission::{
    could_not_assume, create_role, error_code, error_message, sagemaker, str_field, trust_policy,
    RoleFixture, ACCOUNT, BOOT_STACK_SIZE, OTHER_ACCOUNT, REGION,
};

/// The service principal a SageMaker execution role's trust document has to admit, as
/// `CreateNotebookInstanceInput$RoleArn`'s doc names it ("you must grant the SageMaker AI
/// service principal (sagemaker.amazonaws.com) permissions to assume this role").
const SERVICE_PRINCIPAL: &str = "\"sagemaker.amazonaws.com\"";

/// The assume itself, as a trust document's `Action`.
const ASSUME_ROLE: &str = "\"sts:AssumeRole\"";

/// The `AutoMLJobStatus` member a job is accepted in, before its own run has reported an
/// outcome for the clock to materialise: the non-terminal status `CreateAutoMLJobV2`
/// registers a job in.
const ACCEPTED_STATUS: &str = "InProgress";

/// How `DescribeAutoMLJobV2` words a job this account holds no record of: the kind of
/// resource, then the name AS THE REQUEST SENT IT, quoted.
fn could_not_find_job(name: &str) -> String {
    format!("Could not find AutoML job \"{name}\".")
}

/// The roles this account's IAM is made to hold before the creates run: trust documents
/// differing in the shape AWS accepts for `Action` and `Principal.Service` (one string, or a
/// list), in their effect, in the action they allow and in the principal they allow it for,
/// plus a role under a path and a role whose NAME carries punctuation.
///
/// Every value travels in an `awsQuery` form body the harness writes as given, so none of
/// them carries `&`, `=`, `+` or `%`.
fn role_fixtures() -> Vec<RoleFixture> {
    vec![
        RoleFixture {
            role_name: "automl-exec",
            path: None,
            policy: trust_policy("Allow", ASSUME_ROLE, SERVICE_PRINCIPAL),
        },
        RoleFixture {
            role_name: "automl-exec-lists",
            path: None,
            policy: trust_policy(
                "Allow",
                "[\"sts:TagSession\",\"sts:AssumeRole\"]",
                "[\"glue.amazonaws.com\",\"sagemaker.amazonaws.com\"]",
            ),
        },
        RoleFixture {
            role_name: "automl-path-exec",
            path: Some("/service-role/"),
            policy: trust_policy("Allow", ASSUME_ROLE, SERVICE_PRINCIPAL),
        },
        RoleFixture {
            // `RoleName`'s own grammar admits `[\w.,@-]` among others, and the modelled
            // `RoleArn` pattern `^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`
            // (smithy_query operation=CreateAutoMLJobV2) carries it through unchanged.
            role_name: "automl.exec,team@x-y_1",
            path: None,
            policy: trust_policy("Allow", ASSUME_ROLE, SERVICE_PRINCIPAL),
        },
        RoleFixture {
            role_name: "automl-lambda-only",
            path: None,
            policy: trust_policy("Allow", ASSUME_ROLE, "\"lambda.amazonaws.com\""),
        },
        RoleFixture {
            role_name: "automl-tag-session-only",
            path: None,
            policy: trust_policy("Allow", "\"sts:TagSession\"", SERVICE_PRINCIPAL),
        },
        RoleFixture {
            role_name: "automl-denied",
            path: None,
            policy: trust_policy("Deny", ASSUME_ROLE, SERVICE_PRINCIPAL),
        },
    ]
}

/// One role value `CreateAutoMLJobV2` is driven with, and whether SageMaker accepts it.
struct RoleCase {
    case: &'static str,
    role_arn: String,
    accepted: bool,
}

/// The table: the role is the ONLY thing that differs between the requests below, so nothing
/// but the role rule can decide any row's outcome.
fn role_cases() -> Vec<RoleCase> {
    vec![
        RoleCase {
            case: "a held role whose trust document admits the SageMaker principal",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-exec"),
            accepted: true,
        },
        RoleCase {
            case: "a held role whose trust document states its action and principal as lists",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-exec-lists"),
            accepted: true,
        },
        RoleCase {
            case: "a held role addressed under the path IAM holds it under",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/service-role/automl-path-exec"),
            accepted: true,
        },
        RoleCase {
            case: "a held role whose name carries the punctuation the pattern admits",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl.exec,team@x-y_1"),
            accepted: true,
        },
        RoleCase {
            // Another account's IAM is not this emulator's to ask, so nothing is learned
            // about the role and the create is not refused for it.
            case: "a role of another account",
            role_arn: format!("arn:aws:iam::{OTHER_ACCOUNT}:role/automl-exec"),
            accepted: true,
        },
        RoleCase {
            case: "a name this account's IAM holds no role under",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-never-created"),
            accepted: false,
        },
        RoleCase {
            case: "a name under a path this account's IAM holds no role under",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/service-role/automl-never-created"),
            accepted: false,
        },
        RoleCase {
            case: "a held role whose trust document admits another service principal only",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-lambda-only"),
            accepted: false,
        },
        RoleCase {
            case: "a held role whose trust document admits the principal for another action",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-tag-session-only"),
            accepted: false,
        },
        RoleCase {
            case: "a held role whose trust document denies the assume to the principal",
            role_arn: format!("arn:aws:iam::{ACCOUNT}:role/automl-denied"),
            accepted: false,
        },
    ]
}

/// A minimal `CreateAutoMLJobV2` request for `name` running as `role_arn`: the members the
/// operation marks required (smithy_query operation=CreateAutoMLJobV2), with the tabular
/// problem configuration.
fn create_body(name: &str, role_arn: &str) -> Value {
    json!({
        "AutoMLJobName": name,
        "AutoMLJobInputDataConfig": [{
            "ChannelType": "training",
            "ContentType": "text/csv;header=present",
            "CompressionType": "None",
            "DataSource": {
                "S3DataSource": {
                    "S3DataType": "S3Prefix",
                    "S3Uri": "s3://autopilot-input/train/",
                },
            },
        }],
        "OutputDataConfig": { "S3OutputPath": "s3://autopilot-output/jobs/" },
        "AutoMLProblemTypeConfig": { "TabularJobConfig": { "TargetAttributeName": "y" } },
        "RoleArn": role_arn,
    })
}

/// The whole ARN `AutoMLJobArn` carries for a job of `name`: the service's own ARN, scoped
/// to the Region the request was signed for and to this account, whose resource is the job's
/// kind and the name the request gave it.
fn automl_job_arn(name: &str) -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:automl-job/{name}")
}

/// RULE: `CreateAutoMLJobV2` is refused — with nothing stored — exactly when this account's
/// IAM answers that SageMaker cannot assume the `RoleArn` the request carries, with the
/// AutoML family's sentence naming that role; every other answer is accepted and the job is
/// stored around the role it was given.
#[test]
fn an_automl_job_is_refused_exactly_when_iam_answers_its_role_cannot_be_assumed() {
    std::thread::Builder::new()
        .name("sagemaker-automl-role-admission".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            for fixture in role_fixtures() {
                create_role(&router, &runtime, &fixture);
            }

            for (index, role) in role_cases().iter().enumerate() {
                let name = format!("automlrole{index}");
                let (status, answered) = sagemaker(
                    &router,
                    &runtime,
                    "CreateAutoMLJobV2",
                    &create_body(&name, &role.role_arn),
                );
                let (read_status, read) = sagemaker(
                    &router,
                    &runtime,
                    "DescribeAutoMLJobV2",
                    &json!({ "AutoMLJobName": &name }),
                );
                let case = role.case;

                if role.accepted {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: a role IAM answers nothing against must be accepted, \
                         got {status} with {answered}"
                    );
                    let minted = str_field(&answered, "AutoMLJobArn");
                    assert_eq!(
                        minted,
                        automl_job_arn(&name),
                        "case {case:?}: the create must answer the ARN of the job it minted, \
                         got {answered}"
                    );
                    assert_eq!(
                        (
                            read_status,
                            str_field(&read, "AutoMLJobName"),
                            str_field(&read, "AutoMLJobArn"),
                            str_field(&read, "RoleArn"),
                        ),
                        (200, name.clone(), minted, role.role_arn.clone()),
                        "case {case:?}: the accepted create must store the job it answered \
                         for, around the role it was given, got {read_status} with {read}"
                    );
                    assert_eq!(
                        (
                            str_field(&read, "AutoMLJobStatus"),
                            read.get("FailureReason").cloned(),
                        ),
                        (ACCEPTED_STATUS.to_string(), None),
                        "case {case:?}: an accepted job is reported in the non-terminal status \
                         the service accepts it in, and has failed at nothing, got {read}"
                    );
                    continue;
                }

                assert_eq!(
                    (status, error_code(&answered), error_message(&answered)),
                    (
                        400,
                        "ValidationException".to_string(),
                        could_not_assume(&role.role_arn)
                    ),
                    "case {case:?}: the role refusal as SageMaker words it, got {status} with \
                     {answered}"
                );
                assert_eq!(
                    (read_status, error_code(&read), error_message(&read)),
                    (
                        400,
                        "ResourceNotFound".to_string(),
                        could_not_find_job(&name)
                    ),
                    "case {case:?}: a refused create leaves the name absent, got {read_status} \
                     with {read}"
                );
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}

/// RULE: the role decides the call and the refusal stores nothing, so a name a refused
/// create was made under is still FREE — the same request with a role IAM answers nothing
/// against is accepted and stored under that name, and only then does a second create under
/// it answer the modelled in-use refusal.
#[test]
fn a_name_a_refused_create_was_made_under_is_still_free() {
    std::thread::Builder::new()
        .name("sagemaker-automl-role-name-reuse".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            create_role(
                &router,
                &runtime,
                &RoleFixture {
                    role_name: "automl-reuse-lambda",
                    path: None,
                    policy: trust_policy("Allow", ASSUME_ROLE, "\"lambda.amazonaws.com\""),
                },
            );
            create_role(
                &router,
                &runtime,
                &RoleFixture {
                    role_name: "automl-reuse-exec",
                    path: None,
                    policy: trust_policy("Allow", ASSUME_ROLE, SERVICE_PRINCIPAL),
                },
            );
            let unassumable = format!("arn:aws:iam::{ACCOUNT}:role/automl-reuse-lambda");
            let assumable = format!("arn:aws:iam::{ACCOUNT}:role/automl-reuse-exec");
            let name = "automlreusedname";

            let (status, refused) = sagemaker(
                &router,
                &runtime,
                "CreateAutoMLJobV2",
                &create_body(name, &unassumable),
            );
            assert_eq!(
                (status, error_code(&refused), error_message(&refused)),
                (
                    400,
                    "ValidationException".to_string(),
                    could_not_assume(&unassumable)
                ),
                "case \"the unassumable role\": the role refusal as SageMaker words it, got \
                 {status} with {refused}"
            );

            let (status, created) = sagemaker(
                &router,
                &runtime,
                "CreateAutoMLJobV2",
                &create_body(name, &assumable),
            );
            assert_eq!(
                status, 200,
                "case \"the same name with an assumable role\": the name the refusal was made \
                 under must still be free, got {status} with {created}"
            );
            let (read_status, read) = sagemaker(
                &router,
                &runtime,
                "DescribeAutoMLJobV2",
                &json!({ "AutoMLJobName": name }),
            );
            assert_eq!(
                (read_status, str_field(&read, "RoleArn")),
                (200, assumable.clone()),
                "case \"the same name with an assumable role\": the stored job must carry the \
                 role it was created with, got {read_status} with {read}"
            );

            let (status, again) = sagemaker(
                &router,
                &runtime,
                "CreateAutoMLJobV2",
                &create_body(name, &assumable),
            );
            assert_eq!(
                (status, error_code(&again)),
                (400, "ResourceInUse".to_string()),
                "case \"the name a stored job holds\": a second create must be refused as in \
                 use, got {status} with {again}"
            );
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
