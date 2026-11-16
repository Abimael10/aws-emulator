// SPDX-License-Identifier: CC0-1.0

//! How every read of a `Compute` writes the compute's status, and the endpoints a
//! registered compute reports.
//!
//! The rules the rows below span:
//!
//! * a compute whose fleet is not container-based reports its `ComputeStatus` as a word —
//!   the modelled enum value with its first character upper case and the rest lower — while
//!   the status is STORED in the enum form the request members that select on it carry;
//! * both endpoints a registered compute reports are addresses a process on that compute
//!   DIALS, so each is on the endpoint THIS emulator is served at, under the `wss://` scheme
//!   the service's endpoints carry, with the plane named in the path — never a host in
//!   Amazon GameLift Servers' own zone, which resolves to nothing here.
//!
//! Every operation that serves a `Compute` (`RegisterCompute`, `DescribeCompute`,
//! `ListCompute`) answers from that one projection, so the three can only agree.
//!
//! The harness (`GameLift`, `EMULATOR_BASE`, `REGION`, `anywhere_fleet`, `create_fleet`,
//! `compute_plane_endpoint`) is the one `hosting_loop.rs` defines and drives the production
//! dispatch path with.

use serde_json::{json, Value};

use crate::hosting_loop::{
    anywhere_fleet, compute_plane_endpoint, create_fleet, GameLift, EMULATOR_BASE, REGION,
};

/// Every `ComputeStatus` the model declares (`smithy_query query=shape
/// shape=ComputeStatus`), each paired with the non-enumerated form a compute of a fleet
/// that is not container-based reports it in — the member's own doc: "While the
/// ComputeStatus enum type is valid for Container based servers, the result may also
/// include other non-enumerated string values such as 'Active' for fleets which are not
/// Container-based."
const COMPUTE_STATUSES: [(&str, &str); 4] = [
    ("PENDING", "Pending"),
    ("ACTIVE", "Active"),
    ("TERMINATING", "Terminating"),
    ("IMPAIRED", "Impaired"),
];

/// The custom location the fleets below deploy to.
const RACK: &str = "custom-rack-1";

/// Register `name` on `fleet_id` and answer the reported `Compute`.
async fn register_compute(gamelift: &GameLift, fleet_id: &str, name: &str) -> Value {
    let registered = gamelift
        .ok(
            "RegisterCompute",
            json!({
                "FleetId": fleet_id,
                "ComputeName": name,
                "Location": RACK,
                "IpAddress": "10.0.7.21",
            }),
        )
        .await;
    registered["Compute"].clone()
}

/// The `Compute` `DescribeCompute` serves for `name` on `fleet_id`.
async fn describe_compute(gamelift: &GameLift, fleet_id: &str, name: &str) -> Value {
    let described = gamelift
        .ok(
            "DescribeCompute",
            json!({"FleetId": fleet_id, "ComputeName": name}),
        )
        .await;
    described["Compute"].clone()
}

/// The entries `ListCompute` serves for `request`, each paired with its compute's name so a
/// comparison does not depend on the order of the page.
async fn list_compute(gamelift: &GameLift, request: Value) -> Vec<(String, Value)> {
    let listed = gamelift.ok("ListCompute", request).await;
    listed["ComputeList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|compute| {
            let name = compute["ComputeName"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (name, compute)
        })
        .collect()
}

/// Overwrite the stored status of the compute `name` on `fleet_id` with `status`, the way
/// the service's own hosting workflow moves a compute between the statuses the enum
/// declares. Nothing a caller can request moves a registered compute out of `ACTIVE`, so
/// the record is set under the read path instead.
fn set_stored_status(gamelift: &GameLift, fleet_id: &str, name: &str, status: &str) {
    let state = gamelift.state();
    let mut state = state.lock();
    let compute = state
        .computes
        .get_mut(&(fleet_id.to_string(), name.to_string()))
        .unwrap_or_else(|| panic!("compute {name} must be registered on fleet {fleet_id}"));
    compute.status = status.to_string();
}

/// Every read of a compute of a non-container fleet reports the non-enumerated status and
/// both endpoints on the address this emulator is served at, and the three read paths agree
/// member for member.
///
/// The rows differ along what the rules generalise over: WHERE the emulator is served (so
/// an endpoint built from a fixed host would pass against at most one row), how many
/// computes the fleet holds (so the listing is asserted on one entry and on several), and
/// the form of the compute's own name.
#[tokio::test]
async fn every_compute_read_reports_the_non_container_status_and_this_emulators_endpoints() {
    struct Case {
        label: &'static str,
        /// The base URL the emulator under test is served at.
        base: &'static str,
        names: &'static [&'static str],
    }

    let cases = [
        Case {
            label: "the default in-process base, one compute",
            base: EMULATOR_BASE,
            names: &["rack-1-host-a"],
        },
        Case {
            label: "a loopback port, two computes",
            base: "https://127.0.0.1:9297",
            names: &["HOST-1", "rack-1/host-2"],
        },
        Case {
            label: "a named host on a port, three computes",
            base: "https://localhost:8443",
            names: &["host-a", "host-b-2", "host-c/inner-3"],
        },
        Case {
            label: "a routable bind, one compute",
            base: "https://10.0.0.7:9297",
            names: &["rack-1-host-z"],
        },
    ];

    for case in cases {
        let label = case.label;
        let gamelift = GameLift::served_at(case.base);
        let fleet_id = create_fleet(&gamelift, anywhere_fleet("status-host-fleet", &[RACK])).await;

        let mut registered: Vec<(String, Value)> = Vec::new();
        for name in case.names {
            let compute = register_compute(&gamelift, &fleet_id, name).await;
            let reported = json!({
                "ComputeStatus": compute["ComputeStatus"],
                "GameLiftAgentEndpoint": compute["GameLiftAgentEndpoint"],
                "GameLiftServiceSdkEndpoint": compute["GameLiftServiceSdkEndpoint"],
            });
            let expected = json!({
                "ComputeStatus": "Active",
                "GameLiftAgentEndpoint": compute_plane_endpoint(case.base, "gamelift-agent"),
                "GameLiftServiceSdkEndpoint":
                    compute_plane_endpoint(case.base, "gamelift-server-sdk"),
            });
            assert_eq!(
                reported, expected,
                "case {label:?} compute {name:?}: expected {expected}, got {reported} in \
                 {compute}"
            );
            // `GameLiftAgentEndpointOutput` and `GameLiftServiceSdkEndpointOutput` are both
            // modelled len 1..128, so neither reported address may run past that bound.
            for member in ["GameLiftAgentEndpoint", "GameLiftServiceSdkEndpoint"] {
                let value = compute[member].as_str().unwrap_or_default();
                assert!(
                    (1..=128).contains(&value.len()),
                    "case {label:?} compute {name:?}: {member} is modelled len 1..128, got \
                     {value:?}"
                );
            }
            registered.push(((*name).to_string(), compute));
        }

        // The describe path serves exactly the record the register reported.
        for (name, compute) in &registered {
            let described = describe_compute(&gamelift, &fleet_id, name).await;
            assert_eq!(
                &described, compute,
                "case {label:?} compute {name:?}: the describe must serve the registered \
                 record, got {described}"
            );
        }

        // So does the listing, for every entry of the page.
        let listed = list_compute(&gamelift, json!({"FleetId": &fleet_id})).await;
        assert_eq!(
            listed.len(),
            registered.len(),
            "case {label:?}: expected {} listed computes, got {listed:?}",
            registered.len()
        );
        for (name, compute) in &registered {
            let entry = listed
                .iter()
                .find(|(listed_name, _)| listed_name == name)
                .map(|(_, entry)| entry.clone())
                .unwrap_or(Value::Null);
            assert_eq!(
                &entry, compute,
                "case {label:?} compute {name:?}: the listing must serve the registered \
                 record, got {entry}"
            );
        }
    }
}

/// Each status the modelled enum declares reads back in its non-enumerated form, while the
/// value the store holds stays the enum form the request members that select on a status
/// carry: `ListCompute` narrowed to a `ListComputeInputStatus` selects the compute holding
/// that status and no other.
///
/// The rows are the whole `ComputeStatus` catalog, including the statuses no register
/// produces, so the rule is asserted beyond the one status a fresh compute carries.
#[tokio::test]
async fn each_modelled_compute_status_reads_back_in_its_non_enumerated_form() {
    /// The statuses `ListCompute` accepts as a selector (`smithy_query query=shape
    /// shape=ListComputeInputStatus`).
    const STATUS_SELECTORS: [&str; 2] = ["ACTIVE", "IMPAIRED"];

    let gamelift = GameLift::new();
    let fleet_id = create_fleet(&gamelift, anywhere_fleet("status-catalog-fleet", &[RACK])).await;
    let name = "catalog-host";
    register_compute(&gamelift, &fleet_id, name).await;

    for (stored, reported) in COMPUTE_STATUSES {
        set_stored_status(&gamelift, &fleet_id, name, stored);

        let described = describe_compute(&gamelift, &fleet_id, name).await;
        assert_eq!(
            described["ComputeStatus"],
            json!(reported),
            "case {stored:?}: the describe must report the status as {reported:?}, got \
             {described}"
        );

        let listed = list_compute(&gamelift, json!({"FleetId": &fleet_id})).await;
        let entry = listed
            .first()
            .map(|(_, entry)| entry.clone())
            .unwrap_or(Value::Null);
        assert_eq!(
            entry["ComputeStatus"],
            json!(reported),
            "case {stored:?}: the listing must report the status as {reported:?}, got {entry}"
        );

        // The stored enum form is what a status selector matches against.
        for selector in STATUS_SELECTORS {
            let selected =
                list_compute(&gamelift, json!({"FleetId": &fleet_id, "ComputeStatus": selector}))
                    .await;
            let names: Vec<&str> = selected
                .iter()
                .map(|(listed_name, _)| listed_name.as_str())
                .collect();
            let expected: Vec<&str> = if selector == stored {
                vec![name]
            } else {
                Vec::new()
            };
            assert_eq!(
                names, expected,
                "case {stored:?}: {selector:?} must select {expected:?}, got {names:?}"
            );
        }
    }
}

/// A compute registered through one Region's endpoint reports the same emulator-local
/// endpoints as one registered through another: the addresses a compute dials are the
/// emulator's own, so they do not vary with the Region the call was served in, while the
/// Region still names the ARNs the compute carries.
#[tokio::test]
async fn the_reported_endpoints_do_not_depend_on_the_region_the_call_was_served_in() {
    let gamelift = GameLift::served_at("https://127.0.0.1:9297");
    let fleet_id = create_fleet(&gamelift, anywhere_fleet("region-host-fleet", &[RACK])).await;

    let expected = json!({
        "GameLiftAgentEndpoint": compute_plane_endpoint("https://127.0.0.1:9297", "gamelift-agent"),
        "GameLiftServiceSdkEndpoint":
            compute_plane_endpoint("https://127.0.0.1:9297", "gamelift-server-sdk"),
    });
    for (case, region) in [
        ("the harness Region", REGION),
        ("another Region", "eu-central-1"),
        ("a third Region", "ap-southeast-2"),
    ] {
        let name = format!("host-{region}");
        let registered = gamelift
            .ok_in(
                region,
                "RegisterCompute",
                json!({
                    "FleetId": &fleet_id,
                    "ComputeName": &name,
                    "Location": RACK,
                    "IpAddress": "10.0.7.22",
                }),
            )
            .await;
        let compute = registered["Compute"].clone();
        let reported = json!({
            "GameLiftAgentEndpoint": compute["GameLiftAgentEndpoint"],
            "GameLiftServiceSdkEndpoint": compute["GameLiftServiceSdkEndpoint"],
        });
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected}, got {reported} in {compute}"
        );
    }
}
