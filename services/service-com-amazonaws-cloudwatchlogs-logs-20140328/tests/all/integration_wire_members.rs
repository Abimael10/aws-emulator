// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WIRE SHAPE of the integration create and the two integration
//! reads: the members CloudWatch Logs answers them with beyond the ones their modelled
//! output shapes declare, and the retention bound the create holds `retentionDays` to.
//!
//! The rules under test, one per group below:
//!
//! 1. A reported integration carries its name, its status and its type a second time under
//!    the member names `name`, `status` and `type`, on a read of one integration and on
//!    every summary of a listing — each repeating the documented member it is the other
//!    name of, so the two can never disagree.
//! 2. A read of one integration also carries an `integration` record (its name, its type and
//!    the configuration member of that type), a listing an `integrations` list holding one
//!    such record per summary — EMPTY when the account holds no integration rather than
//!    absent — and the create the `integrationConfig` it accepted.
//! 3. Every OpenSearch Service resource of an integration reports a `status.statusMessage`,
//!    which is empty for a resource whose status needs nothing added to it, and the
//!    collection reports its own name — the resource segment of its ARN.
//! 4. `retentionDays` is held to the bounds the SERVICE enforces, 1..3650 — not the 30 the
//!    model's `@range` stops at — every `Arn`-typed member of the configuration is held to
//!    the length an ARN takes, and EVERY member of one request that breaks one of those
//!    constraints is counted into one envelope, each clause naming the member path and the
//!    requirement it failed.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! member no serializer wrote cannot pass.
//!
//! The tables span what the rules generalise over rather than one request: an account
//! holding none, one and several integrations; integrations read while PROVISIONING and
//! after the clock has made them ACTIVE; and retentions at each bound the service enforces,
//! one inside each, one past each, and one between the model's stale maximum and the
//! service's.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::ops::types::OpenSearchResourceConfig;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    ComAmazonawsCloudwatchlogsLogs20140328State, IntegrationRecord, INTEGRATION_STATUS_ACTIVE,
    INTEGRATION_STATUS_PROVISIONING, INTEGRATION_TYPE_OPENSEARCH, MAX_OPENSEARCH_RETENTION_DAYS,
    MIN_OPENSEARCH_RETENTION_DAYS,
};

use crate::harness::{now_ms, Logs, REGION};

/// The operations under test.
const PUT: &str = "PutIntegration";
const GET: &str = "GetIntegration";
const LIST: &str = "ListIntegrations";

/// The code `PutIntegration` answers for a parameter it will not accept.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The account every ARN this emulator mints carries.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The IAM role `dataSourceRoleArn` names.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/CloudWatchOpenSearch", account())
}

/// A `PutIntegration` body naming `retention_days` days of retention.
fn put_body(name: &str, retention_days: i64) -> Value {
    json!({
        "integrationName": name,
        "integrationType": INTEGRATION_TYPE_OPENSEARCH,
        "resourceConfig": {"openSearchResourceConfig": {
            "dataSourceRoleArn": role_arn(),
            "dashboardViewerPrincipals": [role_arn()],
            "retentionDays": retention_days,
        }},
    })
}

/// Put one integration in the shared store directly, `age_ms` old, through the SAME record
/// constructor `PutIntegration` uses — so what the readers report is what a create stores.
///
/// The wire create admits one integration per account, which is that operation's own rule;
/// the wire SHAPE a read answers is about whatever the account holds, so a case needing
/// several integrations seeds them and then reads them back over the wire.
fn seed(logs: &Logs, name: &str, age_ms: i64) {
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let seq = store.take_integration_seq();
    let record = IntegrationRecord::new(
        REGION,
        name,
        INTEGRATION_TYPE_OPENSEARCH,
        &OpenSearchResourceConfig {
            kms_key_arn: None,
            data_source_role_arn: role_arn(),
            dashboard_viewer_principals: vec![role_arn()],
            application_arn: None,
            retention_days: 7,
        },
        now_ms() - age_ms,
        seq,
    );
    store.integrations.insert(name.to_string(), record);
}

/// One string member of a document.
fn member<'a>(document: &'a Value, key: &str) -> Option<&'a str> {
    document.get(key).and_then(Value::as_str)
}

/// The `integration` record a reported integration named `name` and in `status` must carry
/// beside the documented members.
fn expected_record(name: &str) -> Value {
    json!({
        "integrationConfig": {"openSearchIntegrationConfig": {}},
        "integrationName": name,
        "integrationType": INTEGRATION_TYPE_OPENSEARCH,
    })
}

/// Assert that `reported` carries each documented member a second time under its alias.
fn assert_aliases(case: &str, reported: &Value) {
    for (documented, alias) in [
        ("integrationName", "name"),
        ("integrationStatus", "status"),
        ("integrationType", "type"),
    ] {
        let expected = member(reported, documented);
        assert!(
            expected.is_some(),
            "case {case:?}: expected the documented {documented}, got {reported}"
        );
        assert_eq!(
            member(reported, alias),
            expected,
            "case {case:?}: expected {alias} to repeat {documented} ({expected:?}), got \
             {reported}"
        );
    }
}

/// RULES 1-3: a read of one integration carries the aliases, the `integration` record and,
/// on every OpenSearch Service resource, a status message — whether the integration is still
/// provisioning or the clock has made it active.
#[tokio::test]
async fn a_read_of_one_integration_carries_the_members_its_wire_response_carries() {
    // (case, how long ago the integration was created, the status that read must report)
    let rows = [
        (
            "an integration still provisioning",
            0_i64,
            INTEGRATION_STATUS_PROVISIONING,
        ),
        (
            "an integration the clock has made active",
            60 * 60 * 1_000,
            INTEGRATION_STATUS_ACTIVE,
        ),
    ];
    for (case, age_ms, expected_status) in rows {
        let logs = Logs::new();
        seed(&logs, "dash", age_ms);

        let reported = logs.ok(GET, &json!({"integrationName": "dash"})).await;
        assert_eq!(
            member(&reported, "integrationStatus"),
            Some(expected_status),
            "case {case:?}: expected the status {expected_status}, got {reported}"
        );
        assert_aliases(case, &reported);
        assert_eq!(
            reported.get("integration"),
            Some(&expected_record("dash")),
            "case {case:?}: expected the integration record {}, got {reported}",
            expected_record("dash")
        );

        let details = reported
            .get("integrationDetails")
            .and_then(|details| details.get("openSearchIntegrationDetails"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected openSearchIntegrationDetails, got {reported}")
            });
        // Every resource of the integration's setup, as the model's
        // `OpenSearchIntegrationDetails` declares them
        // (`smithy_query(query="shape",shape="OpenSearchIntegrationDetails")`).
        for resource in [
            "accessPolicy",
            "application",
            "collection",
            "dataSource",
            "encryptionPolicy",
            "lifecyclePolicy",
            "networkPolicy",
            "workspace",
        ] {
            let status = details
                .get(resource)
                .and_then(|reported| reported.get("status"))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected {resource}.status, got {details}")
                });
            assert_eq!(
                member(status, "statusMessage"),
                Some(""),
                "case {case:?}: expected an empty {resource}.status.statusMessage, got {status}"
            );
            assert!(
                member(status, "status").is_some(),
                "case {case:?}: expected {resource}.status.status, got {status}"
            );
        }
        // The collection's own name, which is the resource segment of its ARN, so the two
        // cannot name two collections. No ENDPOINT is reported for the application or the
        // collection: this emulator stands up neither resource, so there is no address a
        // client could open.
        let collection = details
            .get("collection")
            .unwrap_or_else(|| panic!("case {case:?}: expected the collection, got {details}"));
        assert_eq!(
            member(collection, "collectionName"),
            Some("dash-collection"),
            "case {case:?}: expected the collection's own name, got {collection}"
        );
        let collection_arn = member(collection, "collectionArn").unwrap_or_default();
        assert!(
            collection_arn.ends_with("dash-collection"),
            "case {case:?}: the name and the ARN must name one collection, got \
             {collection_arn:?}"
        );
        for (resource, key) in [
            ("application", "applicationEndpoint"),
            ("collection", "collectionEndpoint"),
        ] {
            assert_eq!(
                details.get(resource).and_then(|reported| reported.get(key)),
                None,
                "case {case:?}: no {resource}.{key} is reported, got {details}"
            );
        }
    }
}

/// RULES 1-2: a listing carries the aliases on every summary and an `integrations` list
/// holding one record per summary — an EMPTY list for an account that holds none.
#[tokio::test]
async fn a_listing_carries_one_record_per_summary_and_an_empty_list_for_none() {
    // (case, the integrations the account holds, in creation order)
    let rows: [(&str, &[&str]); 3] = [
        ("an account holding none", &[]),
        ("an account holding one", &["dash"]),
        (
            "an account holding several",
            &["first", "second", "third"],
        ),
    ];
    for (case, held) in rows {
        let logs = Logs::new();
        for name in held {
            seed(&logs, name, 60 * 60 * 1_000);
        }

        let page = logs.ok(LIST, &json!({})).await;
        let summaries = page
            .get("integrationSummaries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected integrationSummaries, got {page}"))
            .clone();
        assert_eq!(
            summaries.len(),
            held.len(),
            "case {case:?}: expected {} summary/summaries, got {page}",
            held.len()
        );
        for summary in &summaries {
            assert_aliases(case, summary);
        }

        let integrations = page
            .get("integrations")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the integrations list, got {page}")
            });
        let expected: Vec<Value> = held.iter().map(|name| expected_record(name)).collect();
        assert_eq!(
            integrations, &expected,
            "case {case:?}: expected the records {expected:?}, got {integrations:?}"
        );
    }
}

/// RULE 2: the create carries the configuration it accepted, beside the name and the status
/// it hands back.
#[tokio::test]
async fn the_create_carries_the_configuration_it_accepted() {
    for (case, name) in [
        ("the shortest name", "a"),
        ("a name over every character class", "Ops.team-logs_2/dash#1"),
    ] {
        let logs = Logs::new();
        let created = logs.ok(PUT, &put_body(name, 7)).await;
        assert_eq!(
            member(&created, "integrationName"),
            Some(name),
            "case {case:?}: a create answers the name it created, got {created}"
        );
        assert_eq!(
            member(&created, "integrationStatus"),
            Some(INTEGRATION_STATUS_PROVISIONING),
            "case {case:?}: a create hands back a PROVISIONING integration, got {created}"
        );
        assert_eq!(
            created.get("integrationConfig"),
            Some(&json!({})),
            "case {case:?}: expected the accepted integrationConfig, got {created}"
        );
    }
}

/// RULE 4: the retention the create accepts is bounded by what the SERVICE enforces, and a
/// value outside those bounds is refused naming the member path and the bound it failed.
#[tokio::test]
async fn the_retention_bound_is_the_one_the_service_enforces() {
    assert_eq!(
        (MIN_OPENSEARCH_RETENTION_DAYS, MAX_OPENSEARCH_RETENTION_DAYS),
        (1, 3650),
        "the bounds this suite is written against"
    );
    // (case, the retention, whether the create must be accepted)
    let rows: [(&str, i64, bool); 8] = [
        ("at the minimum", 1, true),
        ("just inside the minimum", 2, true),
        ("the model's stale maximum", 30, true),
        ("one past the model's stale maximum", 31, true),
        ("a realistic retention of a year", 365, true),
        ("at the maximum the service enforces", 3650, true),
        ("one below the minimum", 0, false),
        ("one past the maximum the service enforces", 3651, false),
    ];
    for (case, retention_days, accepted) in rows {
        // One integration per account, so each row gets its own account's store.
        let logs = Logs::new();
        let (status, body) = logs.call(PUT, &put_body("dash", retention_days)).await;
        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?} ({retention_days}): expected HTTP 200, got {status} {body}"
            );
            // The retention the create accepted is the one it stored.
            let state = logs
                .engine()
                .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
            let store = state.lock();
            let stored = store
                .integrations
                .get("dash")
                .map(|integration| i64::from(integration.retention_days));
            assert_eq!(
                stored,
                Some(retention_days),
                "case {case:?}: expected the stored retentionDays {retention_days}, got \
                 {stored:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?} ({retention_days}): expected HTTP 400, got {status} {body}"
        );
        let code = crate::harness::error_code(&body);
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {code} / {body}"
        );
        let bound = if retention_days < i64::from(MIN_OPENSEARCH_RETENTION_DAYS) {
            format!("Member must have value greater than or equal to {MIN_OPENSEARCH_RETENTION_DAYS}")
        } else {
            format!("Member must have value less than or equal to {MAX_OPENSEARCH_RETENTION_DAYS}")
        };
        let expected = format!(
            "1 validation error detected: Value at \
             'resourceConfig.openSearchResourceConfig.retentionDays' failed to satisfy \
             constraint: {bound}"
        );
        assert_eq!(
            member(&body, "message"),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {body}"
        );
        // Nothing was stored for a refused create.
        let state = logs
            .engine()
            .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
        let store = state.lock();
        assert!(
            store.integrations.is_empty(),
            "case {case:?}: a refused create stores nothing, got {} integration(s)",
            store.integrations.len()
        );
    }
}

/// The clause a configuration member at `member` is refused with.
fn config_clause(member: &str, requirement: &str) -> String {
    format!(
        "Value at 'resourceConfig.openSearchResourceConfig.{member}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// The requirement an ARN member shorter than an ARN failed: the 20 characters an ARN takes.
fn arn_length_requirement() -> String {
    "Member must have length greater than or equal to 20".to_string()
}

/// A `PutIntegration` body whose OpenSearch configuration is `config`.
fn put_config_body(name: &str, config: Value) -> Value {
    json!({
        "integrationName": name,
        "integrationType": INTEGRATION_TYPE_OPENSEARCH,
        "resourceConfig": {"openSearchResourceConfig": config},
    })
}

/// RULE 4 (the ARN members): every `Arn`-typed member of the configuration is held to the
/// length an ARN takes, with a LIST member stating that requirement as its elements', and
/// every member of one request that breaks a constraint is counted into one envelope in the
/// ascending order of the member names.
#[tokio::test]
async fn every_configuration_member_outside_a_constraint_is_counted_in_one_envelope() {
    let role = role_arn();
    let element_set = format!("Member must satisfy constraint: [{}]", arn_length_requirement());
    // (case, the configuration, the clauses the refusal must state in order)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a role ARN shorter than an ARN",
            json!({
                "dataSourceRoleArn": "not-an-arn",
                "dashboardViewerPrincipals": [role.clone()],
                "retentionDays": 7,
            }),
            vec![config_clause("dataSourceRoleArn", &arn_length_requirement())],
        ),
        (
            "a viewer principal shorter than an ARN",
            json!({
                "dataSourceRoleArn": role.clone(),
                "dashboardViewerPrincipals": [role.clone(), "viewer"],
                "retentionDays": 7,
            }),
            vec![config_clause("dashboardViewerPrincipals", &element_set)],
        ),
        (
            "both of them, counted into one envelope",
            json!({
                "dataSourceRoleArn": "not-an-arn",
                "dashboardViewerPrincipals": ["viewer"],
                "retentionDays": 7,
            }),
            vec![
                config_clause("dashboardViewerPrincipals", &element_set),
                config_clause("dataSourceRoleArn", &arn_length_requirement()),
            ],
        ),
        (
            "every ARN member and the retention together",
            json!({
                "applicationArn": "app",
                "dataSourceRoleArn": "role",
                "dashboardViewerPrincipals": ["viewer"],
                "kmsKeyArn": "key",
                "retentionDays": 0,
            }),
            vec![
                config_clause("applicationArn", &arn_length_requirement()),
                config_clause("dashboardViewerPrincipals", &element_set),
                config_clause("dataSourceRoleArn", &arn_length_requirement()),
                config_clause("kmsKeyArn", &arn_length_requirement()),
                config_clause(
                    "retentionDays",
                    "Member must have value greater than or equal to 1",
                ),
            ],
        ),
    ];
    for (case, config, clauses) in rows {
        let logs = Logs::new();
        let (status, answered) = logs.call(PUT, &put_config_body("dash", config)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {answered}"
        );
        let code = crate::harness::error_code(&answered);
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {code} / {answered}"
        );
        let expected = if clauses.len() == 1 {
            format!("1 validation error detected: {}", clauses[0])
        } else {
            format!(
                "{} validation errors detected: {}",
                clauses.len(),
                clauses.join("; ")
            )
        };
        assert_eq!(
            member(&answered, "message"),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {answered}"
        );
        let state = logs
            .engine()
            .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
        let store = state.lock();
        assert!(
            store.integrations.is_empty(),
            "case {case:?}: a refused create stores nothing, got {} integration(s)",
            store.integrations.len()
        );
    }

    // An ARN long enough to be one is read as the resource it names instead, so a
    // configuration every member of which is inside its constraints is served.
    let logs = Logs::new();
    let (status, answered) = logs
        .call(
            PUT,
            &put_config_body(
                "dash",
                json!({
                    "dataSourceRoleArn": role.clone(),
                    "dashboardViewerPrincipals": [role],
                    "retentionDays": 7,
                }),
            ),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"a configuration inside every constraint\": expected HTTP 200, got {status} \
         {answered}"
    );
}
