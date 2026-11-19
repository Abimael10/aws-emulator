// SPDX-License-Identifier: CC0-1.0

//! What `GetGroupsForCapacityReservation` reports — driven over the wire the service actually
//! speaks (`ec2Query`, through the crate's shared [`super::ec2_query`] harness), with Resource
//! Groups mounted on the SAME engine, because a resource group and the membership in it are
//! that service's resources.
//!
//! The rule under test: the groups this operation reports for a Capacity Reservation are
//! exactly the resource groups whose MEMBERSHIP, as the service that owns groups reports it,
//! names that reservation's ARN — each with the group's own ARN and the account that owns the
//! group — in group-ARN order, paged by `MaxResults` and `NextToken`. A reservation no group
//! holds reports none, a grouping made after a read is seen by the next read, an ungrouping
//! takes the group away again, and the membership of ANOTHER resource is never reported for
//! this one.
//!
//! Each table row is a distinct input along what the rule generalises over — how many groups
//! hold the reservation, which of several reservations is asked about, whether the membership
//! was just added or just dropped, which page size, and which token — so no row is the
//! specification.

use std::sync::Arc;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_resourcegroups_ardi as resourcegroups;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, member};

/// The Region every call here is signed for. A resource group lives in the Region whose
/// endpoint created it, so both services are addressed in the same one.
const REGION: &str = "us-west-2";

/// The ec2Query action under test.
const ACTION: &str = "GetGroupsForCapacityReservation";

/// The service configuration type of a resource group that holds Capacity Reservations:
/// "You can only use this operation with the following groups: … AWS::EC2::CapacityReservationPool"
/// (`resource-groups:GroupResources`).
const CAPACITY_RESERVATION_POOL: &str = "AWS::EC2::CapacityReservationPool";

/// EC2 and Resource Groups over ONE shared engine, exactly as the assembled router mounts
/// them: the group a reservation is added to is that service's resource, created through its
/// own API, and EC2 reads the membership back from it over the emulator's in-process callback.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere; these
    // cases are about these services' behaviour.
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(resourcegroups::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// A `(key, value)` request parameter, as the ec2Query harness takes them.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One Capacity Reservation created through EC2's OWN `CreateCapacityReservation`, answering
/// the id and the ARN that call minted — so what a grouping names is a reservation the account
/// really holds.
async fn reservation(router: &MultiServiceRouter, case: &str, instance_type: &str) -> (String, String) {
    let (status, body) = call(
        router,
        REGION,
        "CreateCapacityReservation",
        &[
            p("InstanceType", instance_type),
            p("InstancePlatform", "Linux/UNIX"),
            p("InstanceCount", "2"),
            p("AvailabilityZone", "us-west-2a"),
            p("EndDateType", "unlimited"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCapacityReservation must answer 200, got {status} with body {body}"
    );
    let id = element(&body, "capacityReservationId")
        .unwrap_or_else(|| panic!("case {case:?}: no capacityReservationId in {body}"));
    let arn = element(&body, "capacityReservationArn")
        .unwrap_or_else(|| panic!("case {case:?}: no capacityReservationArn in {body}"));
    (id, arn)
}

/// One restJson1 call to Resource Groups, as that service's own wire: `POST <path>` with a JSON
/// body, signed (in scope only) for [`REGION`].
async fn resource_groups_call(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    body: &::serde_json::Value,
) -> ::serde_json::Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(path)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDTEST/20260101/{REGION}/resource-groups/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/json")
        .body(::bytes::Bytes::from(body.to_string()))
        .expect("a well-formed Resource Groups request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: ::serde_json::Value =
        ::serde_json::from_slice(response.body()).unwrap_or(::serde_json::Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: POST {path} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One Capacity Reservation pool group created through Resource Groups' OWN `CreateGroup`,
/// answering the group ARN that service minted.
async fn group(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = ::serde_json::json!({
        "Name": name,
        "Configuration": [{ "Type": CAPACITY_RESERVATION_POOL }],
    });
    let created = resource_groups_call(router, case, "/groups", &body).await;
    created["Group"]["GroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Group.GroupArn in {created}"))
        .to_string()
}

/// Add `resource_arn` to the group `group_arn` names, through Resource Groups' own
/// `GroupResources`, and assert that service accepted it.
async fn add_to_group(router: &MultiServiceRouter, case: &str, group_arn: &str, resource_arn: &str) {
    let body = ::serde_json::json!({ "Group": group_arn, "ResourceArns": [resource_arn] });
    let grouped = resource_groups_call(router, case, "/group-resources", &body).await;
    let succeeded: Vec<&str> = grouped["Succeeded"]
        .as_array()
        .map(|entries| entries.iter().filter_map(::serde_json::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        succeeded,
        vec![resource_arn],
        "case {case:?}: GroupResources must report the reservation grouped, got {grouped}"
    );
}

/// Take `resource_arn` back out of the group `group_arn` names, through Resource Groups' own
/// `UngroupResources`.
async fn remove_from_group(
    router: &MultiServiceRouter,
    case: &str,
    group_arn: &str,
    resource_arn: &str,
) {
    let body = ::serde_json::json!({ "Group": group_arn, "ResourceArns": [resource_arn] });
    let ungrouped = resource_groups_call(router, case, "/ungroup-resources", &body).await;
    let succeeded: Vec<&str> = ungrouped["Succeeded"]
        .as_array()
        .map(|entries| entries.iter().filter_map(::serde_json::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        succeeded,
        vec![resource_arn],
        "case {case:?}: UngroupResources must report the reservation ungrouped, got {ungrouped}"
    );
}

/// One `GetGroupsForCapacityReservation` call for `capacity_reservation_id`, carrying `extra`.
async fn groups_for(
    router: &MultiServiceRouter,
    capacity_reservation_id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("CapacityReservationId", capacity_reservation_id)];
    params.extend_from_slice(extra);
    call(router, REGION, ACTION, &params).await
}

/// The `(groupArn, ownerId)` pairs one report carries, in the order it reported them.
fn reported(case: &str, body: &str) -> Vec<(String, String)> {
    let group_arns = elements(body, "groupArn");
    let owner_ids = elements(body, "ownerId");
    assert_eq!(
        group_arns.len(),
        owner_ids.len(),
        "case {case:?}: every reported group carries an ownerId beside its groupArn: {body}"
    );
    group_arns.into_iter().zip(owner_ids).collect()
}

/// The report for `capacity_reservation_id`, as a 200 and its `(groupArn, ownerId)` pairs.
async fn reported_groups(
    router: &MultiServiceRouter,
    case: &str,
    capacity_reservation_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = groups_for(router, capacity_reservation_id, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
    );
    reported(case, &body)
}

/// The account every group of this emulator is owned by, which is what a reported group's
/// `ownerId` names.
fn owner() -> String {
    ::engine::emulator_account_id().to_string()
}

/// The rule: the groups reported are the memberships Resource Groups holds for the
/// reservation's ARN — none, one or several — and a group holding another reservation is never
/// reported for this one.
#[tokio::test]
async fn the_report_is_the_resource_group_membership_of_the_reservation() {
    let router = emulator();

    // (case, how many groups hold the reservation): the rows differ in the count the rule
    // generalises over, from the reservation no group holds to one several groups hold.
    let rows: &[(&str, usize)] = &[
        ("in no group at all", 0),
        ("in one group", 1),
        ("in two groups", 2),
        ("in three groups", 3),
    ];
    // One reservation of the account that is grouped nowhere in any row, so no row can report
    // a membership that is not the subject's own.
    let (bystander_id, bystander_arn) = reservation(&router, "bystander", "m5.large").await;
    let bystander_group = group(&router, "bystander", "bystander-pool").await;
    add_to_group(&router, "bystander", &bystander_group, &bystander_arn).await;

    for (case, held_by) in rows.iter().copied() {
        let (id, arn) = reservation(&router, case, "c5.large").await;
        let mut expected: Vec<(String, String)> = Vec::new();
        for index in 0..held_by {
            let name = format!("pool-{}-{index}", case.replace(' ', "-"));
            let group_arn = group(&router, case, &name).await;
            add_to_group(&router, case, &group_arn, &arn).await;
            expected.push((group_arn, owner()));
        }
        // Group-ARN order, which is the order the report is paged in.
        expected.sort();

        let actual = reported_groups(&router, case, &id).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the groups the reservation was added to, got {actual:?}"
        );
        assert!(
            !actual
                .iter()
                .any(|(group_arn, _)| *group_arn == bystander_group),
            "case {case:?}: the group holding another reservation must not be reported, \
             got {actual:?}"
        );
    }

    // The bystander's own report is unchanged by every grouping above: its one group.
    let bystander = reported_groups(&router, "bystander", &bystander_id).await;
    assert_eq!(
        bystander,
        vec![(bystander_group, owner())],
        "case \"bystander\": expected the one group that holds it, got {bystander:?}"
    );
}

/// The rule's other half: the report FOLLOWS the membership — a group added after a read is in
/// the next read, and an ungrouping takes it away again.
#[tokio::test]
async fn the_report_follows_the_membership_as_it_changes() {
    let router = emulator();
    let case = "grouped, then grouped again, then ungrouped";
    let (id, arn) = reservation(&router, case, "r5.large").await;

    let before = reported_groups(&router, case, &id).await;
    assert!(
        before.is_empty(),
        "case {case:?}: a reservation no group holds reports none, got {before:?}"
    );

    let first = group(&router, case, "lifecycle-pool-a").await;
    add_to_group(&router, case, &first, &arn).await;
    let after_first = reported_groups(&router, case, &id).await;
    assert_eq!(
        after_first,
        vec![(first.clone(), owner())],
        "case {case:?}: expected the group just added, got {after_first:?}"
    );

    let second = group(&router, case, "lifecycle-pool-b").await;
    add_to_group(&router, case, &second, &arn).await;
    let mut both = vec![(first.clone(), owner()), (second.clone(), owner())];
    both.sort();
    let after_second = reported_groups(&router, case, &id).await;
    assert_eq!(
        after_second, both,
        "case {case:?}: expected both groups, got {after_second:?}"
    );

    remove_from_group(&router, case, &first, &arn).await;
    let after_removal = reported_groups(&router, case, &id).await;
    assert_eq!(
        after_removal,
        vec![(second, owner())],
        "case {case:?}: expected only the group still holding it, got {after_removal:?}"
    );

    // Reading the groups changed nothing: the membership is still what Resource Groups holds.
    let read_again = reported_groups(&router, case, &id).await;
    assert_eq!(
        read_again, after_removal,
        "case {case:?}: a read must not change what the next caller sees, got {read_again:?}"
    );
}

/// The paging rule: `MaxResults` cuts the page, the `NextToken` resumes after it, a walk
/// reports every group once in group-ARN order, and the last page carries no token.
#[tokio::test]
async fn max_results_pages_the_report_and_the_token_resumes_it() {
    let router = emulator();

    // (case, page size): the rows differ in the page size the rule generalises over — one at a
    // time, two at a time, and a page wider than the membership — against the same four groups.
    let rows: &[(&str, usize)] = &[
        ("one group per page", 1),
        ("two groups per page", 2),
        ("a page wider than the membership", 10),
    ];
    let case = "four groups";
    let (id, arn) = reservation(&router, case, "m6i.large").await;
    let mut expected: Vec<(String, String)> = Vec::new();
    for index in 0..4 {
        let group_arn = group(&router, case, &format!("paged-pool-{index}")).await;
        add_to_group(&router, case, &group_arn, &arn).await;
        expected.push((group_arn, owner()));
    }
    expected.sort();

    for (case, page_size) in rows.iter().copied() {
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        // One more round than the membership can need, so a token that never ends is a
        // failure of this assertion rather than a loop.
        for _ in 0..expected.len() + 1 {
            let mut extra = vec![p("MaxResults", page_size.to_string())];
            if let Some(token) = token.as_deref() {
                extra.push(p("NextToken", token));
            }
            let (status, body) = groups_for(&router, &id, &extra).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
            );
            let page = reported(case, &body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page must carry at most MaxResults entries, got {page:?}"
            );
            walked.extend(page);
            token = member(&body, "nextToken").filter(|token| !token.is_empty());
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the last page of the walk must carry no NextToken"
        );
        assert_eq!(
            walked, expected,
            "case {case:?}: a walk reports every group once in group-ARN order, got {walked:?}"
        );
    }
}

/// One row of the refusal table: the case, the reservation the report is asked for, the extra
/// request members, and the HTTP status and error code the refusal carries.
type RefusalRow = (&'static str, String, Vec<(String, String)>, u16, &'static str);

/// The refusals: a token this operation did not mint for THIS reservation names no position in
/// its report, an id the account holds no reservation for is an error rather than an empty
/// report, and `DryRun` reports nothing at all.
#[tokio::test]
async fn refusals_of_a_report_that_cannot_be_served() {
    let router = emulator();
    let (first_id, first_arn) = reservation(&router, "tokens", "c6i.large").await;
    let (second_id, _) = reservation(&router, "tokens", "c6i.xlarge").await;
    for index in 0..2 {
        let group_arn = group(&router, "tokens", &format!("token-pool-{index}")).await;
        add_to_group(&router, "tokens", &group_arn, &first_arn).await;
    }
    // A token the operation itself minted, for the FIRST reservation's report.
    let (status, body) = groups_for(&router, &first_id, &[p("MaxResults", "1")]).await;
    assert_eq!(
        status, 200,
        "a first page must be served, got {status} with body {body}"
    );
    let minted = member(&body, "nextToken")
        .filter(|token| !token.is_empty())
        .expect("a first page of two groups carries a NextToken");

    // (case, the reservation the report is asked for, the extra members, the expected status
    // and code). The rows differ in what cannot be served: a token from another reservation's
    // report, a token no page ever issued, an id that is no reservation id, a well-formed id
    // the account holds nothing under, and a probe that must report nothing.
    let rows: &[RefusalRow] = &[
        (
            "a token minted for another reservation's report",
            second_id.clone(),
            vec![p("NextToken", minted.clone())],
            400,
            "InvalidPaginationToken",
        ),
        (
            "a token this service never issued",
            first_id.clone(),
            vec![p("NextToken", "not-a-token")],
            400,
            "InvalidPaginationToken",
        ),
        (
            "an id that is no Capacity Reservation id",
            "vol-0123456789abcdef0".to_string(),
            Vec::new(),
            400,
            "InvalidCapacityReservationId.Malformed",
        ),
        (
            "a well-formed id the account holds no reservation under",
            "cr-0eeeeeeeeeeeeeeee".to_string(),
            Vec::new(),
            400,
            "InvalidCapacityReservationId.Malformed",
        ),
        (
            // EC2 answers a permissions probe `DryRunOperation` with HTTP 412, which is this
            // service's own status for that code.
            "a permissions probe",
            first_id.clone(),
            vec![p("DryRun", "true")],
            412,
            "DryRunOperation",
        ),
    ];
    for (case, capacity_reservation_id, extra, expected_status, expected) in rows {
        let (status, body) = groups_for(&router, capacity_reservation_id, extra).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            *expected,
            "case {case:?}: expected the code {expected}, got body {body}"
        );
        assert!(
            elements(&body, "groupArn").is_empty(),
            "case {case:?}: a refusal reports no group, got body {body}"
        );
    }

    // The probe reported nothing and changed nothing: the first reservation still reports the
    // two groups that hold it.
    let after = reported_groups(&router, "tokens", &first_id).await;
    assert_eq!(
        after.len(),
        2,
        "case \"tokens\": expected the two groups that hold the reservation, got {after:?}"
    );
}
