// SPDX-License-Identifier: CC0-1.0

//! Which refusal a batch read of what a SERVICE OWNS gives a value that is not an ARN of the
//! resource its member takes, driven end to end over the shared wire harness in
//! [`crate::ecs_wire`].
//!
//! The rule under test: the sentence such a refusal carries is decided by the KIND of
//! identifier the value claims to be, read from the value alone.
//!
//! * A value that claims to be an ARN of the member's own resource — an ECS ARN whose
//!   resource type is `service-deployment` for `DescribeServiceDeployments`,
//!   `service-revision` for `DescribeServiceRevisions` — was meant to be one, so it is
//!   refused as a malformed ARN of that resource ("The ServiceDeployment ARN is invalid."),
//!   whatever is wrong with the rest of its form.
//! * A value that claims no kind at all — a bare name, text that is no ARN, an ARN of
//!   another vendor, an ARN of another ECS resource (the SERVICE's own among them) — leaves
//!   the read only the identity a rollout and a revision hang off, the service's, and so is
//!   refused as the unusable service identifier it is ("The Service ARN provided is
//!   invalid.") — the one sentence this API publishes for a service identifier it cannot
//!   use, which `ListServiceDeployments` answers a request carrying none with
//!   ([`crate::operation_worded_refusals`]).
//!
//! Both members are documented as the ARN of what they name
//! (`DescribeServiceDeployments$serviceDeploymentArns`: "The ARN of the service deployment",
//! `DescribeServiceRevisions$serviceRevisionArns`: "The ARN of the service revision"), and
//! neither refusal echoes anything the request carried.
//!
//! Nothing here changes WHAT a well-formed ARN earns: each table closes with the stored ARN,
//! which is described, and an ARN of the same form naming nothing stored, which is the
//! `MISSING` failure of its own entry rather than a refusal. The same rule holds for the two
//! calls that ACT on a rollout by its ARN, asserted over their own tables in
//! [`crate::continue_service_deployment_validation`], so one value cannot be read as one kind
//! of identifier by one operation and another kind by another. Every case goes through the
//! same front door a client uses, so nothing reaches into the state struct.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, refusal, register_definition, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster the fixture's service lives in.
const CLUSTER: &str = "arnkinds";

/// The bridge-mode revision that service runs.
const FAMILY: &str = "arnkindsfamily";

/// The service whose rollout and revision the reads below address.
const SERVICE: &str = "arnkindssvc";

/// The one error code this service models for a request whose parameters it will not serve.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence a value that claims to be a service-deployment ARN of this service, and is
/// not a well-formed one, earns.
const DEPLOYMENT_ARN_INVALID: &str = "The ServiceDeployment ARN is invalid.";

/// The sentence a value that claims no kind of this service's resources earns, read as the
/// service identifier it would have to be.
const SERVICE_ARN_INVALID: &str = "The Service ARN provided is invalid.";

/// The `Failure$reason` for an identifier that names nothing: "MISSING — the described
/// resource could not be found".
const MISSING: &str = "MISSING";

/// The stored rollout's ARN and the ARN of the revision it targets, from a router holding one
/// cluster, one revision and one service.
async fn fixture() -> (MultiServiceRouter, String, String) {
    let case = "fixture";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    register_definition(&router, FAMILY, case).await;
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    let deployment = string(&created["service"], "currentServiceDeployment", case);
    let described = ok(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [deployment.clone()] }),
        case,
    )
    .await;
    let deployments = list(&described, "serviceDeployments", case);
    let rollout = deployments
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout, got {described}"));
    let revision = string(&rollout["targetServiceRevision"], "arn", case);
    (router, deployment, revision)
}

/// `arn` with its last segment replaced by as many zeros: an ARN of the same FORM naming
/// nothing this store minted, since the ids this service mints start above zero.
fn unminted(arn: &str) -> String {
    let (prefix, id) = arn
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("expected {arn} to end in its id"));
    format!("{prefix}/{}", "0".repeat(id.len()))
}

/// The values that claim no kind of this service's resources, which every such read refuses
/// alike — the service's own ARN among them, built from the cluster and the service the
/// fixture holds.
fn claims_no_kind() -> Vec<(&'static str, String)> {
    vec![
        ("a bare service name", SERVICE.to_string()),
        ("an empty value", String::new()),
        ("text that is no ARN at all", "not-an-arn".to_string()),
        (
            "the service's own ARN",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/{CLUSTER}/{SERVICE}"),
        ),
        (
            "the cluster's ARN",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{CLUSTER}"),
        ),
        (
            "a task ARN of this service",
            format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task/{CLUSTER}/0123456789abcdef0123456789abcdef"
            ),
        ),
        (
            "an ARN of another vendor",
            format!("arn:aws:sqs:{SIGNED_REGION}:{ACCOUNT}:a-queue"),
        ),
        (
            "an ARN with no resource type at all",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:/{CLUSTER}/{SERVICE}/abc"),
        ),
    ]
}

#[tokio::test]
async fn a_rollout_read_names_the_kind_of_identifier_the_value_claims_to_be() {
    let (router, deployment, _) = fixture().await;

    // A value that CLAIMS to be a rollout ARN of this service is refused as a malformed one,
    // whatever is wrong with the rest of its form: the id is no id this service mints, a
    // segment is missing, one is empty, there is one too many, or the resource is written
    // with a colon instead of a slash.
    let claims_the_kind: Vec<(&str, String)> = vec![
        (
            "an id of a width this service never mints",
            format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-deployment/{CLUSTER}/{SERVICE}/\
                 3f8b2c1e-0000-4a11-9d2e-7c6b5a4f3e21"
            ),
        ),
        (
            "no id segment",
            format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-deployment/{CLUSTER}/{SERVICE}"
            ),
        ),
        (
            "an empty cluster segment",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-deployment//{SERVICE}/abc"),
        ),
        (
            "one segment too many",
            format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-deployment/{CLUSTER}/{SERVICE}/\
                 00000000000000000001/extra"
            ),
        ),
        (
            "the resource written with a colon",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-deployment:{SERVICE}"),
        ),
    ];

    for (case, value) in claims_the_kind {
        let (code, message) = refusal(
            &router,
            "DescribeServiceDeployments",
            json!({ "serviceDeploymentArns": [value] }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, DEPLOYMENT_ARN_INVALID),
            "case {case:?}: expected the malformed rollout ARN sentence, got {code:?} with \
             {message:?}"
        );
    }

    for (case, value) in claims_no_kind() {
        let (code, message) = refusal(
            &router,
            "DescribeServiceDeployments",
            json!({ "serviceDeploymentArns": [value] }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, SERVICE_ARN_INVALID),
            "case {case:?}: expected the service-identifier sentence, got {code:?} with \
             {message:?}"
        );
    }

    // A refusal is for the FORM of the value, so one bad entry refuses the batch it came in
    // however many good ones sit beside it.
    let case = "a stored ARN beside a bare name";
    let (code, message) = refusal(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [deployment.clone(), SERVICE] }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (INVALID_PARAMETER, SERVICE_ARN_INVALID),
        "case {case:?}: expected the batch refused for the entry at fault, got {code:?} with \
         {message:?}"
    );

    // And an ARN of the form this service mints is resolved rather than refused: the stored
    // rollout is described, and one naming nothing stored is the MISSING failure of its own
    // entry.
    let case = "the stored rollout beside an unminted ARN of the same form";
    let absent = unminted(&deployment);
    let body = ok(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [deployment.clone(), absent.clone()] }),
        case,
    )
    .await;
    let described: Vec<String> = list(&body, "serviceDeployments", case)
        .iter()
        .map(|found| string(found, "serviceDeploymentArn", case))
        .collect();
    assert_eq!(
        described,
        vec![deployment],
        "case {case:?}: expected the stored rollout described, got {body}"
    );
    assert_eq!(
        list(&body, "failures", case),
        vec![json!({ "arn": absent, "reason": MISSING })],
        "case {case:?}: expected one MISSING failure naming the ARN the request sent, got \
         {body}"
    );
}

#[tokio::test]
async fn a_revision_read_names_the_kind_of_identifier_the_value_claims_to_be() {
    let (router, deployment, revision) = fixture().await;

    // The same values, on the sibling read whose member takes the ARN of a REVISION of a
    // service: each claims no kind of this service's resources, so each leaves only the
    // service's own identity to be read as. The rollout ARN joins them here: it is an ARN of
    // another ECS resource for this member.
    let mut rows = claims_no_kind();
    rows.push(("the rollout's own ARN", deployment));

    for (case, value) in rows {
        let (code, message) = refusal(
            &router,
            "DescribeServiceRevisions",
            json!({ "serviceRevisionArns": [value] }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, SERVICE_ARN_INVALID),
            "case {case:?}: expected the service-identifier sentence, got {code:?} with \
             {message:?}"
        );
    }

    // A value that claims the member's own kind is resolved: the stored revision is
    // described, and one naming nothing stored is the MISSING failure of its own entry.
    let case = "the stored revision beside an unminted ARN of the same form";
    let absent = unminted(&revision);
    let body = ok(
        &router,
        "DescribeServiceRevisions",
        json!({ "serviceRevisionArns": [revision.clone(), absent.clone()] }),
        case,
    )
    .await;
    let described: Vec<String> = list(&body, "serviceRevisions", case)
        .iter()
        .map(|found| string(found, "serviceRevisionArn", case))
        .collect();
    assert_eq!(
        described,
        vec![revision],
        "case {case:?}: expected the stored revision described, got {body}"
    );
    assert_eq!(
        list(&body, "failures", case),
        vec![json!({ "arn": absent, "reason": MISSING })],
        "case {case:?}: expected one MISSING failure naming the ARN the request sent, got \
         {body}"
    );
}
