// SPDX-License-Identifier: CC0-1.0

//! The WHOLE sentence Amazon MQ refuses a value outside a modelled enumeration with,
//! driven over the wire.
//!
//! RULE: a value outside the set the model declares for an enum-typed member is refused
//! with HTTP 400 `BadRequestException` naming that member in `errorAttribute`, and a
//! sentence built from the request and the model per SITE:
//!
//! * a member a create DEFINES its resource with is refused with "<the member's subject>
//!   [<the value the request sent>] is invalid. Valid values are: [<the values the model
//!   declares for that member, comma-space separated>]";
//! * the engine type a CATALOG READ was asked to FILTER its answer on is refused with
//!   "Broker engine [<the value the request sent>] is invalid.", naming no accepted
//!   values.
//!
//! Nothing in either sentence varies per run: the rejected value comes from the request
//! and the accepted set from the model, so the same request is refused with the same
//! words every time.
//!
//! The table spans what the rule generalises over rather than one recorded request:
//! every enum-typed member of both creates (not only the `engineType` the reported
//! divergence cited), crossed with several forms of a value outside its set — each
//! declared value in the wrong case, a declared value with a suffix, a value that
//! belongs to another member's set, a value that is no word at all, and the empty
//! string — plus the engine-type filter of both catalog reads. The accepted side is
//! exercised too, so the check cannot be passing by refusing everything.
//!
//! The value sets are the model's own enumerations, re-exported by the crate
//! (`smithy_query(query="shape",
//! shape="EngineType,DeploymentMode,BrokerStorageType,AuthenticationStrategy,DataReplicationMode")`),
//! never a list invented here. The subjects are the members' own modelled descriptions,
//! quoted on each row.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::{
    AUTHENTICATION_STRATEGY_VALUES, BROKER_STORAGE_TYPE_VALUES, DATA_REPLICATION_MODE_VALUES,
    DEPLOYMENT_MODE_VALUES, ENGINE_TYPE_VALUES,
};

use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::configuration_name::configuration_body;
use crate::configuration_wire_form::engine_type_on_the_wire;

/// One enum-typed member a create defines its resource with: the operation and path that
/// carry it, the member's `jsonName`, the subject Amazon MQ names it by, and the set the
/// model declares.
struct MemberSite {
    operation: &'static str,
    path: &'static str,
    attribute: &'static str,
    subject: &'static str,
    allowed: &'static [&'static str],
}

/// Every enum-typed input member of the two creates. Each `subject` is the member's own
/// modelled description — `smithy_query(query="doc", shape="CreateBrokerRequest",
/// member="<Member>")`: "Required. The type of broker engine.", "Required. The broker's
/// deployment mode.", "The broker's storage type.", "Optional. The authentication
/// strategy used to secure the broker.", "Defines whether this broker is a part of a
/// data replication pair."
const MEMBER_SITES: &[MemberSite] = &[
    MemberSite {
        operation: "CreateBroker",
        path: "/v1/brokers",
        attribute: "engineType",
        subject: "Broker engine type",
        allowed: ENGINE_TYPE_VALUES,
    },
    MemberSite {
        operation: "CreateBroker",
        path: "/v1/brokers",
        attribute: "deploymentMode",
        subject: "Broker deployment mode",
        allowed: DEPLOYMENT_MODE_VALUES,
    },
    MemberSite {
        operation: "CreateBroker",
        path: "/v1/brokers",
        attribute: "storageType",
        subject: "Broker storage type",
        allowed: BROKER_STORAGE_TYPE_VALUES,
    },
    MemberSite {
        operation: "CreateBroker",
        path: "/v1/brokers",
        attribute: "authenticationStrategy",
        subject: "Broker authentication strategy",
        allowed: AUTHENTICATION_STRATEGY_VALUES,
    },
    MemberSite {
        operation: "CreateBroker",
        path: "/v1/brokers",
        attribute: "dataReplicationMode",
        subject: "Broker data replication mode",
        allowed: DATA_REPLICATION_MODE_VALUES,
    },
    MemberSite {
        operation: "CreateConfiguration",
        path: "/v1/configurations",
        attribute: "engineType",
        subject: "Broker engine type",
        allowed: ENGINE_TYPE_VALUES,
    },
    MemberSite {
        operation: "CreateConfiguration",
        path: "/v1/configurations",
        attribute: "authenticationStrategy",
        subject: "Broker authentication strategy",
        allowed: AUTHENTICATION_STRATEGY_VALUES,
    },
];

/// The paths of the two catalog reads that take `engineType` as a query-string filter
/// (`smithy_query(query="digest")`: `DescribeBrokerEngineTypes` GET
/// /v1/broker-engine-types, `DescribeBrokerInstanceOptions` GET
/// /v1/broker-instance-options).
const CATALOG_PATHS: &[(&str, &str)] = &[
    ("DescribeBrokerEngineTypes", "/v1/broker-engine-types"),
    ("DescribeBrokerInstanceOptions", "/v1/broker-instance-options"),
];

/// Values outside `allowed`, built FROM the declared set and from the other members'
/// sets rather than copied from any one request.
fn values_outside(allowed: &[&str]) -> Vec<String> {
    let mut outside: Vec<String> = allowed.iter().map(|value| value.to_lowercase()).collect();
    outside.push(format!("{}_EDGE", allowed[0]));
    // A value belonging to another member's set is outside this one for every member of
    // this table: no two of these enumerations share a value.
    let borrowed = if allowed[0] == DEPLOYMENT_MODE_VALUES[0] {
        ENGINE_TYPE_VALUES[0]
    } else {
        DEPLOYMENT_MODE_VALUES[0]
    };
    outside.push(borrowed.to_string());
    outside.push("42".to_string());
    outside.push(String::new());
    outside
}

/// A create body for `operation` carrying `attribute` set to `value`.
fn body_with(operation: &str, name: &str, attribute: &str, value: &str) -> Value {
    let mut body = match operation {
        "CreateBroker" => create_body(name),
        "CreateConfiguration" => configuration_body(name),
        other => panic!("case {other:?}: this table has no request builder for that operation"),
    };
    body[attribute] = json!(value);
    body
}

#[tokio::test]
async fn a_create_states_the_members_subject_and_the_values_it_accepts() {
    let mq = Mq::new("us-east-1");
    let mut rows = 0usize;

    for site in MEMBER_SITES {
        for (index, value) in values_outside(site.allowed).into_iter().enumerate() {
            let case = format!("{}.{} = {value:?}", site.operation, site.attribute);
            let name = format!("sentence-{}-{index}", site.attribute).to_lowercase();
            let body = body_with(site.operation, &name, site.attribute, &value);
            let (status, code, answer) = mq
                .call_with_code("POST", site.path, Some(body))
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {answer:?}"
            );
            assert_eq!(
                code, "BadRequestException",
                "case {case:?}: expected the modelled error code, got {code:?} {answer:?}"
            );
            assert_eq!(
                answer.get("errorAttribute").and_then(Value::as_str),
                Some(site.attribute),
                "case {case:?}: the refusal must name the member at fault, got {answer:?}"
            );
            let expected = format!(
                "{} [{value}] is invalid. Valid values are: [{}]",
                site.subject,
                site.allowed.join(", ")
            );
            assert_eq!(
                string_at(&answer, "/message"),
                expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
            rows += 1;
        }
    }

    assert_eq!(
        rows,
        MEMBER_SITES
            .iter()
            .map(|site| values_outside(site.allowed).len())
            .sum::<usize>(),
        "every member must be exercised against every form outside its set, covered {rows}"
    );
}

#[tokio::test]
async fn a_catalog_read_names_only_the_engine_the_filter_asked_about() {
    let mq = Mq::new("us-east-1");
    let mut rows = 0usize;

    for (operation, path) in CATALOG_PATHS {
        for value in values_outside(ENGINE_TYPE_VALUES) {
            // The empty filter narrows nothing, exactly like an omitted one, so it is
            // not a refusal on either catalog read and has no sentence to assert.
            if value.is_empty() {
                continue;
            }
            let case = format!("{operation} with engineType {value:?}");
            let (status, code, answer) = mq
                .call_with_code("GET", &format!("{path}?engineType={value}"), None)
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {answer:?}"
            );
            assert_eq!(
                code, "BadRequestException",
                "case {case:?}: expected the modelled error code, got {code:?} {answer:?}"
            );
            assert_eq!(
                answer.get("errorAttribute").and_then(Value::as_str),
                Some("engineType"),
                "case {case:?}: the refusal must name the member at fault, got {answer:?}"
            );
            let expected = format!("Broker engine [{value}] is invalid.");
            assert_eq!(
                string_at(&answer, "/message"),
                expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
            rows += 1;
        }
    }

    assert_eq!(
        rows,
        CATALOG_PATHS.len() * (values_outside(ENGINE_TYPE_VALUES).len() - 1),
        "both catalog reads must be exercised against every form outside the set, covered {rows}"
    );
}

/// The accepted side of the same check: a value the model DOES declare is served, and
/// the configuration it minted reads back carrying it — so the sentences above are a
/// refusal of values outside the set, not of every request.
#[tokio::test]
async fn a_value_inside_the_modelled_set_is_served_and_reads_back() {
    let mq = Mq::new("us-east-1");

    for engine_type in ENGINE_TYPE_VALUES {
        let case = format!("CreateConfiguration.engineType = {engine_type:?}");
        let name = format!("inside-{engine_type}").to_lowercase();
        let mut body = configuration_body(&name);
        body["engineType"] = json!(engine_type);
        let (status, code, created) = mq
            .call_with_code("POST", "/v1/configurations", Some(body))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {code:?} {created:?}"
        );
        let id = string_at(&created, "/id");
        assert!(
            !id.is_empty(),
            "case {case:?}: the answer must carry the minted id, got {created:?}"
        );

        let (status, code, read) = mq
            .call_with_code("GET", &format!("/v1/configurations/{id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the configuration to read back, got {status} {code:?} {read:?}"
        );
        assert_eq!(
            string_at(&read, "/name"),
            name,
            "case {case:?}: the stored configuration must carry the name the create sent, got {read:?}"
        );
        assert_eq!(
            string_at(&read, "/engineType"),
            engine_type_on_the_wire(engine_type),
            "case {case:?}: the stored configuration must carry the engine type the create \
             sent, got {read:?}"
        );
    }
}
