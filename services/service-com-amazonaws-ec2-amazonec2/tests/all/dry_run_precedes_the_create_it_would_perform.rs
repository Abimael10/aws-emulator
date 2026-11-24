// SPDX-License-Identifier: CC0-1.0

//! WHEN the `DryRun` flag is answered, relative to the validation the request it would
//! perform has to pass — driven over the ec2Query wire the service actually speaks, through
//! the crate's shared [`super::ec2_query`] harness.
//!
//! RULE: `DryRun` "checks whether you have the required permissions for the action, WITHOUT
//! ACTUALLY MAKING THE REQUEST, and provides an error response. If you have the required
//! permissions, the error response is `DryRunOperation`" (the member's own documentation,
//! carried by every input shape that models it). The flag is therefore a question about the
//! CALLER, and the request it is set on is not performed at all: the verdict — HTTP 412
//! `DryRunOperation`, with EC2's own sentence — is answered BEFORE the operation validates
//! the parameters of the resource it would create, and nothing is stored. What still comes
//! first is the request's MODELLED shape, which the protocol boundary enforces ahead of any
//! handler: a request missing a member the model marks `required` is not a request the
//! action could have been performed for, so it is answered as the malformed request it is.
//!
//! The rows differ along what the rule generalises over: WHICH fault the create would
//! otherwise have refused — a parameter the request left out (at the top level and inside a
//! list entry), a value outside a published catalog, a value outside a published range, and a
//! time already past — crossed with a request that is wholly valid (whose dry run says the
//! same thing). The second test is the boundary the rule stops at: a request outside the
//! MODELLED shape, in each of the two ways it can be, answered ahead of the flag. Every row
//! of the first table is asked TWICE: once under `DryRun`, where the answer is the verdict,
//! and once without it, where the create's own refusal — or its success — is what the row
//! would have earned.
//!
//! `CreateCapacityReservationFleet` is the operation driven here because its create validates
//! several parameters of its own, so one operation spans the whole class; the Fleet slice's
//! own suite (`capacity_reservation_fleets.rs`) owns what each of those refusals SAYS.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::ops::types::{
    CAPACITY_RESERVATION_INSTANCE_PLATFORM_VALUES, FLEET_INSTANCE_MATCH_CRITERIA_VALUES,
};
use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    FLEET_ALLOCATION_STRATEGY_PRIORITIZED, MISSING_PARAMETER,
};

use super::ec2_query::{call, element, elements, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// An instance type every Region offers, for the one specification each row names.
const INSTANCE_TYPE: &str = "m5.large";

/// What the request a row sends asks for, before the row's own fault is applied: one
/// instance type specification in `zone`, the matching criteria the create takes as a
/// parameter, and a target capacity of two units.
fn complete(zone: &str) -> Params {
    vec![
        p("TotalTargetCapacity", "2"),
        p(
            "InstanceMatchCriteria",
            FLEET_INSTANCE_MATCH_CRITERIA_VALUES[0],
        ),
        p("AllocationStrategy", FLEET_ALLOCATION_STRATEGY_PRIORITIZED),
        p("InstanceTypeSpecification.1.InstanceType", INSTANCE_TYPE),
        p(
            "InstanceTypeSpecification.1.InstancePlatform",
            CAPACITY_RESERVATION_INSTANCE_PLATFORM_VALUES[0],
        ),
        p("InstanceTypeSpecification.1.AvailabilityZone", zone),
    ]
}

/// The request `complete` builds with every parameter whose name starts with `prefix`
/// removed — how a row states the parameter it leaves out.
fn without(params: &Params, prefix: &str) -> Params {
    params
        .iter()
        .filter(|(key, _)| !key.starts_with(prefix))
        .cloned()
        .collect()
}

/// The request `complete` builds with `key` set to `value` — how a row states the value it
/// gets wrong.
fn with(params: &Params, key: &str, value: &str) -> Params {
    let mut params = without(params, key);
    params.push(p(key, value));
    params
}

/// What a row expects of the call it makes WITHOUT the flag: the create's own refusal, or
/// its success.
enum WithoutTheFlag {
    /// HTTP 400 under this EC2 error code.
    Refused(&'static str),
    /// HTTP 200: the request would have succeeded, which is what the flag's own verdict says.
    Served,
}

/// One row: the case, the request it sends and what that request earns with no flag set.
struct Row {
    case: &'static str,
    params: Params,
    without_the_flag: WithoutTheFlag,
}

/// Every Fleet the account holds in [`REGION`], as `DescribeCapacityReservationFleets`
/// reports them.
async fn stored_fleet_ids(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeCapacityReservationFleets",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "the Fleet report must be served, got {status} with {body}"
    );
    elements(&body, "capacityReservationFleetId")
}

#[tokio::test]
async fn a_dry_run_is_answered_before_the_create_validates_what_it_would_create() {
    let router = super::ec2_query::emulator().0;
    // The zone a specification names has to be one the signed Region has, which the Region's
    // own report is what says.
    let (status, zones) = call(&router, REGION, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "the Region must report its Availability Zones, got {status} with {zones}"
    );
    let zone = element(&zones, "zoneName")
        .unwrap_or_else(|| panic!("the Region must report a zone, got {zones}"));
    let valid = complete(&zone);

    let rows: Vec<Row> = vec![
        Row {
            case: "a request the create would have served",
            params: valid.clone(),
            without_the_flag: WithoutTheFlag::Served,
        },
        Row {
            case: "a parameter of the create left out",
            params: without(&valid, "InstanceMatchCriteria"),
            without_the_flag: WithoutTheFlag::Refused(MISSING_PARAMETER),
        },
        Row {
            case: "no instance type specification at all",
            params: without(&valid, "InstanceTypeSpecification."),
            without_the_flag: WithoutTheFlag::Refused(MISSING_PARAMETER),
        },
        Row {
            case: "a specification naming no instance type",
            params: without(&valid, "InstanceTypeSpecification.1.InstanceType"),
            without_the_flag: WithoutTheFlag::Refused(MISSING_PARAMETER),
        },
        Row {
            case: "a specification naming no Availability Zone",
            params: without(&valid, "InstanceTypeSpecification.1.AvailabilityZone"),
            without_the_flag: WithoutTheFlag::Refused(MISSING_PARAMETER),
        },
        Row {
            case: "an allocation strategy outside the one published value",
            params: with(&valid, "AllocationStrategy", "not-a-strategy"),
            without_the_flag: WithoutTheFlag::Refused("InvalidParameterValue"),
        },
        Row {
            case: "a target capacity below the published minimum",
            params: with(&valid, "TotalTargetCapacity", "0"),
            without_the_flag: WithoutTheFlag::Refused("InvalidParameterValue"),
        },
        Row {
            case: "an end date already behind us",
            params: with(&valid, "EndDate", "2020-01-01T00:00:00Z"),
            without_the_flag: WithoutTheFlag::Refused("InvalidParameterValue"),
        },
    ];

    for row in &rows {
        let case = row.case;
        // Under the flag: the verdict, whatever the create would have said about the
        // request, and nothing performed.
        let mut dry = row.params.clone();
        dry.push(p("DryRun", "true"));
        let (status, body) = call(&router, REGION, "CreateCapacityReservationFleet", &dry).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (DRY_RUN_OPERATION_STATUS, DRY_RUN_OPERATION),
            "case {case:?}: expected the dry-run verdict, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            DRY_RUN_OPERATION_MESSAGE,
            "case {case:?}: expected EC2's sentence for the verdict, got {body}"
        );
        assert!(
            stored_fleet_ids(&router).await.is_empty(),
            "case {case:?}: a dry run must create no Fleet, got {:?}",
            stored_fleet_ids(&router).await
        );
    }

    // The same requests WITHOUT the flag: each earns the answer its own contents call for,
    // which is what the verdict above stood in place of.
    for row in &rows {
        let case = row.case;
        let (status, body) =
            call(&router, REGION, "CreateCapacityReservationFleet", &row.params).await;
        match row.without_the_flag {
            WithoutTheFlag::Served => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to be served, got {status} with {body}"
                );
                assert!(
                    element(&body, "capacityReservationFleetId").is_some(),
                    "case {case:?}: expected a Fleet id, got {body}"
                );
            }
            WithoutTheFlag::Refused(code) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, code),
                    "case {case:?}: expected HTTP 400 {code}, got {status} with {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_request_the_model_itself_refuses_is_answered_ahead_of_the_flag() {
    // The boundary the rule stops at: a request the MODEL refuses is not one the action
    // could have been performed for at all, so the protocol boundary answers it ahead of the
    // handler — with the flag set or not. The two rows are the two ways a request can be
    // outside the modelled shape: a member marked `required` that it leaves out
    // (`TotalTargetCapacity`), and a value outside a modelled enum (`InstanceType`).
    let router = super::ec2_query::emulator().0;
    let (_, zones) = call(&router, REGION, "DescribeAvailabilityZones", &[]).await;
    let zone = element(&zones, "zoneName")
        .unwrap_or_else(|| panic!("the Region must report a zone, got {zones}"));
    let valid = complete(&zone);
    let outside_the_model: [(&str, Params, &str); 2] = [
        (
            "a required member left out",
            without(&valid, "TotalTargetCapacity"),
            MISSING_PARAMETER,
        ),
        (
            "a value outside a modelled enum",
            with(
                &valid,
                "InstanceTypeSpecification.1.InstanceType",
                "not-an-instance-type",
            ),
            "InvalidParameterValue",
        ),
    ];

    for (case, params, code) in outside_the_model {
        for (flagged, flag) in [("with no flag", false), ("under DryRun", true)] {
            let case = format!("{case}, {flagged}");
            let mut params = params.clone();
            if flag {
                params.push(p("DryRun", "true"));
            }
            let (status, body) =
                call(&router, REGION, "CreateCapacityReservationFleet", &params).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, code),
                "case {case:?}: expected the modelled shape to be refused, got {status} with \
                 {body}"
            );
        }
    }
}
