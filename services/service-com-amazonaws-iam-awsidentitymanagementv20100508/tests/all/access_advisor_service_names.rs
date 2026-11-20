// SPDX-License-Identifier: CC0-1.0

//! WHICH services an access-advisor report enumerates, and under WHICH name each of them is
//! reported, end to end over the awsQuery wire.
//!
//! RULE: `GetServiceLastAccessedDetails` enumerates the service namespaces access-advisor
//! data covers — AWS's published authorization prefixes
//! (`ServiceLastAccessed$ServiceNamespace`, "find the service prefix" in the Service
//! Authorization Reference) — ascending by namespace, each under the display name that data
//! publishes for the namespace (`ServiceLastAccessed$ServiceName`). So the PAIR at a position
//! is the service: a namespace the data covers is enumerated, a prefix it does not cover is
//! enumerated nowhere while a read that NAMES that prefix still accepts it, and a client
//! reading the list positionally finds the same service at the same position AWS does.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: the namespace
//! read (a prefix that is also a whole service's name, a prefix of a service reported under
//! another service's name, one of several prefixes of one service, a prefix the data does not
//! cover) and whether the entry is asked for by position or by name. Each pair is written
//! out rather than read back out of the response, and the namespaces of one run are asserted
//! to be CONSECUTIVE, which is what a positional read of the report depends on.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`, `await_completed_report`,
//! `read_report_page`) is the one `tests/all/user_lifecycle.rs` owns and every suite in this
//! crate shares.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{
    await_completed_report, call, element, elements, read_report_page, router,
};

/// A grant reaching every service, so the report about the principal carrying it enumerates
/// every service access-advisor data covers.
const EVERY_ACTION: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#;

/// `maxItemsType` admits a thousand, which the whole enumeration fits inside — so one read
/// carries it all and a position in the page is a position in the report.
const WHOLE_REPORT: usize = 1_000;

/// `(the namespace, the display name access-advisor publishes for it)`.
///
/// SOURCE: the Service Authorization Reference's service list, as access-advisor data reports
/// it — the prefix each service publishes and the name bound to that prefix. The rows span a
/// service whose name the prefix spells (`appsync`), services reported under the name of the
/// service that owns the capability rather than their own
/// (`application-signals`, `applicationinsights`, `apptest`), a prefix that is one of two a
/// service publishes (`aps` beside `apigateway`/`execute-api`, which
/// `tests/all/service_last_accessed_details.rs` pins) and an entry of a different letter
/// (`backup`) so the rows are not one alphabetical neighbourhood.
const REPORTED: &[(&str, &str)] = &[
    ("application-signals", "Amazon CloudWatch Application Signals"),
    ("applicationinsights", "Amazon CloudWatch Application Insights"),
    ("appmesh", "AWS App Mesh"),
    ("appstream", "Amazon AppStream 2.0"),
    ("appstudio", "AWS App Studio"),
    ("appsync", "AWS AppSync"),
    ("apptest", "AWS Mainframe Modernization Application Testing"),
    ("aps", "Amazon Managed Service for Prometheus"),
    ("backup", "AWS Backup"),
];

/// Namespaces whose entries are CONSECUTIVE in the report, in this order: the run the
/// catalog's ascending namespace order puts them in, which is what a positional read walks.
///
/// The run covers the services of three products whose prefixes interleave alphabetically —
/// the App* services, Managed Service for Prometheus, and the two Application Recovery
/// Controller capabilities — so an entry missing from the middle of it is caught here rather
/// than read as the service at the next position.
const CONSECUTIVE: &[&str] = &[
    "appmesh",
    "apprunner",
    "appstream",
    "appstudio",
    "appsync",
    "apptest",
    "aps",
    "arc-region-switch",
    "arc-zonal-shift",
];

/// Prefixes IAM publishes that access-advisor data reports NO entry for: a policy can name an
/// action of them, so a read taking a `ServiceNamespace` accepts them, and the report
/// enumerates nothing for them.
///
/// SOURCE: the Service Authorization Reference still publishes the prefix of a service whose
/// access-advisor coverage ended, which is why the two sets are not one set.
const UNREPORTED: &[&str] = &["application-cost-profiler", "amplifybackend"];

/// Create the principal the report is about, holding [`EVERY_ACTION`], and answer its ARN.
async fn principal(router: &MultiServiceRouter, user_name: &str) -> String {
    let (status, body) = call(router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "CreateUser must succeed, got {status}: {body}"
    );
    let arn = element(&body, "Arn");
    let (status, body) = call(
        router,
        "PutUserPolicy",
        &[
            ("UserName", user_name),
            ("PolicyName", "reaches-everything"),
            ("PolicyDocument", EVERY_ACTION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutUserPolicy must succeed, got {status}: {body}"
    );
    arn
}

/// The whole report about `arn`, once the generation the clock owes it is due.
async fn whole_report(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, body) =
        call(router, "GenerateServiceLastAccessedDetails", &[("Arn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GenerateServiceLastAccessedDetails must succeed, got {status}: {body}"
    );
    let job_id = element(&body, "JobId");
    await_completed_report(router, case, &job_id).await;
    let page = read_report_page(router, case, &job_id, Some(WHOLE_REPORT), None).await;
    assert!(
        !page.is_truncated,
        "case {case:?}: the whole enumeration must fit in one page of {WHOLE_REPORT}"
    );
    (page.namespaces, page.names)
}

/// RULE over the table: every namespace the rows name is enumerated exactly once, under the
/// name access-advisor publishes for it.
#[tokio::test]
async fn each_enumerated_namespace_is_reported_under_its_published_name() {
    let router = router();
    let arn = principal(&router, "advisor-names").await;
    let (namespaces, names) = whole_report(&router, "the published names", &arn).await;
    assert_eq!(
        namespaces.len(),
        names.len(),
        "every entry must carry both members, got {} namespaces and {} names",
        namespaces.len(),
        names.len()
    );

    for (namespace, service_name) in REPORTED {
        let positions: Vec<usize> = namespaces
            .iter()
            .enumerate()
            .filter(|(_, reported)| *reported == namespace)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            positions.len(),
            1,
            "case {namespace:?}: expected the report to enumerate it exactly once, got it at \
             {positions:?}"
        );
        let at = positions[0];
        assert_eq!(
            names[at], *service_name,
            "case {namespace:?}: expected the published name {service_name:?}, got {:?}",
            names[at]
        );
    }
}

/// RULE, positionally: the namespaces of a run are enumerated at CONSECUTIVE positions in the
/// catalog's ascending order, so a client reading the list by position reads the next service
/// and not one the enumeration skipped.
#[tokio::test]
async fn the_entries_of_a_run_are_consecutive_and_ascending() {
    let router = router();
    let arn = principal(&router, "advisor-order").await;
    let (namespaces, _) = whole_report(&router, "the enumerated run", &arn).await;

    let first = namespaces
        .iter()
        .position(|reported| reported == CONSECUTIVE[0])
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the report must enumerate the run's first namespace, got \
                 {namespaces:?}",
                CONSECUTIVE[0]
            )
        });
    let run: Vec<&str> = namespaces
        .iter()
        .skip(first)
        .take(CONSECUTIVE.len())
        .map(String::as_str)
        .collect();
    assert_eq!(
        run, *CONSECUTIVE,
        "case \"the run from {:?}\": expected the consecutive run {CONSECUTIVE:?}, got {run:?}",
        CONSECUTIVE[0]
    );

    // And the whole enumeration ascends strictly, which is the order those positions are
    // decided by ("by default, the list is sorted by service namespace").
    for pair in namespaces.windows(2) {
        assert!(
            pair[0] < pair[1],
            "case \"the whole enumeration\": expected ascending namespaces, got {:?} before \
             {:?}",
            pair[0],
            pair[1]
        );
    }
}

/// RULE, the other half: a published prefix access-advisor data does not cover is enumerated
/// NOWHERE, while the reads that NAME a namespace still accept it — the two sets are not one
/// set, so a missing entry is an answer about the data rather than about the prefix.
#[tokio::test]
async fn a_prefix_the_data_does_not_cover_is_enumerated_nowhere_but_still_accepted() {
    let router = router();
    let arn = principal(&router, "advisor-uncovered").await;
    let (namespaces, _) = whole_report(&router, "the uncovered prefixes", &arn).await;

    for namespace in UNREPORTED {
        assert!(
            state::published_service(namespace).is_some(),
            "case {namespace:?}: the catalog must publish this prefix"
        );
        assert!(
            !namespaces.iter().any(|reported| reported == namespace),
            "case {namespace:?}: a prefix the data does not cover must be enumerated nowhere, \
             got {namespaces:?}"
        );
        let (status, body) = call(
            &router,
            "ListPoliciesGrantingServiceAccess",
            &[
                ("Arn", arn.as_str()),
                ("ServiceNamespaces.member.1", namespace),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {namespace:?}: a read naming the prefix must accept it, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "ServiceNamespace"),
            vec![(*namespace).to_string()],
            "case {namespace:?}: expected the namespace asked about, got {body}"
        );
    }
}
