// SPDX-License-Identifier: CC0-1.0

//! The scaling process this service accepts the NAME of without publishing it.
//!
//! `SuspendProcesses` and `ResumeProcesses` carry `ScalingProcessQuery$ScalingProcesses`,
//! whose own documentation lists nine processes and states "if you omit this property, all
//! processes are specified"
//! (`smithy_query(query="doc",shape="ScalingProcessQuery",member="ScalingProcesses")`), and
//! `DescribeScalingProcessTypes` publishes exactly those nine. Beside them the service keeps
//! accepting one further name — [`UNPUBLISHED_PROCESS`], the low-priority removal of a
//! group's instances from its load balancers, which the service no longer performs — and a
//! group that has it suspended reports it as a `SuspendedProcess` of its own
//! (`smithy_query(query="shape",shape="SuspendedProcess")`). So the set a request may NAME,
//! and the set a request naming none suspends, is the published catalog plus that one name,
//! while the set the service PUBLISHES — the catalog a read reports and the valid names an
//! unknown-process refusal offers — stays the nine.
//!
//! The tables below span what that rule generalises over: whether the name is sent alone,
//! beside published ones, twice, or left for the request that names none; which side of the
//! pair (suspend or resume) carries it; and whether the processes resumed are named
//! explicitly or left to the request that names none. Every row goes over the awsQuery wire
//! through one router, and the name is read back through `DescribeAutoScalingGroups`.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_values, xml_unescape,
};

/// The process the service accepts the name of without publishing it, as the service spells
/// it. Written out here because nothing publishes it: it is in no catalog a read reports and
/// in no list the model documents — it is recorded behaviour of `SuspendProcesses`, which
/// accepts the name, and of `DescribeAutoScalingGroups`, which reports it among the
/// suspensions of a group that suspended every process.
const UNPUBLISHED_PROCESS: &str = "RemoveFromLoadBalancerLowPriority";

/// The launch source every group here is created from: a launch TEMPLATE, because no
/// account can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The processes the service publishes as its own catalog, read over the wire from
/// `DescribeScalingProcessTypes`, so no row here writes that membership out.
async fn published_catalog(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeScalingProcessTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published process catalog: expected 200, got {status}: {body}"
    );
    let catalog = tag_values(&body, "ProcessName");
    assert!(
        catalog.len() > 1,
        "the published process catalog must name more than one process, got {catalog:?}"
    );
    catalog
}

/// Every process a suspension or a resumption may name: the published catalog and the one
/// name beside it, in the ASCII order a group reports its suspensions in.
fn suspendable(catalog: &[String]) -> Vec<String> {
    let mut processes = catalog.to_vec();
    processes.push(UNPUBLISHED_PROCESS.to_string());
    processes.sort();
    processes
}

/// Create one group placed in the zones of `subnet_ids`, holding no instances: these rows
/// are about the names a group has suspended, not about its capacity.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    subnet_ids: &str,
) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", subnet_ids),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the group {name:?}: expected 200, got {status}: {body}"
    );
}

/// One `SuspendProcesses` or `ResumeProcesses` call for one group, naming the processes
/// `processes` lists — naming none when it is empty.
async fn process_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    action: &str,
    group: &str,
    processes: &[String],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            process.clone(),
        ));
    }
    query(router, action, &borrowed(&params)).await
}

/// The processes one group reports suspended, in the order `DescribeAutoScalingGroups`
/// reports them, each beside the reason it is suspended for.
async fn reported_suspensions(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the group {group:?} back: expected 200, got {status}: {body}"
    );
    let names = tag_values(&body, "ProcessName");
    let reasons = tag_values(&body, "SuspensionReason");
    assert_eq!(
        names.len(),
        reasons.len(),
        "every suspension reports a process and a reason, got {names:?} and {reasons:?} from \
         {body}"
    );
    names.into_iter().zip(reasons).collect()
}

/// What a row's request names in `ScalingProcesses`.
enum Named {
    /// The member is left out altogether, which names every suspendable process.
    Omitted,
    /// The processes written here, in this order.
    Processes(Vec<&'static str>),
    /// Every process the service publishes, named explicitly — which leaves the one it does
    /// not publish unnamed.
    WholePublishedCatalog,
}

impl Named {
    /// The names one row sends, given the catalog the service publishes.
    fn names(&self, catalog: &[String]) -> Vec<String> {
        match self {
            Self::Omitted => Vec::new(),
            Self::Processes(processes) => processes
                .iter()
                .map(|process| (*process).to_string())
                .collect(),
            Self::WholePublishedCatalog => catalog.to_vec(),
        }
    }
}

/// What a row expects the group to report afterwards.
enum Expected {
    /// Every suspendable process, the unpublished name among them.
    EverySuspendableProcess,
    /// Only the processes the service publishes.
    ThePublishedCatalog,
    /// Exactly these, in the ASCII order the group reports them in.
    Processes(Vec<&'static str>),
}

impl Expected {
    /// The process names one row expects, given the catalog the service publishes.
    fn names(&self, catalog: &[String]) -> Vec<String> {
        match self {
            Self::EverySuspendableProcess => suspendable(catalog),
            Self::ThePublishedCatalog => {
                let mut published = catalog.to_vec();
                published.sort();
                published
            }
            Self::Processes(processes) => processes
                .iter()
                .map(|process| (*process).to_string())
                .collect(),
        }
    }
}

/// One row of the suspension table.
struct SuspensionCase {
    case: &'static str,
    named: Named,
    expected: Expected,
}

/// A suspension that names the unpublished process is served, and the group reports that
/// process beside the published ones it has suspended — including the suspension a request
/// that names none records.
#[tokio::test]
async fn a_suspension_may_name_the_process_the_service_does_not_publish() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;

    let cases = [
        SuspensionCase {
            case: "the unpublished process alone",
            named: Named::Processes(vec![UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(vec![UNPUBLISHED_PROCESS]),
        },
        SuspensionCase {
            case: "the unpublished process twice: one suspension",
            named: Named::Processes(vec![UNPUBLISHED_PROCESS, UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(vec![UNPUBLISHED_PROCESS]),
        },
        SuspensionCase {
            case: "the unpublished process after a published one",
            named: Named::Processes(vec!["Launch", UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(vec!["Launch", UNPUBLISHED_PROCESS]),
        },
        SuspensionCase {
            case: "the unpublished process before two published ones",
            named: Named::Processes(vec![UNPUBLISHED_PROCESS, "Terminate", "AZRebalance"]),
            expected: Expected::Processes(vec![
                "AZRebalance",
                UNPUBLISHED_PROCESS,
                "Terminate",
            ]),
        },
        SuspensionCase {
            case: "no process named: every suspendable process, the unpublished one included",
            named: Named::Omitted,
            expected: Expected::EverySuspendableProcess,
        },
        SuspensionCase {
            case: "the whole published catalog named explicitly: only those are suspended",
            named: Named::WholePublishedCatalog,
            expected: Expected::ThePublishedCatalog,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = format!("unpublished-suspend-{index}");
        create_group(&router, &name, &subnets[0].id).await;

        let named = case.named.names(&catalog);
        let (status, body) = process_call(&router, "SuspendProcesses", &name, &named).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<SuspendProcessesResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );
        assert!(
            !body.contains("<ErrorResponse"),
            "case {:?}: a served suspension carries no error envelope, got {body}",
            case.case
        );

        let expected = case.expected.names(&catalog);
        let reported = reported_suspensions(&router, &name).await;
        let reported_names: Vec<String> = reported
            .iter()
            .map(|(process, _)| process.clone())
            .collect();
        assert_eq!(
            reported_names, expected,
            "case {:?}: expected the suspended processes {expected:?}, got {reported:?}",
            case.case
        );
        // A read reports state, it does not move it: the same suspensions come back.
        let again = reported_suspensions(&router, &name).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second read reports the same suspensions, got {again:?} after \
             {reported:?}",
            case.case
        );
    }
}

/// One row of the resumption table.
struct ResumptionCase {
    case: &'static str,
    suspend: Named,
    resume: Named,
    expected: Expected,
}

/// A resumption may name the unpublished process too, and a request that names none resumes
/// it with the rest — while one that names the published catalog explicitly leaves it
/// suspended.
#[tokio::test]
async fn a_resumption_may_name_the_process_the_service_does_not_publish() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;

    let cases = [
        ResumptionCase {
            case: "suspended alone, resumed by name",
            suspend: Named::Processes(vec![UNPUBLISHED_PROCESS]),
            resume: Named::Processes(vec![UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(Vec::new()),
        },
        ResumptionCase {
            case: "suspended beside a published process, only the unpublished one resumed",
            suspend: Named::Processes(vec![UNPUBLISHED_PROCESS, "HealthCheck"]),
            resume: Named::Processes(vec![UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(vec!["HealthCheck"]),
        },
        ResumptionCase {
            case: "suspended beside a published process, only the published one resumed",
            suspend: Named::Processes(vec![UNPUBLISHED_PROCESS, "HealthCheck"]),
            resume: Named::Processes(vec!["HealthCheck"]),
            expected: Expected::Processes(vec![UNPUBLISHED_PROCESS]),
        },
        ResumptionCase {
            case: "every process suspended, every one resumed by naming none",
            suspend: Named::Omitted,
            resume: Named::Omitted,
            expected: Expected::Processes(Vec::new()),
        },
        ResumptionCase {
            case: "every process suspended, the published catalog resumed explicitly",
            suspend: Named::Omitted,
            resume: Named::WholePublishedCatalog,
            expected: Expected::Processes(vec![UNPUBLISHED_PROCESS]),
        },
        ResumptionCase {
            case: "nothing suspended: resuming the unpublished process changes nothing",
            suspend: Named::Processes(Vec::new()),
            resume: Named::Processes(vec![UNPUBLISHED_PROCESS]),
            expected: Expected::Processes(Vec::new()),
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = format!("unpublished-resume-{index}");
        create_group(&router, &name, &subnets[0].id).await;

        let suspended = case.suspend.names(&catalog);
        let suspends_something =
            !matches!(&case.suspend, Named::Processes(processes) if processes.is_empty());
        if suspends_something {
            let (status, body) = process_call(&router, "SuspendProcesses", &name, &suspended).await;
            assert_eq!(
                status, 200,
                "case {:?}: suspending expected 200, got {status}: {body}",
                case.case
            );
        }

        let (status, body) =
            process_call(&router, "ResumeProcesses", &name, &case.resume.names(&catalog)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<ResumeProcessesResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );

        let expected = case.expected.names(&catalog);
        let reported = reported_suspensions(&router, &name).await;
        let reported_names: Vec<String> = reported
            .iter()
            .map(|(process, _)| process.clone())
            .collect();
        assert_eq!(
            reported_names, expected,
            "case {:?}: expected the suspended processes {expected:?} to be left, got \
             {reported:?}",
            case.case
        );
    }
}

/// The name the service accepts is published by nothing: the catalog a read reports does not
/// carry it, and the valid names an unknown-process refusal offers are that same catalog —
/// so a name outside the set a request may carry is still refused, and writes nothing.
#[tokio::test]
async fn the_unpublished_process_is_in_no_catalog_and_in_no_refusal() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;
    create_group(&router, "held", &subnets[0].id).await;

    assert!(
        !catalog.iter().any(|process| process == UNPUBLISHED_PROCESS),
        "the published catalog does not name {UNPUBLISHED_PROCESS:?}, got {catalog:?}"
    );

    // A name the service has no process for at all, beside one it does: the refusal names
    // every name it could not use and offers the processes it publishes.
    for unusable in [
        vec!["Nope"],
        vec![UNPUBLISHED_PROCESS, "Nope"],
        // The unpublished name is matched exactly, like every published one: a different
        // spelling of it names no process either.
        vec!["removefromloadbalancerlowpriority"],
    ] {
        let named: Vec<String> = unusable
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        let offending: Vec<String> = named
            .iter()
            .filter(|process| process.as_str() != UNPUBLISHED_PROCESS)
            .cloned()
            .collect();
        let (status, body) = process_call(&router, "SuspendProcesses", "held", &named).await;
        assert_eq!(
            status, 400,
            "case {named:?}: expected 400, got {status}: {body}"
        );
        assert!(
            body.contains("<Code>ValidationError</Code>"),
            "case {named:?}: expected the ValidationError code, got {body}"
        );
        let expected_message = format!(
            "Request contained illegal suspendable process names: [{}].Valid names are :[{}].",
            offending.join(", "),
            catalog.join(", ")
        );
        assert!(
            xml_unescape(&body).contains(&expected_message),
            "case {named:?}: expected the message {expected_message:?}, got {body}"
        );

        let reported = reported_suspensions(&router, "held").await;
        assert!(
            reported.is_empty(),
            "case {named:?}: a refused request leaves the group running every process, got \
             {reported:?}"
        );
    }
}
