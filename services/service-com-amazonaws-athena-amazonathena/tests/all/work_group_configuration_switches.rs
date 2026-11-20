// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the boolean switches a workgroup's configuration carries:
//! `GetWorkGroup` reports each one with the value it has in effect — the value a create
//! or an update set, Athena's own default for a create that carried no configuration at
//! all, and off for one no write ever set — rather than leaving the member out. Driven
//! over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots, so every assertion is about what an SDK
//! client observes.
//!
//! Each test states a rule and asserts it over a TABLE of inputs rather than one recorded
//! request: the rows differ along which switches the create names, which value each is
//! given, how many are named at once, and whether a later update sets or leaves one.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, router as build_router};

/// Every member `WorkGroupConfiguration` carries as a boolean, from the shape itself
/// (`smithy_query(query="shape",shape="com.amazonaws.athena#WorkGroupConfiguration")`:
/// the four members targeting `BoxedBoolean`). A read reports each of them, so this is
/// the set every row below is checked over — including
/// `EnforceWorkGroupConfiguration`, which no row of the create table names.
const CONFIGURATION_SWITCHES: [&str; 4] = [
    "EnforceWorkGroupConfiguration",
    "PublishCloudWatchMetricsEnabled",
    "RequesterPaysEnabled",
    "EnableMinimumEncryptionConfiguration",
];

/// The switches a workgroup Athena configured ITSELF comes up with — a create that carried
/// no `Configuration` at all: the workgroup's settings override the client's and query
/// metrics are published. Spelled out here rather than taken from the service, so the
/// expectation does not move with the code under test.
const DEFAULTED_SWITCHES: [(&str, bool); 2] = [
    ("EnforceWorkGroupConfiguration", true),
    ("PublishCloudWatchMetricsEnabled", true),
];

/// The `Configuration` `GetWorkGroup` reports for `name`.
async fn configuration(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, body) = call(router, "GetWorkGroup", &json!({ "WorkGroup": name })).await;
    assert_eq!(
        status, 200,
        "GetWorkGroup({name}) must answer 200, got {status} with {body:?}"
    );
    body["WorkGroup"]["Configuration"].clone()
}

/// The value every switch must read as, given the ones `named` were set to: each named
/// switch keeps its value and every other is off — `false` spelled out here rather than
/// taken from the service, so the expectation does not move with the code under test.
fn expected_switches(named: &[(&'static str, bool)]) -> BTreeMap<&'static str, bool> {
    let mut expected: BTreeMap<&'static str, bool> = CONFIGURATION_SWITCHES
        .iter()
        .map(|switch| (*switch, false))
        .collect();
    for (switch, value) in named {
        expected.insert(switch, *value);
    }
    expected
}

/// The value every switch must read as for a workgroup created with NO `Configuration` at
/// all: Athena's own defaults ([`DEFAULTED_SWITCHES`]) over the same off baseline.
fn expected_default_switches() -> BTreeMap<&'static str, bool> {
    let defaulted: Vec<(&'static str, bool)> = DEFAULTED_SWITCHES.to_vec();
    expected_switches(&defaulted)
}

/// Assert every switch of `configuration` reads as `expected` says, naming `case`.
fn assert_switches(case: &str, configuration: &Value, expected: &BTreeMap<&'static str, bool>) {
    for switch in CONFIGURATION_SWITCHES {
        let actual = &configuration[switch];
        let want = expected[switch];
        assert_eq!(
            actual,
            &json!(want),
            "case {case:?}: expected Configuration.{switch} to read as {want}, got \
             {actual:?} in {configuration:?}"
        );
    }
}

/// One row of the create table: the `Configuration` the create carries (null for a create
/// that carries none at all), and the switches it names.
struct CreateCase {
    case: &'static str,
    configuration: Value,
    named: Vec<(&'static str, bool)>,
}

/// The rows: a create with no configuration at all, one with a configuration that names
/// no switch, each switch named on its own in both values, and every switch named at
/// once.
fn create_cases() -> Vec<CreateCase> {
    let mut cases = vec![
        CreateCase {
            case: "a create with no Configuration member at all",
            configuration: Value::Null,
            named: Vec::new(),
        },
        CreateCase {
            case: "a configuration that names no switch",
            configuration: json!({
                "ResultConfiguration": { "OutputLocation": "s3://athena-results/prefix/" },
            }),
            named: Vec::new(),
        },
    ];
    // Each switch on its own, in both values: the rule is about the member being
    // reported, so a switch set to the value an unset one reports is a row of its own.
    for switch in CONFIGURATION_SWITCHES {
        for value in [true, false] {
            cases.push(CreateCase {
                case: if value {
                    "one switch set on"
                } else {
                    "one switch set off"
                },
                configuration: json!({ switch: value }),
                named: vec![(switch, value)],
            });
        }
    }
    cases.push(CreateCase {
        case: "every switch named at once, with alternating values",
        configuration: json!({
            "EnforceWorkGroupConfiguration": true,
            "PublishCloudWatchMetricsEnabled": false,
            "RequesterPaysEnabled": true,
            "EnableMinimumEncryptionConfiguration": false,
        }),
        named: vec![
            ("EnforceWorkGroupConfiguration", true),
            ("PublishCloudWatchMetricsEnabled", false),
            ("RequesterPaysEnabled", true),
            ("EnableMinimumEncryptionConfiguration", false),
        ],
    });
    cases
}

#[tokio::test]
async fn get_work_group_reports_every_configuration_switch_a_create_left_unset() {
    let router = build_router();

    for (index, case) in create_cases().into_iter().enumerate() {
        let name = format!("switches-{index}");
        let mut request = json!({ "Name": name });
        if !case.configuration.is_null() {
            request["Configuration"] = case.configuration.clone();
        }
        let (status, created) = call(&router, "CreateWorkGroup", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: the workgroup must be created, got {status} with {created:?}",
            case.case
        );

        let read = configuration(&router, &name).await;
        // A create that carried no `Configuration` at all is given Athena's own default
        // configuration; one that carried a configuration is stored as it was sent, so
        // every switch it left unset is off.
        let expected = if case.configuration.is_null() {
            expected_default_switches()
        } else {
            expected_switches(&case.named)
        };
        assert_switches(case.case, &read, &expected);

        // The switches are reported beside, not instead of, the configuration the create
        // carried: a member it named still reads back as it was sent.
        if let Some(sent) = case.configuration.get("ResultConfiguration") {
            assert_eq!(
                &read["ResultConfiguration"], sent,
                "case {:?}: expected the sent ResultConfiguration {sent:?}, got {:?}",
                case.case, read["ResultConfiguration"]
            );
        }
    }
}

/// One row of the update table: the case name, the `ConfigurationUpdates` to send, and
/// the switches in effect once it is applied.
type UpdateCase = (&'static str, Value, Vec<(&'static str, bool)>);

#[tokio::test]
async fn an_update_decides_the_switches_it_names_and_leaves_the_rest_in_effect() {
    let router = build_router();

    // (case, the ConfigurationUpdates to send, the switches in effect afterwards). The
    // workgroup starts from a create that named ONE switch, so each row shows an update
    // setting a switch, overriding the created one, and leaving the others alone.
    let cases: Vec<UpdateCase> = vec![
        (
            "an update that names no switch",
            json!({ "BytesScannedCutoffPerQuery": 10_000_000_i64 }),
            vec![("PublishCloudWatchMetricsEnabled", true)],
        ),
        (
            "one switch the create left unset",
            json!({ "RequesterPaysEnabled": true }),
            vec![
                ("PublishCloudWatchMetricsEnabled", true),
                ("RequesterPaysEnabled", true),
            ],
        ),
        (
            "the switch the create named, turned off",
            json!({ "PublishCloudWatchMetricsEnabled": false }),
            vec![("PublishCloudWatchMetricsEnabled", false)],
        ),
        (
            "two switches at once",
            json!({
                "EnableMinimumEncryptionConfiguration": true,
                "EnforceWorkGroupConfiguration": true,
            }),
            vec![
                ("PublishCloudWatchMetricsEnabled", true),
                ("EnableMinimumEncryptionConfiguration", true),
                ("EnforceWorkGroupConfiguration", true),
            ],
        ),
    ];

    for (index, (case, updates, expected)) in cases.into_iter().enumerate() {
        let name = format!("switch-updates-{index}");
        let (status, created) = call(
            &router,
            "CreateWorkGroup",
            &json!({
                "Name": name,
                "Configuration": { "PublishCloudWatchMetricsEnabled": true },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the workgroup must be created, got {status} with {created:?}"
        );

        let (status, updated) = call(
            &router,
            "UpdateWorkGroup",
            &json!({ "WorkGroup": name, "ConfigurationUpdates": updates }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateWorkGroup must answer 200, got {status} with {updated:?}"
        );

        let read = configuration(&router, &name).await;
        assert_switches(case, &read, &expected_switches(&expected));
    }
}

#[tokio::test]
async fn two_reads_of_one_work_group_report_the_same_switches() {
    let router = build_router();
    let name = "switches-are-derived";

    let (status, created) = call(&router, "CreateWorkGroup", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "the workgroup must be created, got {status} with {created:?}"
    );

    // A read reports the switches without writing them, so the second read of a
    // workgroup nothing wrote between answers exactly what the first did.
    let first = configuration(&router, name).await;
    let second = configuration(&router, name).await;
    assert_eq!(
        first, second,
        "expected two reads to report one configuration, got {first:?} then {second:?}"
    );
    assert_switches(
        "a workgroup created with no configuration",
        &second,
        &expected_default_switches(),
    );
}
