// SPDX-License-Identifier: CC0-1.0

//! What the DAEMON reads make of the ARNs their required members carry, and in which order a
//! daemon WRITE resolves the resources its own parameters name.
//!
//! The rules under test, each asserted over a table that spans its class rather than over the
//! one request that exposed it:
//!
//! * a listing of ONE resource's children SELECTS them rather than acting in the parent:
//!   `ListDaemonDeployments` answers HTTP 200 with the modelled list present and empty for
//!   every daemon ARN that resolves to no rollout — a cluster this account never held, a
//!   cluster that holds no such daemon — and keeps its 400s for a value that identifies no
//!   daemon of any account;
//! * a read whose required member is a LIST OF RESOURCE ARNS refuses a member that holds no
//!   usable ARN — an entry that is not shaped like that resource's ARN, and so could identify
//!   no resource of ANY account, or no entry at all — as the invalid parameter it is
//!   (`InvalidParameterException`, with the sentence the member's own read words), and keeps
//!   the `failures` channel for every entry that IS shaped like one and resolves to nothing
//!   here — a cluster this account never created, a cluster that holds no such daemon, an id
//!   nothing is stored under — answering HTTP 200 with one `MISSING` failure per such entry
//!   beside the resources the request did resolve;
//! * the ID in the last segment of such an ARN is MINTED by the service, so what a client is
//!   asked for is an id of the FORM it hands out rather than one this store holds: an id of
//!   that form the store holds nothing under is one `MISSING` failure of its own entry, while
//!   a value outside the form identifies no resource of any account and is refused with the
//!   rest of the request;
//! * a daemon write resolves the resources its own PARAMETERS name — the daemon task
//!   definition first, then the capacity providers — before the cluster and the daemon its
//!   `daemonArn` identifies, so a request naming a task definition that does not exist is
//!   refused about that task definition whatever the state of the cluster or the daemon.
//!
//! Every case drives the emulator through the shared wire harness in [`crate::ecs_wire`], the
//! same front door a client uses.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::managed_instances;
use crate::ecs_wire::{create_cluster, list, ok, refusal, service, string, ACCOUNT, SIGNED_REGION};

/// The image the daemon task definitions here run; irrelevant to every rule, so one value.
const IMAGE: &str = "public.ecr.aws/aws-observability/aws-otel-collector:latest";

/// The error code every refusal below that is about a PARAMETER carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// `DescribeDaemonDeployments`' sentence for a `daemonDeploymentArns` entry that could name
/// no rollout of this account.
const DEPLOYMENT_ARNS_INVALID: &str = "Request parameter(s) are invalid.";

/// `DescribeDaemonRevisions`' sentence for a `daemonRevisionArns` entry that could name no
/// revision of this account.
const REVISION_ARN_INVALID: &str = "The Daemon Revision ARN is invalid.";

/// An id of the form a daemon's deployments and revisions are numbered by — the UUID (five
/// hyphen-separated groups of 8, 4, 4, 4 and 12 hexadecimal digits) this service's own
/// `daemon-deployment`/`daemon-revision` ARNs carry — with `value` spelled into it, so two
/// rows of a table below differ along the id while both are of that form.
fn sequence_id(value: u64) -> String {
    format!("{value:08x}-0000-0000-0000-{value:012x}")
}

/// The ARN of a daemon of `cluster`.
fn daemon_arn(cluster: &str, name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon/{cluster}/{name}")
}

/// The ARN of a `resource_type` resource below a daemon, as this service writes one:
/// `arn:aws:ecs:<region>:<account>:<type>/<cluster>/<daemon>/<id>`.
fn below_daemon(resource_type: &str, cluster: &str, daemon: &str, id: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:{resource_type}/{cluster}/{daemon}/{id}")
}

/// Create a MANAGED INSTANCES capacity provider scoped to `cluster` and return its ARN:
/// "ECS Managed Daemons is only supported for Amazon ECS Managed Instances Capacity
/// Providers", so a daemon deploys onto nothing else.
async fn managed_provider(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateCapacityProvider",
        json!({
            "name": name,
            "cluster": cluster,
            "managedInstancesProvider": managed_instances(),
        }),
        case,
    )
    .await;
    string(&body["capacityProvider"], "capacityProviderArn", case)
}

/// Register one daemon task-definition revision of `family` and return its ARN.
async fn daemon_definition(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RegisterDaemonTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{ "name": "agent", "image": IMAGE, "memory": 128 }],
        }),
        case,
    )
    .await;
    string(&body, "daemonTaskDefinitionArn", case)
}

/// Create one daemon and answer its ARN with the ARN of the rollout the create started.
async fn create_daemon(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    provider: &str,
    definition: &str,
    case: &str,
) -> (String, String) {
    let body = ok(
        router,
        "CreateDaemon",
        json!({
            "clusterArn": cluster,
            "daemonName": name,
            "daemonTaskDefinitionArn": definition,
            "capacityProviderArns": [provider],
        }),
        case,
    )
    .await;
    (
        string(&body, "daemonArn", case),
        string(&body, "deploymentArn", case),
    )
}

/// `ListDaemonDeployments` for one daemon ARN, as the summaries it reports.
async fn list_deployments(
    router: &MultiServiceRouter,
    reference: &str,
    case: &str,
) -> Vec<Value> {
    let body = ok(
        router,
        "ListDaemonDeployments",
        json!({ "daemonArn": reference }),
        case,
    )
    .await;
    assert!(
        body["daemonDeployments"].is_array(),
        "case {case:?}: expected the modelled list to be present, got {body}"
    );
    list(&body, "daemonDeployments", case)
}

#[tokio::test]
async fn a_deployment_listing_answers_the_empty_page_a_daemon_arn_without_rollouts_selects() {
    let router = service();
    const CLUSTER: &str = "daemon-empty-listings";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-empty", CASE).await;
    let definition = daemon_definition(&router, "daemonemptylistings", CASE).await;
    // One daemon that DOES hold a rollout, so an empty page below is the ARN's own answer
    // and not this listing reporting nothing whatever it is asked.
    let (held, deployment) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        &provider,
        &definition,
        CASE,
    )
    .await;
    let listed = list_deployments(&router, &held, "the daemon the create made").await;
    assert_eq!(
        listed
            .iter()
            .map(|summary| string(summary, "daemonDeploymentArn", CASE))
            .collect::<Vec<String>>(),
        vec![deployment],
        "case {CASE:?}: expected the rollout the create started, got {listed:?}"
    );

    // The rows differ along WHY the ARN names no rollout: the cluster it carries, the daemon
    // within a cluster that is there, and a daemon of this cluster that was never created
    // under a name another daemon's name is a prefix of — none of which is an error path.
    let rows: [(&str, String); 4] = [
        (
            "a daemon ARN under a cluster this account never held",
            daemon_arn("daemon-nowhere", "agents"),
        ),
        (
            "a daemon ARN under a cluster that holds no such daemon",
            daemon_arn(CLUSTER, "nosuchdaemon"),
        ),
        (
            "a daemon name the held daemon's name is a prefix of",
            daemon_arn(CLUSTER, "agents-2"),
        ),
        (
            "a daemon ARN of a cluster whose name is the daemon's",
            daemon_arn("agents", CLUSTER),
        ),
    ];
    for (case, reference) in rows {
        let listed = list_deployments(&router, &reference, case).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the empty page the ARN selects, got {listed:?}"
        );
    }

    // ... and the 400s are kept for a value that identifies no daemon of any account. This
    // operation models no not-found error for a daemon, so it answers the invalid identifier
    // it was given.
    let unusable: [(&str, String); 5] = [
        ("a value that is no ARN at all", "agents".to_string()),
        (
            "an ARN of another service",
            format!("arn:aws:eks:{SIGNED_REGION}:{ACCOUNT}:daemon/{CLUSTER}/agents"),
        ),
        (
            "an ARN of another resource of this service",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{CLUSTER}"),
        ),
        (
            "a daemon ARN with no daemon name",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon/{CLUSTER}/"),
        ),
        (
            "a daemon ARN whose cluster is spelled outside the name rule",
            daemon_arn("not a cluster", "agents"),
        ),
    ];
    for (case, reference) in unusable {
        let (code, message) = refusal(
            &router,
            "ListDaemonDeployments",
            json!({ "daemonArn": reference }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                "ClientException",
                format!("Daemon not found: {reference}").as_str()
            ),
            "case {case:?}: expected the invalid identifier to be answered"
        );
    }
}

/// One daemon read whose required member is a list of ARNs: the operation, that member, the
/// resource type its entries name, and the sentence it words for an entry that could name no
/// resource of this account.
type ArnListRead = (&'static str, &'static str, &'static str, &'static str);

/// Every such read of the daemon surface, so each table below spans both of them.
const ARN_LIST_READS: [ArnListRead; 2] = [
    (
        "DescribeDaemonDeployments",
        "daemonDeploymentArns",
        "daemon-deployment",
        DEPLOYMENT_ARNS_INVALID,
    ),
    (
        "DescribeDaemonRevisions",
        "daemonRevisionArns",
        "daemon-revision",
        REVISION_ARN_INVALID,
    ),
];

#[tokio::test]
async fn a_daemon_read_refuses_an_arn_that_could_identify_no_resource_of_any_account() {
    let router = service();
    const CLUSTER: &str = "daemon-arn-rules";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;

    for (operation, member, resource_type, expected) in ARN_LIST_READS {
        // The rows differ along WHAT about the member makes it hold no usable ARN: its being
        // empty, an entry's not being an ARN, its partition/vendor fields, its resource type,
        // how many segments its resource path carries, the character set of the names in it,
        // and the form of the id it ends in. What an entry RESOLVES to here is NOT one of them
        // — neither the daemon it sits under nor which id of that form it carries — since an
        // entry of this shape names a resource of this account and is reported on the
        // `failures` channel when this store holds none
        // ([`a_daemon_read_reports_an_entry_under_a_daemon_it_does_not_hold_as_a_failure`]).
        // The last row pairs an unusable entry with a usable one, so the refusal is of the
        // REQUEST and not of one entry's own report.
        let rows: [(&str, Value); 11] = [
            ("a member that holds no entry at all", json!([])),
            ("a value that is no ARN at all", json!(["not-an-arn"])),
            (
                "an ARN of another service",
                json!([format!(
                    "arn:aws:eks:{SIGNED_REGION}:{ACCOUNT}:{resource_type}/{CLUSTER}/agents/{}",
                    sequence_id(1)
                )]),
            ),
            (
                "an ARN with no partition",
                json!([format!(
                    "arn::ecs:{SIGNED_REGION}:{ACCOUNT}:{resource_type}/{CLUSTER}/agents/{}",
                    sequence_id(1)
                )]),
            ),
            (
                "an ARN with no Region",
                json!([format!(
                    "arn:aws:ecs::{ACCOUNT}:{resource_type}/{CLUSTER}/agents/{}",
                    sequence_id(1)
                )]),
            ),
            (
                "an ARN with no account",
                json!([format!(
                    "arn:aws:ecs:{SIGNED_REGION}::{resource_type}/{CLUSTER}/agents/{}",
                    sequence_id(1)
                )]),
            ),
            (
                "an ARN of the daemon itself",
                json!([daemon_arn(CLUSTER, "agents")]),
            ),
            (
                "a resource path that carries no daemon segment",
                json!([format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:{resource_type}/{CLUSTER}/{}",
                    sequence_id(1)
                )]),
            ),
            (
                "a resource path with a segment too many",
                json!([format!(
                    "{}/extra",
                    below_daemon(resource_type, CLUSTER, "agents", &sequence_id(1))
                )]),
            ),
            (
                "a daemon name spelled outside the name rule",
                json!([below_daemon(
                    resource_type,
                    CLUSTER,
                    "agents!",
                    &sequence_id(1)
                )]),
            ),
            (
                "an unusable entry beside a usable one",
                json!([
                    below_daemon(resource_type, CLUSTER, "agents", &sequence_id(1)),
                    "not-an-arn",
                ]),
            ),
        ];

        for (row, entries) in rows {
            let case = format!("{operation} given {row}");
            let (code, message) =
                refusal(&router, operation, json!({ member: entries }), &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (INVALID_PARAMETER, expected),
                "case {case:?}: expected the sentence this read words for an unusable \
                 {member} entry, got {code:?} with {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_daemon_read_reports_an_arn_that_could_name_a_resource_and_names_none_as_a_failure() {
    let router = service();
    const CLUSTER: &str = "daemon-arn-failures";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-failures", CASE).await;
    let definition = daemon_definition(&router, "daemonarnfailures", CASE).await;
    let (daemon, deployment) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        &provider,
        &definition,
        CASE,
    )
    .await;
    let revision = {
        let body = ok(
            &router,
            "DescribeDaemonDeployments",
            json!({ "daemonDeploymentArns": [deployment.clone()] }),
            CASE,
        )
        .await;
        let reported = list(&body, "daemonDeployments", CASE);
        string(
            &reported
                .first()
                .unwrap_or_else(|| panic!("case {CASE:?}: expected the rollout, got {body}"))
                ["targetDaemonRevision"],
            "arn",
            CASE,
        )
    };
    assert!(
        revision.starts_with(&format!(
            "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon-revision/{CLUSTER}/agents/"
        )),
        "case {CASE:?}: expected the revision of this daemon, got {revision:?}"
    );

    // The entries the daemon's own resources ARE, beside one of the same form that this
    // account holds nothing under: the read answers about the first and reports the second
    // as a failure of its own entry, rather than losing the whole answer.
    let rows: [(&str, &str, &str, &str, &str); 2] = [
        (
            "DescribeDaemonDeployments",
            "daemonDeploymentArns",
            "daemonDeployments",
            "daemonDeploymentArn",
            &deployment,
        ),
        (
            "DescribeDaemonRevisions",
            "daemonRevisionArns",
            "daemonRevisions",
            "daemonRevisionArn",
            &revision,
        ),
    ];
    for (operation, member, reported, reported_arn, held) in rows {
        let unheld = {
            let (head, _) = held
                .rsplit_once('/')
                .unwrap_or_else(|| panic!("case {operation}: a daemon-scoped ARN, got {held:?}"));
            format!("{head}/{}", sequence_id(999))
        };
        let case = format!("{operation} over one held and one unheld ARN");
        let body = ok(
            &router,
            operation,
            json!({ member: [held, unheld.clone()] }),
            &case,
        )
        .await;
        assert_eq!(
            list(&body, reported, &case)
                .iter()
                .map(|entry| string(entry, reported_arn, &case))
                .collect::<Vec<String>>(),
            vec![held.to_string()],
            "case {case:?}: expected the resource that resolves, got {body}"
        );
        let failures = list(&body, "failures", &case);
        assert_eq!(
            failures
                .iter()
                .map(|failure| (
                    string(failure, "arn", &case),
                    string(failure, "reason", &case)
                ))
                .collect::<Vec<(String, String)>>(),
            vec![(unheld, "MISSING".to_string())],
            "case {case:?}: expected the unheld entry echoed as MISSING, got {body}"
        );
    }

    // The daemon the rows read through is still the one the create made: these reads report,
    // and change nothing a later caller sees.
    let body = ok(
        &router,
        "DescribeDaemon",
        json!({ "daemonArn": daemon }),
        CASE,
    )
    .await;
    assert_eq!(
        string(&body["daemon"], "status", CASE),
        "ACTIVE",
        "case {CASE:?}: expected the reads to leave the daemon as it was, got {body}"
    );
}

#[tokio::test]
async fn a_daemon_write_resolves_the_resources_its_parameters_name_before_the_daemon() {
    let router = service();
    const CLUSTER: &str = "daemon-write-order";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-order", CASE).await;
    let definition = daemon_definition(&router, "daemonwriteorder", CASE).await;

    // A daemon task definition nothing was registered under is refused about THAT resource
    // however much else of the request names nothing: the rows differ along what else is
    // missing — the capacity providers, the daemon, the cluster the daemon ARN carries, and
    // all three at once.
    let missing_definition = "neverregistered";
    let rows: [(&str, &str, Value); 3] = [
        (
            "UpdateDaemon",
            "a daemon of a cluster this account never held",
            json!({
                "daemonArn": daemon_arn("daemon-nowhere", "agents"),
                "daemonTaskDefinitionArn": missing_definition,
                "capacityProviderArns": ["mi-nothing"],
            }),
        ),
        (
            "UpdateDaemon",
            "a daemon the cluster does not hold",
            json!({
                "daemonArn": daemon_arn(CLUSTER, "nosuchdaemon"),
                "daemonTaskDefinitionArn": missing_definition,
                "capacityProviderArns": [provider.clone()],
            }),
        ),
        (
            "CreateDaemon",
            "a cluster this account never held",
            json!({
                "clusterArn": "daemon-nowhere",
                "daemonName": "agents",
                "daemonTaskDefinitionArn": missing_definition,
                "capacityProviderArns": ["mi-nothing"],
            }),
        ),
    ];
    for (operation, row, request) in rows {
        let case = format!("{operation} against {row}");
        let (code, message) = refusal(&router, operation, request, &case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ClientException", "DaemonTaskDefinition not found."),
            "case {case:?}: expected the refusal the named task definition earns"
        );
    }

    // With the task definition resolved, the capacity providers the request names come next
    // — before the daemon the ARN identifies, which this cluster does not hold either.
    let (code, message) = refusal(
        &router,
        "UpdateDaemon",
        json!({
            "daemonArn": daemon_arn(CLUSTER, "nosuchdaemon"),
            "daemonTaskDefinitionArn": definition,
            "capacityProviderArns": ["mi-nothing"],
        }),
        "UpdateDaemon naming a capacity provider this account does not hold",
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            INVALID_PARAMETER,
            "One or more capacity providers not found or invalid"
        ),
        "expected the sentence the daemon surface words for its capacityProviderArns"
    );

    // Nothing above was stored: every one of them was refused.
    let body = ok(
        &router,
        "ListDaemons",
        json!({ "clusterArn": CLUSTER }),
        CASE,
    )
    .await;
    assert!(
        list(&body, "daemonSummariesList", CASE).is_empty(),
        "expected every refused write to store nothing, got {body}"
    );
}

/// The ARN of the rollout one daemon of `cluster` was created with, and the ARN of the
/// revision that rollout is deploying — the two daemon-scoped ARNs this service MINTED, read
/// back through the rollout's own report.
async fn minted_daemon_arns(
    router: &MultiServiceRouter,
    cluster: &str,
    provider: &str,
    definition: &str,
    case: &str,
) -> (String, String) {
    let (_, deployment) = create_daemon(
        router,
        cluster,
        "agents",
        provider,
        definition,
        case,
    )
    .await;
    let body = ok(
        router,
        "DescribeDaemonDeployments",
        json!({ "daemonDeploymentArns": [deployment.clone()] }),
        case,
    )
    .await;
    let reported = list(&body, "daemonDeployments", case);
    let revision = string(
        &reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the rollout, got {body}"))
            ["targetDaemonRevision"],
        "arn",
        case,
    );
    (deployment, revision)
}

#[tokio::test]
async fn a_daemon_read_reads_an_unheld_id_of_the_form_it_mints_and_refuses_a_value_outside_it() {
    let router = service();
    const CLUSTER: &str = "daemon-arn-id-forms";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-id-forms", CASE).await;
    let definition = daemon_definition(&router, "daemonarnidforms", CASE).await;
    let (deployment, revision) =
        minted_daemon_arns(&router, CLUSTER, &provider, &definition, CASE).await;

    for (operation, member, resource_type, expected) in ARN_LIST_READS {
        // The ARN this service minted for the resource, so every row below is answered by
        // what the store holds under the id rather than by a read that reports nothing
        // whatever it is asked.
        let held = if resource_type == "daemon-deployment" {
            deployment.clone()
        } else {
            revision.clone()
        };
        let reported = if resource_type == "daemon-deployment" {
            ("daemonDeployments", "daemonDeploymentArn")
        } else {
            ("daemonRevisions", "daemonRevisionArn")
        };
        // The id this service minted is one of the form it hands out, so the rows below read
        // against a store whose own ids are of that form too.
        assert!(
            held.rsplit_once('/')
                .is_some_and(|(_, id)| is_uuid(id)),
            "case {CASE:?}: expected the minted ARN to end in an id of the form the service \
             spells such an id in, got {held:?}"
        );

        // One half of the rule: an id OF THAT FORM this store holds nothing under is the
        // entry's own `MISSING` failure at HTTP 200. The rows differ along which id of the
        // form it is — a counter this store never reached, the nil uuid, a uuid of mixed hex
        // digits, and the same uuid spelled in upper case (both cases are that form).
        let unheld_ids: [(&str, String); 4] = [
            (
                "an id of the form under a counter this store never reached",
                sequence_id(987_654),
            ),
            (
                "the nil uuid",
                "00000000-0000-0000-0000-000000000000".to_string(),
            ),
            (
                "a uuid of mixed hex digits",
                "7f3c1a2b-9d84-4e55-b0c6-2a1f5e6d7c88".to_string(),
            ),
            (
                "that uuid spelled in upper case",
                "7F3C1A2B-9D84-4E55-B0C6-2A1F5E6D7C88".to_string(),
            ),
        ];
        for (row, id) in unheld_ids {
            let unheld = below_daemon(resource_type, CLUSTER, "agents", &id);
            let case = format!("{operation} given {row}");
            // Paired with the ARN that DOES resolve, so the answer carries both channels:
            // the request is not lost, and the entry that names nothing is its own failure.
            let body = ok(
                &router,
                operation,
                json!({ member: [held.clone(), unheld.clone()] }),
                &case,
            )
            .await;
            assert_eq!(
                list(&body, reported.0, &case)
                    .iter()
                    .map(|entry| string(entry, reported.1, &case))
                    .collect::<Vec<String>>(),
                vec![held.clone()],
                "case {case:?}: expected the resource the minted ARN names to still be \
                 reported, got {body}"
            );
            assert_eq!(
                list(&body, "failures", &case)
                    .iter()
                    .map(|failure| (
                        string(failure, "arn", &case),
                        string(failure, "reason", &case)
                    ))
                    .collect::<Vec<(String, String)>>(),
                vec![(unheld.clone(), "MISSING".to_string())],
                "case {case:?}: expected the entry echoed as MISSING, got {body}"
            );
        }

        // The other half: a value in the id's place that is OUTSIDE that form could name no
        // resource of any account, so the whole read is refused about the parameter that
        // carried it. The rows differ along HOW the value departs from the form — a bare
        // counter, a word, a word with a counter after it, a minted id with a suffix, each
        // group width one digit short and one too long, one group too few and one too many,
        // the hex digits with their hyphens taken out, a digit outside hexadecimal, and a
        // long token — and each is paired with the entry that DOES resolve, so what is
        // refused is the request rather than that entry's own report.
        let unusable_ids: [(&str, String); 12] = [
            ("a bare counter", "1".to_string()),
            ("a word", "latest".to_string()),
            ("a word with a counter after it", "rollout-0000000017".to_string()),
            (
                "a minted id with a suffix",
                format!("{}-gone", sequence_id(1)),
            ),
            (
                "a group one digit short",
                "7f3c1a2b-9d84-4e55-b0c6-2a1f5e6d7c8".to_string(),
            ),
            (
                "a group one digit too long",
                "7f3c1a2b-9d84-4e55-b0c6-2a1f5e6d7c889".to_string(),
            ),
            (
                "a leading group one digit short",
                "7f3c1a2-9d84-4e55-b0c6-2a1f5e6d7c88".to_string(),
            ),
            (
                "one group too few",
                "7f3c1a2b-9d84-4e55-2a1f5e6d7c88".to_string(),
            ),
            (
                "one group too many",
                "7f3c1a2b-9d84-4e55-b0c6-2a1f5e6d7c88-0000".to_string(),
            ),
            (
                "the hex digits with their hyphens taken out",
                "7f3c1a2b9d844e55b0c62a1f5e6d7c88".to_string(),
            ),
            (
                "a digit outside hexadecimal",
                "7f3c1a2b-9d84-4e55-b0c6-2a1f5e6d7cg8".to_string(),
            ),
            ("a long token", "z".repeat(64)),
        ];
        for (row, id) in unusable_ids {
            let unusable = below_daemon(resource_type, CLUSTER, "agents", &id);
            let case = format!("{operation} given {row} as the id");
            let (code, message) = refusal(
                &router,
                operation,
                json!({ member: [held.clone(), unusable] }),
                &case,
            )
            .await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (INVALID_PARAMETER, expected),
                "case {case:?}: expected the sentence this read words for an unusable \
                 {member} entry, got {code:?} with {message:?}"
            );
        }
    }
}

/// Whether `value` is spelled as the UUID this service's daemon-scoped ids are spelled in:
/// five hyphen-separated groups of 8, 4, 4, 4 and 12 hexadecimal digits, in either case.
/// Read here off the value a response carried, so a row asserts the FORM rather than the id
/// one run happened to mint.
fn is_uuid(value: &str) -> bool {
    let mut groups = value.split('-');
    for width in [8, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

#[tokio::test]
async fn a_daemon_read_reports_an_entry_under_a_daemon_it_does_not_hold_as_a_failure() {
    let router = service();
    const CLUSTER: &str = "daemon-unheld-daemons";
    // A second cluster of this account that holds no daemon at all, so a row can differ
    // along the DAEMON while the cluster it names is one this account created.
    const EMPTY_CLUSTER: &str = "daemon-unheld-empty";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    create_cluster(&router, EMPTY_CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-unheld", CASE).await;
    let definition = daemon_definition(&router, "daemonunhelddaemons", CASE).await;
    let (deployment, revision) =
        minted_daemon_arns(&router, CLUSTER, &provider, &definition, CASE).await;

    for (operation, member, resource_type, _) in ARN_LIST_READS {
        // The ARNs this service MINTED for the daemon it holds, so every row below is
        // answered against a store that does hold such resources.
        let held = if resource_type == "daemon-deployment" {
            deployment.clone()
        } else {
            revision.clone()
        };
        let reported = if resource_type == "daemon-deployment" {
            ("daemonDeployments", "daemonDeploymentArn")
        } else {
            ("daemonRevisions", "daemonRevisionArn")
        };

        // The rows differ along WHY this account holds no such daemon: the cluster was
        // never created, the cluster exists but holds no daemon, the cluster holds a
        // daemon under another name, and the name it holds is a PREFIX of the name the
        // entry carries. Each entry is still shaped like the ARN of a resource of this
        // account, so each is reported on its own `failures` entry at HTTP 200 rather than
        // refusing the read — the not-found convention the other ECS batch reads follow.
        let rows: [(&str, String); 4] = [
            (
                "a cluster this account never created",
                below_daemon(resource_type, "daemon-nowhere", "agents", &sequence_id(1)),
            ),
            (
                "a cluster of this account that holds no daemon",
                below_daemon(resource_type, EMPTY_CLUSTER, "agents", &sequence_id(1)),
            ),
            (
                "a daemon this cluster does not hold",
                below_daemon(resource_type, CLUSTER, "nosuchdaemon", &sequence_id(1)),
            ),
            (
                "a daemon name the held daemon's name is a prefix of",
                below_daemon(resource_type, CLUSTER, "agents-2", &sequence_id(1)),
            ),
        ];
        for (row, entry) in rows {
            let case = format!("{operation} given an entry under {row}");
            let body = ok(&router, operation, json!({ member: [entry.clone()] }), &case).await;
            assert!(
                list(&body, reported.0, &case).is_empty(),
                "case {case:?}: expected no resource reported for an entry that resolves to \
                 none, got {body}"
            );
            assert_eq!(
                list(&body, "failures", &case)
                    .iter()
                    .map(|failure| (
                        string(failure, "arn", &case),
                        string(failure, "reason", &case)
                    ))
                    .collect::<Vec<(String, String)>>(),
                vec![(entry.clone(), "MISSING".to_string())],
                "case {case:?}: expected the entry echoed as MISSING, got {body}"
            );

            // The same entry BESIDE the one that resolves: the entry that names nothing is
            // its own failure, so the answer the caller could have had is not lost.
            let case = format!("{case}, beside a usable entry");
            let body = ok(
                &router,
                operation,
                json!({ member: [held.clone(), entry.clone()] }),
                &case,
            )
            .await;
            assert_eq!(
                list(&body, reported.0, &case)
                    .iter()
                    .map(|resolved| string(resolved, reported.1, &case))
                    .collect::<Vec<String>>(),
                vec![held.clone()],
                "case {case:?}: expected the resource that resolves to still be reported, \
                 got {body}"
            );
            assert_eq!(
                list(&body, "failures", &case)
                    .iter()
                    .map(|failure| (
                        string(failure, "arn", &case),
                        string(failure, "reason", &case)
                    ))
                    .collect::<Vec<(String, String)>>(),
                vec![(entry, "MISSING".to_string())],
                "case {case:?}: expected the unresolved entry echoed as MISSING, got {body}"
            );
        }

        // The other half of the rule: under a daemon this account DOES hold, an id the
        // store holds nothing under is the per-entry `MISSING` failure at HTTP 200 —
        // whatever that id looks like, here the nil uuid.
        let unheld = below_daemon(
            resource_type,
            CLUSTER,
            "agents",
            "00000000-0000-0000-0000-000000000000",
        );
        let case = format!("{operation} given an unheld id under the daemon this account holds");
        let body = ok(
            &router,
            operation,
            json!({ member: [held.clone(), unheld.clone()] }),
            &case,
        )
        .await;
        assert_eq!(
            list(&body, reported.0, &case)
                .iter()
                .map(|entry| string(entry, reported.1, &case))
                .collect::<Vec<String>>(),
            vec![held.clone()],
            "case {case:?}: expected the resource the minted ARN names to still be reported, \
             got {body}"
        );
        assert_eq!(
            list(&body, "failures", &case)
                .iter()
                .map(|failure| (
                    string(failure, "arn", &case),
                    string(failure, "reason", &case)
                ))
                .collect::<Vec<(String, String)>>(),
            vec![(unheld, "MISSING".to_string())],
            "case {case:?}: expected the unheld id echoed as MISSING, got {body}"
        );
    }
}

/// One row of the batch table below: the case, the entries the request carries, the ARNs the
/// read resolves and reports, and the ARNs it reports as `MISSING` failures.
type BatchRow = (&'static str, Vec<String>, Vec<String>, Vec<String>);

#[tokio::test]
async fn a_daemon_read_answers_every_entry_it_resolves_beside_the_ones_it_does_not() {
    let router = service();
    const CLUSTER: &str = "daemon-arn-batches";
    // A second cluster of this account that holds no daemon, so a row can differ along the
    // DAEMON while the cluster it names is one this account created.
    const EMPTY_CLUSTER: &str = "daemon-arn-batches-empty";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    create_cluster(&router, EMPTY_CLUSTER, CASE).await;
    let provider = managed_provider(&router, CLUSTER, "mi-batches", CASE).await;
    let definition = daemon_definition(&router, "daemonarnbatches", CASE).await;
    let (deployment, revision) =
        minted_daemon_arns(&router, CLUSTER, &provider, &definition, CASE).await;

    for (operation, member, resource_type, _) in ARN_LIST_READS {
        // The ARN this service MINTED for the resource, so each row is answered against a
        // store that does hold one of them.
        let held = if resource_type == "daemon-deployment" {
            deployment.clone()
        } else {
            revision.clone()
        };
        let reported = if resource_type == "daemon-deployment" {
            ("daemonDeployments", "daemonDeploymentArn")
        } else {
            ("daemonRevisions", "daemonRevisionArn")
        };

        // Entries of the ARN's own shape that this store resolves nothing for, one per part
        // of the identifier it holds nothing for.
        let unknown_cluster =
            below_daemon(resource_type, "daemon-nowhere", "agents", &sequence_id(17));
        let empty_cluster = below_daemon(resource_type, EMPTY_CLUSTER, "agents", &sequence_id(17));
        let unknown_daemon = below_daemon(resource_type, CLUSTER, "nosuchdaemon", &sequence_id(17));
        let unknown_id = below_daemon(
            resource_type,
            CLUSTER,
            "agents",
            "00000000-0000-0000-0000-000000000000",
        );

        // The rows differ along how many unresolved entries the batch carries, where they sit
        // relative to the entry that resolves, which part of the identifier each of them
        // resolves nothing for, whether the batch resolves anything at all, and whether an
        // entry is repeated — none of which may cost the caller the answers it could have had.
        let rows: [BatchRow; 6] = [
            (
                "one unresolved entry after the resource it holds",
                vec![held.clone(), unknown_cluster.clone()],
                vec![held.clone()],
                vec![unknown_cluster.clone()],
            ),
            (
                "one unresolved entry before the resource it holds",
                vec![unknown_cluster.clone(), held.clone()],
                vec![held.clone()],
                vec![unknown_cluster.clone()],
            ),
            (
                "one unresolved entry per part of the identifier",
                vec![
                    unknown_cluster.clone(),
                    empty_cluster.clone(),
                    held.clone(),
                    unknown_daemon.clone(),
                    unknown_id.clone(),
                ],
                vec![held.clone()],
                vec![
                    unknown_cluster.clone(),
                    empty_cluster.clone(),
                    unknown_daemon.clone(),
                    unknown_id.clone(),
                ],
            ),
            (
                "nothing but unresolved entries",
                vec![unknown_cluster.clone(), unknown_id.clone()],
                Vec::new(),
                vec![unknown_cluster.clone(), unknown_id.clone()],
            ),
            (
                "the same unresolved entry named twice",
                vec![
                    held.clone(),
                    unknown_cluster.clone(),
                    unknown_cluster.clone(),
                ],
                vec![held.clone()],
                vec![unknown_cluster.clone()],
            ),
            (
                "the resource it holds named twice beside one unresolved entry",
                vec![held.clone(), held.clone(), unknown_daemon.clone()],
                vec![held.clone()],
                vec![unknown_daemon.clone()],
            ),
        ];

        for (row, entries, expected_resolved, expected_failures) in rows {
            let case = format!("{operation} given {row}");
            let body = ok(&router, operation, json!({ member: entries }), &case).await;
            assert_eq!(
                list(&body, reported.0, &case)
                    .iter()
                    .map(|resolved| string(resolved, reported.1, &case))
                    .collect::<Vec<String>>(),
                expected_resolved,
                "case {case:?}: expected every entry that resolves to be reported once, \
                 got {body}"
            );
            assert_eq!(
                list(&body, "failures", &case)
                    .iter()
                    .map(|failure| (
                        string(failure, "arn", &case),
                        string(failure, "reason", &case)
                    ))
                    .collect::<Vec<(String, String)>>(),
                expected_failures
                    .iter()
                    .map(|arn| (arn.clone(), "MISSING".to_string()))
                    .collect::<Vec<(String, String)>>(),
                "case {case:?}: expected one MISSING failure per unresolved entry, in the \
                 order the request named them, got {body}"
            );
        }

        // These reads report: after every row above, the resource the store holds still
        // resolves on its own and nothing was failed for it.
        let case = format!("{operation} re-reading the resource the store holds");
        let body = ok(&router, operation, json!({ member: [held.clone()] }), &case).await;
        assert_eq!(
            list(&body, reported.0, &case)
                .iter()
                .map(|resolved| string(resolved, reported.1, &case))
                .collect::<Vec<String>>(),
            vec![held.clone()],
            "case {case:?}: expected the reads to leave the resource readable, got {body}"
        );
        assert!(
            list(&body, "failures", &case).is_empty(),
            "case {case:?}: expected no failure for the resource the store holds, got {body}"
        );
    }
}
