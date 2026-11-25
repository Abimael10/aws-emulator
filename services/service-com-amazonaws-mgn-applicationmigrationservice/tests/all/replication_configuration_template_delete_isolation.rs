// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteReplicationConfigurationTemplate` call leaves behind, end to end over
//! HTTP.
//!
//! The rule these tests pin: the delete removes exactly the one replication configuration
//! template the request named and nothing else the account holds — every record of every
//! OTHER kind the service keeps for that account (applications, waves, connectors) is
//! reported unchanged by its own reader afterwards, and the account itself stays
//! initialized, so the next call of the account serves rather than being told the account
//! is uninitialized. The axis these rows differ along is WHICH other records the account
//! holds when the delete runs; the status, the refusals and the race belong to
//! `replication_configuration_template_delete.rs`.
//!
//! Every assertion goes through the router's `dispatch`, so it exercises the same restJson1
//! deserialize → handle → serialize path a client drives, and each record is created and
//! read back through the service's own operations.
//!
//! The harness (`Mgn`, the region and account constants) is the one
//! `application_lifecycle.rs` defines; the create body is the one
//! `replication_configuration_template_delete.rs` builds from the model's `@required`
//! members.

use serde_json::{json, Value};

use crate::application_lifecycle::Mgn;
use crate::replication_configuration_template_delete::minimal_request;

/// The operation under test, at its modelled `http.uri`
/// (`smithy_query(query="operation",operation="DeleteReplicationConfigurationTemplate")`:
/// `POST /DeleteReplicationConfigurationTemplate`, code 204).
const DELETE: &str = "DeleteReplicationConfigurationTemplate";

/// `CreateReplicationConfigurationTemplate`, likewise (code 201).
const CREATE: &str = "CreateReplicationConfigurationTemplate";

/// `DescribeReplicationConfigurationTemplates`, likewise (code 200).
const DESCRIBE: &str = "DescribeReplicationConfigurationTemplates";

/// The status the delete's modelled `@http(code: 204)` answer carries.
const NO_CONTENT: u16 = 204;

/// The `ssmInstanceID` a connector is created under: the model's `SsmInstanceID` pattern
/// `^(^i-(0-9a-fA-F){8,}|mi-[0-9a-f]{17})$`
/// (`smithy_query(query="operation",operation="CreateConnector")`).
const SSM_INSTANCE_ID: &str = "i-0123456789abcdefg";

/// A kind of record this service keeps for an account BESIDE its replication configuration
/// templates — each one created and read back through its own operations, so what the
/// assertions below compare is what a client sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sibling {
    Application,
    Wave,
    Connector,
}

impl Sibling {
    /// The operation that creates one, and the body it is created with.
    fn create(self) -> (&'static str, Value) {
        match self {
            Sibling::Application => (
                "CreateApplication",
                json!({"name": "sap-erp", "description": "kept across the delete"}),
            ),
            Sibling::Wave => (
                "CreateWave",
                json!({"name": "cutover-wave", "tags": {"env": "dev"}}),
            ),
            Sibling::Connector => (
                "CreateConnector",
                json!({"name": "migration-connector", "ssmInstanceID": SSM_INSTANCE_ID}),
            ),
        }
    }

    /// The operation that reports it afterwards.
    fn reader(self) -> &'static str {
        match self {
            Sibling::Application => "ListApplications",
            Sibling::Wave => "ListWaves",
            Sibling::Connector => "ListConnectors",
        }
    }
}

/// The class these rows span: which other records the account holds when its template is
/// deleted — none, one of a kind, two kinds, and every kind at once.
fn sibling_cases() -> Vec<(&'static str, Vec<Sibling>)> {
    vec![
        ("the account holds nothing else", Vec::new()),
        (
            "the account also holds an application",
            vec![Sibling::Application],
        ),
        (
            "the account also holds a wave and a connector",
            vec![Sibling::Wave, Sibling::Connector],
        ),
        (
            "the account also holds an application, a wave and a connector",
            vec![Sibling::Application, Sibling::Wave, Sibling::Connector],
        ),
    ]
}

/// `CreateReplicationConfigurationTemplate`, asserting the modelled 201 and answering the
/// minted `replicationConfigurationTemplateID`.
async fn create_template(mgn: &Mgn, case: &str, subnet: &str) -> String {
    let (status, created) = mgn.call(CREATE, &minimal_request(subnet)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {CREATE} expected 201, got {status} with {created:?}"
    );
    created["replicationConfigurationTemplateID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no replicationConfigurationTemplateID in {created:?}")
        })
        .to_string()
}

/// Create one sibling record, asserting its create's modelled 201 and answering the record
/// it reported.
async fn create_sibling(mgn: &Mgn, case: &str, sibling: Sibling) -> Value {
    let (operation, request) = sibling.create();
    let (status, created) = mgn.call(operation, &request).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {operation} expected 201, got {status} with {created:?}"
    );
    created
}

/// The records `reader` reports for the caller's account, asserting the modelled 200.
async fn reported(mgn: &Mgn, case: &str, reader: &str) -> Vec<Value> {
    let (status, listed) = mgn.call(reader, &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {reader} expected 200, got {status} with {listed:?}"
    );
    listed["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no items array in {listed:?}"))
        .clone()
}

/// A delete removes the named template and leaves every other record of the account
/// exactly as its own reader reported it before the call.
#[tokio::test]
async fn a_delete_leaves_every_other_record_of_the_account_untouched() {
    for (case, siblings) in sibling_cases() {
        let mgn = Mgn::new();
        mgn.initialize().await;

        let mut before: Vec<(Sibling, Value)> = Vec::new();
        for sibling in &siblings {
            before.push((*sibling, create_sibling(&mgn, case, *sibling).await));
        }
        let template_id = create_template(&mgn, case, "subnet-0123456789abcdef").await;

        let (status, _, body) = mgn
            .call_reporting_error_type(
                DELETE,
                &json!({"replicationConfigurationTemplateID": &template_id}),
            )
            .await;
        assert_eq!(
            status, NO_CONTENT,
            "case {case:?}: {DELETE} expected {NO_CONTENT}, got {status} with {body:?}"
        );

        // The template is gone...
        let templates = reported(&mgn, case, DESCRIBE).await;
        assert!(
            templates.is_empty(),
            "case {case:?}: the deleted template must not be reported, got {templates:?}"
        );

        // ...and every other record is reported exactly as its create reported it.
        for (sibling, record) in &before {
            let reader = sibling.reader();
            let items = reported(&mgn, case, reader).await;
            assert_eq!(
                items.as_slice(),
                [record.clone()].as_slice(),
                "case {case:?}: {reader} must report the {sibling:?} record unchanged after \
                 the delete, got {items:?}"
            );
        }
    }
}

/// The delete leaves the ACCOUNT initialized: a call of the same account straight after it
/// serves rather than being refused as uninitialized, and the template the delete removed
/// does not come back with it.
#[tokio::test]
async fn a_delete_leaves_the_account_initialized() {
    let mgn = Mgn::new();
    mgn.initialize().await;

    // Three rounds, so the rule is pinned as the account's standing state rather than as a
    // one-off answer to the first delete.
    let mut removed: Vec<String> = Vec::new();
    for round in 0..3u32 {
        let case = format!("round {round}");
        let template_id = create_template(&mgn, &case, "subnet-0123456789abcdef").await;
        let (status, _, body) = mgn
            .call_reporting_error_type(
                DELETE,
                &json!({"replicationConfigurationTemplateID": &template_id}),
            )
            .await;
        assert_eq!(
            status, NO_CONTENT,
            "case {case:?}: {DELETE} expected {NO_CONTENT}, got {status} with {body:?}"
        );

        // A create of the same account serves immediately afterwards, which an
        // uninitialized account would not: that create is the next round's template.
        let listed = reported(&mgn, &case, DESCRIBE).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: no template may remain after the delete, got {listed:?}"
        );
        assert!(
            !removed.contains(&template_id),
            "case {case:?}: {template_id:?} was already removed in an earlier round, got \
             {removed:?}"
        );
        removed.push(template_id);
    }

    // A wave created after the deletes serves too: the account is still initialized for
    // every kind of record, not only for templates.
    let wave = create_sibling(&mgn, "after the deletes", Sibling::Wave).await;
    let waves = reported(&mgn, "after the deletes", Sibling::Wave.reader()).await;
    assert_eq!(
        waves.as_slice(),
        [wave].as_slice(),
        "after the deletes the account must still serve other creates, got {waves:?}"
    );
}
