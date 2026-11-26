// SPDX-License-Identifier: CC0-1.0

//! What a DELETE this service ACCEPTED makes of the reads that named the resource, driven over
//! the ec2Query wire the service speaks (`Action=<Operation>` in an
//! `application/x-www-form-urlencoded` POST, answered with XML — README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! ONE rule, over every resource kind whose modelled shape has no terminal deleted state to be
//! reported in: once the delete the service answered 200 has settled, the account no longer
//! holds the resource, so the describe that READS it by id refuses — HTTP 400 with that
//! resource kind's own not-found code and the sentence AWS words for that code, naming the id
//! the REQUEST sent — and the unfiltered describe stops listing it. A client that deletes and
//! then confirms the deletion has that refusal to stop on; nothing it reads afterwards can
//! make the resource readable again.
//!
//! Each row is a distinct input along what the rule generalises over: WHICH resource kind is
//! deleted, and so which call creates it, which call deletes it, which describe reads it back,
//! whether the removal happens inside the delete or falls due on the clock afterwards, and
//! which code and which sentence the refusal carries. The ids are the ones the SERVICE minted
//! in this run, so no recorded value is part of the expectation.
//!
//! The per-kind suites beside this one (`delete_public_ipv4_pool`, `instance_event_windows`,
//! `delete_network_insights_access_scope_analysis`, `ipam_scope_deletion`, `delete_ipam`) cover
//! what else each of these deletes decides; this suite is the ONE rule they share, asserted
//! across the kinds at once, and the last row is a kind no divergence was reported for, so the
//! rule is not fitted to the kinds that were.

use std::time::{Duration, Instant};

use ec2::state::{
    INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND, INVALID_IPAM_ID_NOT_FOUND,
    INVALID_IPAM_SCOPE_ID_NOT_FOUND, INVALID_NETWORK_INSIGHTS_ACCESS_SCOPE_ANALYSIS_ID_NOT_FOUND,
    INVALID_PUBLIC_IPV4_POOL_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for, which is the Region the envelope is selected by.
const REGION: &str = "us-west-2";

/// How long a look for the removal a delete owes keeps looking before the case fails: far
/// longer than any span this service records for such a move, so a loaded host cannot turn
/// "not due yet" into a failure. The loop leaves as soon as the refusal holds, so a fast host
/// waits no longer than the span itself.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How long each look of that poll waits before looking again — a client's own retry pause,
/// well under the span it is waiting for.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Which resource kind a row drives, which is what decides the calls that create it.
#[derive(Debug, Clone, Copy)]
enum Resource {
    /// `CreatePublicIpv4Pool`, which needs nothing of the caller.
    PublicIpv4Pool,
    /// `CreateInstanceEventWindow`, which needs a name and one of the two schedule forms.
    InstanceEventWindow,
    /// `StartNetworkInsightsAccessScopeAnalysis`, against a scope created first.
    AccessScopeAnalysis,
    /// `CreateIpamScope`, in an IPAM created first — a scope of the caller's own, the default
    /// scopes an IPAM comes up with not being deletable.
    IpamScope,
    /// `CreateIpam`, which needs nothing of the caller.
    Ipam,
}

/// One row of the table: a resource kind, and everything the rule is asserted through for it.
struct Row {
    /// The case name every assertion of the row names.
    case: &'static str,
    /// What is created and deleted.
    resource: Resource,
    /// The element the create answers the minted id in, which is also the element the
    /// unfiltered describe reports each item's id in.
    id_element: &'static str,
    /// The operation that deletes it, and the member naming the resource in that request.
    delete: &'static str,
    delete_param: &'static str,
    /// The operation that reads it back, and the id-list member of that request (ec2Query
    /// flattens the list into `<member>.N`).
    describe: &'static str,
    describe_param: &'static str,
    /// The code the describe refuses the deleted id with.
    not_found: &'static str,
    /// The sentence that refusal carries, as the template it is: fixed text around the id the
    /// REQUEST sent, which is the only variable part of it.
    message: fn(&str) -> String,
}

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn reported(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Create the row's resource through the operations that own it, and answer the id the SERVICE
/// minted for it.
async fn create(case: &str, router: &MultiServiceRouter, row: &Row) -> String {
    let body = match row.resource {
        Resource::PublicIpv4Pool => ok(case, router, "CreatePublicIpv4Pool", &[]).await,
        Resource::InstanceEventWindow => {
            ok(
                case,
                router,
                "CreateInstanceEventWindow",
                &[
                    param("Name", "deleted-resource-rule"),
                    param("CronExpression", "* 0-4 * * 1"),
                ],
            )
            .await
        }
        Resource::AccessScopeAnalysis => {
            // An analysis is started against a scope, so the scope comes first — with no
            // content paths of its own, this row being about the ANALYSIS.
            let scope = ok(
                case,
                router,
                "CreateNetworkInsightsAccessScope",
                &[param("ClientToken", "scope-of-the-analysis-this-row-deletes")],
            )
            .await;
            let scope_id = reported(case, &scope, "networkInsightsAccessScopeId");
            ok(
                case,
                router,
                "StartNetworkInsightsAccessScopeAnalysis",
                &[
                    param("NetworkInsightsAccessScopeId", &scope_id),
                    param("ClientToken", "the-analysis-this-row-deletes"),
                ],
            )
            .await
        }
        Resource::IpamScope => {
            let ipam = ok(case, router, "CreateIpam", &[]).await;
            let ipam_id = reported(case, &ipam, "ipamId");
            ok(
                case,
                router,
                "CreateIpamScope",
                &[
                    param("IpamId", &ipam_id),
                    param("Description", "the scope this row deletes"),
                ],
            )
            .await
        }
        Resource::Ipam => ok(case, router, "CreateIpam", &[]).await,
    };
    reported(case, &body, row.id_element)
}

/// The row's describe, naming `id` and nothing else.
async fn read_by_id(router: &MultiServiceRouter, row: &Row, id: &str) -> (u16, String) {
    call(router, row.describe, &indexed(row.describe_param, &[id])).await
}

/// The ids the row's describe reports when it names none: what the account holds of that kind.
async fn listed_ids(case: &str, router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let body = ok(case, router, row.describe, &[]).await;
    elements(&body, row.id_element)
}

/// Look for the refusal the rule requires until it holds, or the deadline passes.
///
/// A removal is owed either INSIDE the delete or one transition span after it, depending on the
/// resource kind, and which of the two it is is not what this rule is about — so the condition
/// is waited for rather than a span of time, and the body of the refusal is answered for the
/// assertions that read its code and its sentence.
async fn refused_body(case: &str, router: &MultiServiceRouter, row: &Row, id: &str) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = read_by_id(router, row, id).await;
        if status == 400 {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {describe} to stop resolving the deleted {id}, got \
             {status} with body {body}",
            describe = row.describe
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that the row's describe refused `id` with the row's code and sentence.
fn assert_refusal(case: &str, row: &Row, id: &str, body: &str) {
    let code = error_code(body);
    assert_eq!(
        code, row.not_found,
        "case {case:?}: expected {expected} from {describe} for the deleted {id}, got {code:?} \
         in body {body}",
        expected = row.not_found,
        describe = row.describe
    );
    let expected = (row.message)(id);
    let message = error_message(body);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?} from {describe} for the deleted \
         {id}, got {message:?}",
        describe = row.describe
    );
}

/// THE RULE: a delete the service answered 200 takes the resource out of the state every later
/// read uses — the describe that names its id refuses with that kind's own code and sentence,
/// the unfiltered describe stops listing it, and reading it again does not bring it back.
#[tokio::test]
async fn a_deleted_resource_stops_resolving_for_the_describe_that_read_it() {
    // Rows: the four kinds a recorded delete-then-describe diverged on, plus the IPAM itself —
    // a kind no divergence named — so the rule is asserted beyond the kinds it was observed
    // through. Each sentence below is the one the service's own refusal for that code words:
    // fixed text around the id the request sent.
    let rows = [
        Row {
            case: "a public IPv4 pool",
            resource: Resource::PublicIpv4Pool,
            id_element: "poolId",
            delete: "DeletePublicIpv4Pool",
            delete_param: "PoolId",
            describe: "DescribePublicIpv4Pools",
            describe_param: "PoolId",
            not_found: INVALID_PUBLIC_IPV4_POOL_ID_NOT_FOUND,
            message: |id| format!("The pool ID '{id}' does not exist."),
        },
        Row {
            case: "an instance event window",
            resource: Resource::InstanceEventWindow,
            id_element: "instanceEventWindowId",
            delete: "DeleteInstanceEventWindow",
            delete_param: "InstanceEventWindowId",
            describe: "DescribeInstanceEventWindows",
            describe_param: "InstanceEventWindowId",
            not_found: INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
            // The describe resolves a LIST of ids, so its sentence carries the ids it could
            // not resolve as a bracketed list — here the one id the request named.
            message: |id| format!("Invalid Instance Event Window IDs [{id}]."),
        },
        Row {
            case: "a Network Access Scope analysis",
            resource: Resource::AccessScopeAnalysis,
            id_element: "networkInsightsAccessScopeAnalysisId",
            delete: "DeleteNetworkInsightsAccessScopeAnalysis",
            delete_param: "NetworkInsightsAccessScopeAnalysisId",
            describe: "DescribeNetworkInsightsAccessScopeAnalyses",
            describe_param: "NetworkInsightsAccessScopeAnalysisId",
            not_found: INVALID_NETWORK_INSIGHTS_ACCESS_SCOPE_ANALYSIS_ID_NOT_FOUND,
            message: |id| format!("{id} does not exist"),
        },
        Row {
            case: "an IPAM scope",
            resource: Resource::IpamScope,
            id_element: "ipamScopeId",
            delete: "DeleteIpamScope",
            delete_param: "IpamScopeId",
            describe: "DescribeIpamScopes",
            describe_param: "IpamScopeId",
            not_found: INVALID_IPAM_SCOPE_ID_NOT_FOUND,
            message: |id| format!("The IPAM scope ({id}) does not exist."),
        },
        Row {
            case: "an IPAM",
            resource: Resource::Ipam,
            id_element: "ipamId",
            delete: "DeleteIpam",
            delete_param: "IpamId",
            describe: "DescribeIpams",
            describe_param: "IpamId",
            not_found: INVALID_IPAM_ID_NOT_FOUND,
            message: |id| format!("The IPAM ({id}) does not exist."),
        },
    ];

    for row in &rows {
        let case = row.case;
        // One emulator per row: the rows are independent inputs rather than a sequence, so
        // what each row asserts about what the account holds is an assertion about that row.
        let router = emulator();
        let id = create(case, &router, row).await;

        // The resource is readable BEFORE the delete, so the refusal below is the delete's
        // doing rather than a describe that never reported it.
        let (status, body) = read_by_id(&router, row, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {describe} to report {id} before it is deleted, got \
             {status} with body {body}",
            describe = row.describe
        );
        assert_eq!(
            elements(&body, row.id_element),
            vec![id.clone()],
            "case {case:?}: expected {describe} to report exactly {id}, got body {body}",
            describe = row.describe
        );
        assert!(
            listed_ids(case, &router, row).await.contains(&id),
            "case {case:?}: expected the account's {describe} to list {id} before the delete",
            describe = row.describe
        );

        // The delete the rule is about: one this service ACCEPTED.
        ok(case, &router, row.delete, &[param(row.delete_param, &id)]).await;

        // Once the removal the delete owes has happened, the id stops resolving — under that
        // kind's own code, in the sentence AWS words for it, naming the id this run minted.
        let refusal = refused_body(case, &router, row, &id).await;
        assert_refusal(case, row, &id, &refusal);

        // The unfiltered describe no longer lists it either: the record is gone from the state
        // every read uses, not merely hidden from the read that names an id.
        let still_listed = listed_ids(case, &router, row).await;
        assert!(
            !still_listed.contains(&id),
            "case {case:?}: expected the account's {describe} to stop listing the deleted \
             {id}, got {still_listed:?}",
            describe = row.describe
        );

        // Reading it again answers the same refusal: a read does not bring a deleted resource
        // back, so a client's wait-until-gone loop that looks twice is told the same thing
        // twice.
        let (status, body) = read_by_id(&router, row, &id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a second {describe} of the deleted {id} to be refused, \
             got {status} with body {body}",
            describe = row.describe
        );
        assert_refusal(case, row, &id, &body);
    }
}
