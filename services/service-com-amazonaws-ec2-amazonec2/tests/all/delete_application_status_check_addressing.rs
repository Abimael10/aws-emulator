// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ADDRESSING a `DeleteApplicationStatusCheck` answer reports:
//! which network device, IP version and IP scope the deleted check covered. Driven over the
//! wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=…`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **the delete answers the stored record, so every member the model
//! declares on `ApplicationStatusCheckResponseObject` that the check holds a value for is on
//! the wire** — including `deviceIndex`, `ipVersion` and `ipScope`
//! (`smithy_query(query="shape",shape="ApplicationStatusCheckResponseObject")`), each
//! echoing the configuration the check was left in rather than the values this one request
//! named. A client reconciling a check's configuration at delete time reads them there, and
//! an SDK that treats a modelled enum member as always present on this answer gets a value.
//!
//! The rows differ along what the stored addressing IS and how it got there: the documented
//! defaults a create fills in, every member named by the create, one of them named, each IP
//! version of the modelled catalog, a device index above the first, and a configuration a
//! LATER modify replaced — so the expectation is the stored state and not any one request's
//! parameters. The sibling suite `delete_application_status_check` pins what a delete DOES;
//! this one pins the members its answer carries.

use ec2::ops::types::{IP_SCOPE_ENUM_VALUES, IP_VERSION_ENUM_VALUES};
use ec2::state::{
    DEFAULT_APPLICATION_STATUS_CHECK_DEVICE_INDEX, DEFAULT_APPLICATION_STATUS_CHECK_IP_SCOPE,
    DEFAULT_APPLICATION_STATUS_CHECK_IP_VERSION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::call;
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for: a check's configuration is a fact of the check
/// rather than of the Region.
const REGION: &str = "us-west-2";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The three addressing members of one reported check: `(deviceIndex, ipVersion, ipScope)`,
/// each `None` when the element is absent — which is the difference this suite is about.
type Addressing = (Option<String>, Option<String>, Option<String>);

/// Those three members of the one check a create's or a delete's answer carries, read off
/// the envelope's own `applicationStatusCheck` element.
fn answered_addressing(case: &str, body: &str) -> Addressing {
    let parsed = document(case, body);
    let node = child(parsed.root_element(), "applicationStatusCheck")
        .unwrap_or_else(|| panic!("case {case:?}: no <applicationStatusCheck> in {body}"));
    addressing_of(node)
}

/// Those three members of the one check a report carries, read off the `item`s of the
/// report's own `applicationStatusCheckSet` — so a member of a set nested inside the record
/// can never be read as the check's.
fn reported_addressing(case: &str, body: &str) -> Addressing {
    let parsed = document(case, body);
    let item = set_items(parsed.root_element(), "applicationStatusCheckSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report carries no check: {body}"));
    addressing_of(item)
}

/// The three members of one reported check element.
fn addressing_of(node: roxmltree::Node<'_, '_>) -> Addressing {
    (
        child_text(node, "deviceIndex"),
        child_text(node, "ipVersion"),
        child_text(node, "ipScope"),
    )
}

/// One `CreateApplicationStatusCheck` call carrying `extra` beside the two required members,
/// as the id of the check it created.
async fn create_check(
    router: &MultiServiceRouter,
    case: &str,
    extra: &[(String, String)],
) -> String {
    let mut params = vec![p("Protocol", "http"), p("Port", "80")];
    params.extend_from_slice(extra);
    let (status, body) = call(router, REGION, "CreateApplicationStatusCheck", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateApplicationStatusCheck must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    child(parsed.root_element(), "applicationStatusCheck")
        .and_then(|check| child_text(check, "applicationStatusCheckId"))
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no check id: {body}"))
}

/// One row of the table: how the check's addressing is configured, and the three values the
/// delete must then report.
struct Row {
    case: &'static str,
    /// The addressing fields the CREATE names, if any.
    created_with: Vec<(String, String)>,
    /// The addressing fields a later MODIFY replaces, if any.
    modified_with: Vec<(String, String)>,
    device_index: String,
    ip_version: String,
    ip_scope: String,
}

/// The table: one row per stored addressing a delete can be asked about.
///
/// The defaults are this crate's own documented ones
/// (`DEFAULT_APPLICATION_STATUS_CHECK_DEVICE_INDEX` / `_IP_VERSION` / `_IP_SCOPE`), and the
/// versions and scopes named are the modelled catalogs' values (`IP_VERSION_ENUM_VALUES`,
/// `IP_SCOPE_ENUM_VALUES`), so every member of each catalog is covered rather than the one
/// value a recorded request happened to carry.
fn rows() -> Vec<Row> {
    let default_device = DEFAULT_APPLICATION_STATUS_CHECK_DEVICE_INDEX.to_string();
    let default_version = DEFAULT_APPLICATION_STATUS_CHECK_IP_VERSION.to_string();
    let default_scope = DEFAULT_APPLICATION_STATUS_CHECK_IP_SCOPE.to_string();
    let other_version = IP_VERSION_ENUM_VALUES
        .iter()
        .find(|value| **value != DEFAULT_APPLICATION_STATUS_CHECK_IP_VERSION)
        .copied()
        .unwrap_or("ipv6")
        .to_string();
    let scope = IP_SCOPE_ENUM_VALUES
        .first()
        .copied()
        .unwrap_or(DEFAULT_APPLICATION_STATUS_CHECK_IP_SCOPE)
        .to_string();

    let mut table = vec![
        Row {
            case: "no addressing named at all, so the documented defaults are stored",
            created_with: Vec::new(),
            modified_with: Vec::new(),
            device_index: default_device.clone(),
            ip_version: default_version.clone(),
            ip_scope: default_scope.clone(),
        },
        Row {
            case: "every addressing member named by the create",
            created_with: vec![
                p("DeviceIndex", "2"),
                p("IpVersion", other_version.clone()),
                p("IpScope", scope.clone()),
            ],
            modified_with: Vec::new(),
            device_index: "2".to_string(),
            ip_version: other_version.clone(),
            ip_scope: scope.clone(),
        },
        Row {
            case: "only the device index named, and one well above the first device",
            created_with: vec![p("DeviceIndex", "9")],
            modified_with: Vec::new(),
            device_index: "9".to_string(),
            ip_version: default_version.clone(),
            ip_scope: default_scope.clone(),
        },
        Row {
            case: "a configuration a later modify replaced",
            created_with: vec![p("DeviceIndex", "1")],
            modified_with: vec![p("DeviceIndex", "4"), p("IpVersion", other_version.clone())],
            device_index: "4".to_string(),
            ip_version: other_version.clone(),
            ip_scope: default_scope.clone(),
        },
    ];

    // One row per member of the modelled IP version catalog, so the answer is driven over
    // every version a check can be configured with rather than over one of them.
    for version in IP_VERSION_ENUM_VALUES {
        table.push(Row {
            case: "an IP version of the modelled catalog",
            created_with: vec![p("IpVersion", *version)],
            modified_with: Vec::new(),
            device_index: default_device.clone(),
            ip_version: (*version).to_string(),
            ip_scope: default_scope.clone(),
        });
    }
    table
}

/// RULE: a delete answers the deleted check's `deviceIndex`, `ipVersion` and `ipScope` as the
/// store holds them, and a describe that asks for deleted checks reports the same three.
#[tokio::test]
async fn a_delete_reports_the_addressing_the_deleted_check_covered() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in rows() {
        let case = row.case;
        let expected: Addressing = (
            Some(row.device_index.clone()),
            Some(row.ip_version.clone()),
            Some(row.ip_scope.clone()),
        );

        let id = create_check(&router, case, &row.created_with).await;
        if !row.modified_with.is_empty() {
            let mut params = vec![p("ApplicationStatusCheckId", id.clone())];
            params.extend_from_slice(&row.modified_with);
            let (status, body) =
                call(&router, REGION, "ModifyApplicationStatusCheck", &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ModifyApplicationStatusCheck must succeed, got {status} with \
                 body {body}"
            );
        }

        let (status, body) = call(
            &router,
            REGION,
            "DeleteApplicationStatusCheck",
            &[p("ApplicationStatusCheckId", id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteApplicationStatusCheck must succeed, got {status} with body \
             {body}"
        );
        let deleted = answered_addressing(case, &body);
        assert_eq!(
            deleted, expected,
            "case {case:?}: expected the delete to report (deviceIndex, ipVersion, ipScope) \
             {expected:?}, got {deleted:?} in {body}"
        );

        // The answer is the record the store holds: the check a describe reports inside its
        // deletion grace period carries exactly the same three.
        let (status, body) = call(
            &router,
            REGION,
            "DescribeApplicationStatusChecks",
            &[p("ApplicationStatusCheckId.1", id.clone()), p("IncludeAll", "true")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeApplicationStatusChecks must succeed, got {status} with \
             body {body}"
        );
        let described = reported_addressing(case, &body);
        assert_eq!(
            described, expected,
            "case {case:?}: expected the describe to report {expected:?} for the deleted \
             check, got {described:?} in {body}"
        );
    }
}
