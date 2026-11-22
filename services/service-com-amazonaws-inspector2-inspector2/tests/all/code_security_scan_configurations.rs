// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon Inspector code security scan configuration slice:
//! `CreateCodeSecurityScanConfiguration`, `GetCodeSecurityScanConfiguration`,
//! `ListCodeSecurityScanConfigurations` and `DeleteCodeSecurityScanConfiguration`.
//!
//! The rule under test: a code security scan configuration is a stored resource — the
//! create mints its `ScanConfigurationArn` and records the name, level, scan configuration,
//! scope settings and tags it was sent; the get reports exactly that record (with the tag
//! set as a map and the periodic schedule the service chose, both covered by
//! `code_security_periodic_schedule`) plus the instants it was created and last updated at;
//! the list reports one summary per stored record, with the scan settings flattened onto
//! the summary's own members, and pages at `maxResults`; and the delete removes the record,
//! so a later get or delete of the same ARN is refused as a missing resource. A create the
//! request boundary refuses stores nothing at all, so the list never reports it.
//!
//! Every request goes through the real `MultiServiceRouter` — routing, restJson1
//! deserialization, modelled-constraint validation, the handler, and restJson1
//! serialization — so what is asserted here is the wire behaviour a client sees.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The signed-request harness this crate's suites share: `router()` builds a router over
// its own engine, `post_in` sends one restJson1 POST signed for a region.
use crate::filters_and_enablement::{post_in, router};

/// The single account id every generated emulator presents
/// (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// A well-formed `ScanConfigurationArn` this emulator never minted — for the not-found
/// paths, which need an ARN that passes the member's modelled `@pattern` and names no
/// stored configuration.
/// Source: `smithy_query(query="shape", shape="ScanConfigurationArn")`.
const UNMINTED_ARN: &str = "arn:aws:inspector2:us-east-1:000000000000:owner/000000000000/\
                            codesecurity-configuration/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// The refusal text both reads answer for an ARN that names no stored configuration:
/// Amazon Inspector's own wording for a scan configuration that does not exist.
const NOT_FOUND_MESSAGE: &str = "Scan configuration not found";

/// `CreateCodeSecurityScanConfiguration` — `POST /codesecurity/scan-configuration/create`.
async fn create_in(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
) -> (u16, Option<String>, Value) {
    post_in(router, region, "/codesecurity/scan-configuration/create", body).await
}

/// `GetCodeSecurityScanConfiguration` — `POST /codesecurity/scan-configuration/get`.
async fn get(router: &MultiServiceRouter, arn: &str) -> (u16, Option<String>, Value) {
    post_in(
        router,
        "us-east-1",
        "/codesecurity/scan-configuration/get",
        &json!({ "scanConfigurationArn": arn }),
    )
    .await
}

/// `DeleteCodeSecurityScanConfiguration` — `POST /codesecurity/scan-configuration/delete`.
async fn delete(router: &MultiServiceRouter, arn: &str) -> (u16, Option<String>, Value) {
    post_in(
        router,
        "us-east-1",
        "/codesecurity/scan-configuration/delete",
        &json!({ "scanConfigurationArn": arn }),
    )
    .await
}

/// `ListCodeSecurityScanConfigurations` — `POST /codesecurity/scan-configuration/list`.
///
/// Both input members are bound to the QUERY STRING, so `query` is sent there and the body
/// carries nothing.
/// Source: `smithy_query(query="operation", operation="ListCodeSecurityScanConfigurations")`
/// — `nextToken` and `maxResults` are `httpQuery` members.
async fn list(router: &MultiServiceRouter, query: &str) -> (u16, Option<String>, Value) {
    let uri = if query.is_empty() {
        "/codesecurity/scan-configuration/list".to_string()
    } else {
        format!("/codesecurity/scan-configuration/list?{query}")
    };
    post_in(router, "us-east-1", &uri, &json!({})).await
}

/// Assert `arn` is a `ScanConfigurationArn` of `region` owned by `owner`, with the
/// hyphenated 36-character UUID id segment the real service mints.
///
/// Source: `smithy_query(query="shape", shape="ScanConfigurationArn")` — pattern
/// `^arn:(aws[a-zA-Z-]*)?:inspector2:[a-z]{2}(-gov)?-[a-z]+-\d{1}:\d{12}:owner/(\d{12}|o-[a-z0-9]{10,32})/codesecurity-configuration/[a-f0-9-]{36}$`.
fn assert_scan_configuration_arn(case: &str, arn: &str, region: &str, owner: &str) {
    let prefix =
        format!("arn:aws:inspector2:{region}:{owner}:owner/{owner}/codesecurity-configuration/");
    let id = arn.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected an ARN starting {prefix:?}, got {arn:?}")
    });
    assert_eq!(
        id.len(),
        36,
        "case {case:?}: expected a 36-character id segment, got {id:?}"
    );
    assert!(
        id.chars()
            .all(|char| char.is_ascii_hexdigit() && !char.is_ascii_uppercase() || char == '-'),
        "case {case:?}: id segment outside [a-f0-9-], got {id:?}"
    );
    let groups: Vec<usize> = id.split('-').map(str::len).collect();
    assert_eq!(
        groups,
        vec![8, 4, 4, 4, 12],
        "case {case:?}: expected the hyphenated UUID grouping, got {id:?}"
    );
}

/// One create request: a row of the table below.
struct Row {
    case: &'static str,
    region: &'static str,
    name: String,
    /// A `ConfigurationLevel` token.
    level: &'static str,
    /// The `CodeSecurityScanConfiguration` as sent.
    configuration: Value,
    /// The `PeriodicScanFrequency` a schedule expression is owed for — set for a row whose
    /// periodic settings name a frequency without one, so the service chooses the schedule
    /// itself (see `code_security_periodic_schedule`).
    chosen_schedule_for: Option<&'static str>,
    /// The `ScopeSettings` as sent, absent for a general scan configuration.
    scope_settings: Option<Value>,
    tags: Option<Value>,
}

/// The create requests the round-trip test drives, spanning what the rule generalises
/// over: both `ConfigurationLevel` tokens, every `RuleSetCategory` token and the modelled
/// `ruleSetCategories` length bounds (1 and 3), the periodic settings absent / frequency
/// only / frequency with a cron expression and every `PeriodicScanFrequency` token, the
/// continuous-integration settings absent / one event / both events (the modelled maximum),
/// scope settings absent (a general configuration) and present (a default one), tags
/// absent / empty / present, the `ScanConfigurationName` length bounds (1 and 60), and
/// four regions.
///
/// Sources: `smithy_query(query="shape", shape="ConfigurationLevel,RuleSetCategory,RuleSetCategories,PeriodicScanFrequency,ContinuousIntegrationScanEvent,ContinuousIntegrationScanSupportedEvents,ProjectSelectionScope,ScanConfigurationName")`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "account level, one rule set category, no periodic or CI settings",
            region: "us-east-1",
            // The shortest name the member's modelled length admits.
            name: "a".to_string(),
            level: "ACCOUNT",
            configuration: json!({ "ruleSetCategories": ["SAST"] }),
            chosen_schedule_for: None,
            scope_settings: None,
            tags: None,
        },
        Row {
            case: "organization level, every rule set category, weekly periodic scan, default scope",
            region: "eu-west-2",
            name: "org-wide.scan_config:v1$".to_string(),
            level: "ORGANIZATION",
            configuration: json!({
                "ruleSetCategories": ["SAST", "IAC", "SCA"],
                "periodicScanConfiguration": { "frequency": "WEEKLY" },
            }),
            // The create named the frequency and no expression, so the service chooses the
            // weekly schedule itself.
            chosen_schedule_for: Some("WEEKLY"),
            scope_settings: Some(json!({ "projectSelectionScope": "ALL" })),
            tags: Some(json!({ "owner": "security-team", "costCenter": "4711" })),
        },
        Row {
            case: "monthly periodic scan with a cron expression, one CI event, empty tags",
            region: "ap-southeast-1",
            name: "monthly-with-expression".to_string(),
            level: "ACCOUNT",
            configuration: json!({
                "ruleSetCategories": ["IAC", "SCA"],
                "periodicScanConfiguration": {
                    "frequency": "MONTHLY",
                    "frequencyExpression": "cron(0 0 15 * ? *)",
                },
                "continuousIntegrationScanConfiguration": {
                    "supportedEvents": ["PULL_REQUEST"],
                },
            }),
            // The create named its own schedule, so that is the one stored.
            chosen_schedule_for: None,
            scope_settings: None,
            tags: Some(json!({})),
        },
        Row {
            case: "periodic scanning off, both CI events, longest name",
            region: "us-west-2",
            // The longest name the member's modelled length admits (60 characters).
            name: "n".repeat(60),
            level: "ACCOUNT",
            configuration: json!({
                "ruleSetCategories": ["SCA"],
                "periodicScanConfiguration": { "frequency": "NEVER" },
                "continuousIntegrationScanConfiguration": {
                    "supportedEvents": ["PULL_REQUEST", "PUSH"],
                },
            }),
            // `NEVER` names no schedule, so there is no expression to choose.
            chosen_schedule_for: None,
            scope_settings: Some(json!({ "projectSelectionScope": "ALL" })),
            tags: Some(json!({ "purpose": "code-security" })),
        },
        Row {
            case: "organization level, CI scans only, no scope settings",
            region: "eu-west-2",
            name: "ci-only".to_string(),
            level: "ORGANIZATION",
            configuration: json!({
                "ruleSetCategories": ["SAST", "SCA"],
                "continuousIntegrationScanConfiguration": { "supportedEvents": ["PUSH"] },
            }),
            chosen_schedule_for: None,
            scope_settings: None,
            tags: None,
        },
    ]
}

/// The create body of one row.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "name": row.name,
        "level": row.level,
        "configuration": row.configuration,
    });
    if let Some(scope_settings) = &row.scope_settings {
        body["scopeSettings"] = scope_settings.clone();
    }
    if let Some(tags) = &row.tags {
        body["tags"] = tags.clone();
    }
    body
}

/// RULE: a created code security scan configuration is a stored record — the get reports
/// exactly the name, level, scan configuration, scope settings and tags the create was
/// sent, plus the instants it was created and last updated at; and the delete removes it,
/// so a later get or delete of its ARN is a 404.
#[tokio::test]
async fn a_created_configuration_is_read_back_as_stored_and_gone_after_delete() {
    for row in rows() {
        let case = row.case;
        let router = router();

        let (status, error_type, created) =
            create_in(&router, row.region, &create_body(&row)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateCodeSecurityScanConfiguration: expected 200, got {status} / \
             {created} ({error_type:?})"
        );
        let arn = created["scanConfigurationArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no scanConfigurationArn in {created}"))
            .to_string();
        assert_scan_configuration_arn(case, &arn, row.region, ACCOUNT);

        // The get reports the stored record, member by member.
        let (status, error_type, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetCodeSecurityScanConfiguration: expected 200, got {status} / {read} \
             ({error_type:?})"
        );
        assert_eq!(
            read["scanConfigurationArn"],
            json!(arn),
            "case {case:?}: expected the created ARN, got {read}"
        );
        assert_eq!(
            read["name"],
            json!(row.name),
            "case {case:?}: expected the name as sent, got {read}"
        );
        assert_eq!(
            read["level"],
            json!(row.level),
            "case {case:?}: expected the level as sent, got {read}"
        );
        // The configuration as sent, with the schedule the service chose for a row that
        // named a frequency without one taken out — that schedule is asserted against its
        // own rule, not against a value this test carries.
        let mut reported = read["configuration"].clone();
        if let Some(frequency) = row.chosen_schedule_for {
            let periodic = reported["periodicScanConfiguration"]
                .as_object_mut()
                .unwrap_or_else(|| panic!("case {case:?}: no periodicScanConfiguration in {read}"));
            let expression = periodic
                .remove("frequencyExpression")
                .unwrap_or(Value::Null);
            let expression = expression.as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected a chosen frequencyExpression in {read}")
            });
            crate::code_security_periodic_schedule::assert_schedule_matches(
                case,
                frequency,
                expression,
                crate::code_security_periodic_schedule::created_at(case, &read),
            );
        }
        assert_eq!(
            reported, row.configuration,
            "case {case:?}: expected the configuration as sent, got {read}"
        );
        match &row.scope_settings {
            // Absent for a general scan configuration, and as sent for a default one.
            None => assert_eq!(
                read["scopeSettings"],
                Value::Null,
                "case {case:?}: expected no scopeSettings member, got {read}"
            ),
            Some(scope_settings) => assert_eq!(
                read["scopeSettings"], *scope_settings,
                "case {case:?}: expected the scope settings as sent, got {read}"
            ),
        }
        // The tag map is always reported: the map the create carried, and an empty one for
        // a create that carried no `tags` member at all.
        let expected_tags = row.tags.clone().unwrap_or_else(|| json!({}));
        assert_eq!(
            read["tags"], expected_tags,
            "case {case:?}: expected tags {expected_tags}, got {read}"
        );
        let created_at = crate::code_security_periodic_schedule::created_at(case, &read);
        let last_updated_at = read["lastUpdatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: no lastUpdatedAt timestamp in {read}"));
        assert_eq!(
            created_at, last_updated_at,
            "case {case:?}: a configuration that was never updated reports its creation \
             instant as its last update, got {read}"
        );
        assert!(
            created_at > 0.0,
            "case {case:?}: expected an epoch timestamp, got {created_at}"
        );

        // A get is a pure read: a second one answers exactly the same thing.
        let (status, _, again) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a repeated get: expected 200, got {status} / {again}"
        );
        assert_eq!(
            again, read,
            "case {case:?}: a get must not change what a later caller sees, got {again}"
        );

        // The delete reports the ARN it removed.
        let (status, error_type, deleted) = delete(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteCodeSecurityScanConfiguration: expected 200, got {status} / \
             {deleted} ({error_type:?})"
        );
        assert_eq!(
            deleted["scanConfigurationArn"],
            json!(arn),
            "case {case:?}: expected the deleted ARN, got {deleted}"
        );

        // And the record is gone: both reads of it are a missing resource, whether the ARN
        // was deleted or never minted at all.
        for (attempt, candidate) in [("already deleted", arn.as_str()), ("never minted", UNMINTED_ARN)]
        {
            for (operation, answered) in [
                ("get", get(&router, candidate).await),
                ("delete", delete(&router, candidate).await),
            ] {
                let (status, error_type, body) = answered;
                assert_eq!(
                    status, 404,
                    "case {case:?}/{attempt}/{operation}: expected 404, got {status} / {body}"
                );
                assert_eq!(
                    error_type.as_deref(),
                    Some("ResourceNotFoundException"),
                    "case {case:?}/{attempt}/{operation}: expected ResourceNotFoundException, got \
                     {error_type:?} / {body}"
                );
                assert_eq!(
                    body["message"],
                    json!(NOT_FOUND_MESSAGE),
                    "case {case:?}/{attempt}/{operation}: unexpected refusal message, got {body}"
                );
            }
        }
    }
}

/// RULE: every create mints a NEW configuration with an ARN of its own — the ARN
/// identifies the record, so two configurations never share one, not even under the same
/// name, and deleting one leaves the others readable.
#[tokio::test]
async fn every_create_mints_a_distinct_configuration() {
    let router = router();
    // Rows differ along how many creates carry the SAME name: a repeated name is a second
    // configuration, not a second write to the first.
    let names = ["same", "same", "other", "same"];
    let mut created: Vec<(usize, String)> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let body = json!({
            "name": name,
            "level": "ACCOUNT",
            "configuration": { "ruleSetCategories": ["SAST"] },
            // One tag per create, so each record carries something only it carries.
            "tags": { "index": index.to_string() },
        });
        let (status, error_type, response) = create_in(&router, "us-east-1", &body).await;
        assert_eq!(
            status, 200,
            "case {name:?}/{index}: CreateCodeSecurityScanConfiguration: expected 200, got \
             {status} / {response} ({error_type:?})"
        );
        let arn = response["scanConfigurationArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}/{index}: no scanConfigurationArn in {response}"))
            .to_string();
        created.push((index, arn));
    }

    let arns: BTreeSet<&str> = created.iter().map(|(_, arn)| arn.as_str()).collect();
    assert_eq!(
        arns.len(),
        created.len(),
        "expected one ARN per create, got {created:?}"
    );

    // Each ARN reads back its OWN record.
    for (index, arn) in &created {
        let (status, _, read) = get(&router, arn).await;
        assert_eq!(
            status, 200,
            "case {index}: expected 200 for {arn:?}, got {status} / {read}"
        );
        assert_eq!(
            read["name"],
            json!(names[*index]),
            "case {index}: expected the name this ARN was created with, got {read}"
        );
        assert_eq!(
            read["tags"],
            json!({ "index": index.to_string() }),
            "case {index}: expected the tags this ARN was created with, got {read}"
        );
    }

    // Deleting one record leaves every other one readable.
    let (deleted_index, deleted_arn) = created.remove(1);
    let (status, _, deleted) = delete(&router, &deleted_arn).await;
    assert_eq!(
        status, 200,
        "case {deleted_index}: expected 200 deleting {deleted_arn:?}, got {status} / {deleted}"
    );
    for (index, arn) in &created {
        let (status, _, read) = get(&router, arn).await;
        assert_eq!(
            status, 200,
            "case {index}: expected 200 after deleting another configuration, got {status} / \
             {read}"
        );
    }
}

/// The summary `ListCodeSecurityScanConfigurations` owes for the configuration `row` was
/// created as, under `arn`: the record's identity, plus the scan settings FLATTENED onto
/// the summary's own members. The periodic members are read off what the get reports
/// nested, because a row that named a frequency without an expression runs on the schedule
/// the SERVICE chose (asserted against its own rule in `code_security_periodic_schedule`),
/// not on one this table carries.
///
/// Source: `smithy_query(query="shape", shape="CodeSecurityScanConfigurationSummary")` —
/// `scanConfigurationArn`, `name`, `ownerAccountId` and `ruleSetCategories` are required,
/// and `periodicScanFrequency`, `frequencyExpression`,
/// `continuousIntegrationScanSupportedEvents` and `scopeSettings` are each reported only
/// when the configuration holds that setting.
fn expected_summary(row: &Row, arn: &str, read: &Value) -> Value {
    let mut summary = json!({
        "scanConfigurationArn": arn,
        "name": row.name,
        "ownerAccountId": ACCOUNT,
        "ruleSetCategories": row.configuration["ruleSetCategories"].clone(),
    });
    let periodic = &read["configuration"]["periodicScanConfiguration"];
    for (flattened, nested) in [
        ("periodicScanFrequency", "frequency"),
        ("frequencyExpression", "frequencyExpression"),
    ] {
        if !periodic[nested].is_null() {
            summary[flattened] = periodic[nested].clone();
        }
    }
    let events = &row.configuration["continuousIntegrationScanConfiguration"]["supportedEvents"];
    if !events.is_null() {
        summary["continuousIntegrationScanSupportedEvents"] = events.clone();
    }
    if let Some(scope_settings) = &row.scope_settings {
        summary["scopeSettings"] = scope_settings.clone();
    }
    summary
}

/// The summaries of `listed`, ordered by ARN so two answers over the same records compare
/// equal whatever order they were reported in.
fn summaries(case: &str, listed: &Value) -> Vec<Value> {
    let mut reported = listed["configurations"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no configurations list in {listed}"))
        .clone();
    reported.sort_by_key(|summary| {
        summary["scanConfigurationArn"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    });
    reported
}

/// RULE: the list reports one summary per STORED configuration — the identity the create
/// minted it under and the scan settings flattened onto the summary's members — and nothing
/// for an account that holds none.
///
/// Every row is created in one region, so what is asserted here is the flattening and not
/// which regions a list spans.
#[tokio::test]
async fn list_reports_a_flattened_summary_of_every_stored_configuration() {
    let router = router();

    // An account that has created nothing lists an empty set rather than an absent member.
    let (status, error_type, listed) = list(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"no configurations\": expected 200, got {status} / {listed} ({error_type:?})"
    );
    assert_eq!(
        listed["configurations"],
        json!([]),
        "case \"no configurations\": expected an empty list, got {listed}"
    );
    assert_eq!(
        listed["nextToken"],
        Value::Null,
        "case \"no configurations\": expected no cursor, got {listed}"
    );

    let mut expected: Vec<Value> = Vec::new();
    for row in rows() {
        let case = row.case;
        let (status, error_type, created) =
            create_in(&router, "us-east-1", &create_body(&row)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateCodeSecurityScanConfiguration: expected 200, got {status} / \
             {created} ({error_type:?})"
        );
        let arn = created["scanConfigurationArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no scanConfigurationArn in {created}"))
            .to_string();
        let (status, _, read) = get(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetCodeSecurityScanConfiguration: expected 200, got {status} / {read}"
        );
        expected.push(expected_summary(&row, &arn, &read));
    }
    expected.sort_by_key(|summary| {
        summary["scanConfigurationArn"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    });

    let (status, error_type, listed) = list(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"every configuration\": expected 200, got {status} / {listed} ({error_type:?})"
    );
    assert_eq!(
        summaries("every configuration", &listed),
        expected,
        "case \"every configuration\": expected one flattened summary per stored \
         configuration, got {listed}"
    );
    assert_eq!(
        listed["nextToken"],
        Value::Null,
        "case \"every configuration\": every record fits one page, so no cursor is owed, got \
         {listed}"
    );

    // A list is a pure read: a second one answers exactly the same thing.
    let (status, _, again) = list(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"repeated list\": expected 200, got {status} / {again}"
    );
    assert_eq!(
        again, listed,
        "case \"repeated list\": a list must not change what a later caller sees, got {again}"
    );

    // A deleted record leaves the list, and the others stay in it.
    let removed = expected.remove(0);
    let arn = removed["scanConfigurationArn"].as_str().expect("an ARN");
    let (status, _, deleted) = delete(&router, arn).await;
    assert_eq!(
        status, 200,
        "case \"after a delete\": expected 200 deleting {arn:?}, got {status} / {deleted}"
    );
    let (status, _, listed) = list(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"after a delete\": expected 200, got {status} / {listed}"
    );
    assert_eq!(
        summaries("after a delete", &listed),
        expected,
        "case \"after a delete\": a deleted configuration leaves the list, got {listed}"
    );
}

/// RULE: `maxResults` truncates the page and the `nextToken` it returns fetches exactly the
/// remainder, so paging yields every stored configuration exactly once.
///
/// Rows differ along how many configurations are stored and how large a page is asked for,
/// including both published bounds of the modelled 1..100 range and a page larger than the
/// result set.
/// Source: `smithy_query(query="operation", operation="ListCodeSecurityScanConfigurations")`
/// — `maxResults` range 1..100.
#[tokio::test]
async fn list_pages_at_max_results_and_yields_every_configuration_once() {
    for (total, page_size) in [(1usize, 1i64), (4, 1), (4, 2), (4, 3), (4, 4), (3, 100)] {
        let case = format!("total={total} maxResults={page_size}");
        let router = router();
        let mut created: Vec<String> = Vec::new();
        for index in 0..total {
            let body = json!({
                "name": format!("configuration-{index}"),
                "level": "ACCOUNT",
                "configuration": { "ruleSetCategories": ["SAST"] },
            });
            let (status, error_type, response) = create_in(&router, "us-east-1", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateCodeSecurityScanConfiguration: expected 200, got {status} \
                 / {response} ({error_type:?})"
            );
            created.push(
                response["scanConfigurationArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no scanConfigurationArn in {response}"))
                    .to_string(),
            );
        }
        created.sort();

        let mut seen: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let query = match &next_token {
                Some(token) => format!("maxResults={page_size}&nextToken={token}"),
                None => format!("maxResults={page_size}"),
            };
            let (status, error_type, listed) = list(&router, &query).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListCodeSecurityScanConfigurations: expected 200, got {status} / \
                 {listed} ({error_type:?})"
            );
            let page = listed["configurations"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no configurations list in {listed}"));
            assert!(
                page.len() as i64 <= page_size,
                "case {case:?}: a page carried {} summaries",
                page.len()
            );
            for summary in page {
                seen.push(
                    summary["scanConfigurationArn"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: no ARN in {summary}"))
                        .to_string(),
                );
            }
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case:?}: paging did not terminate after {pages} pages"
            );
            next_token = listed["nextToken"].as_str().map(str::to_string);
            if next_token.is_none() {
                break;
            }
        }

        let mut paged = seen.clone();
        paged.sort();
        paged.dedup();
        assert_eq!(
            paged.len(),
            seen.len(),
            "case {case:?}: paging reported a configuration twice, got {seen:?}"
        );
        assert_eq!(
            paged, created,
            "case {case:?}: paging must yield every stored configuration exactly once, got \
             {seen:?}"
        );
    }
}

/// RULE: a `nextToken` this service did not mint names no position, so the request is
/// refused as invalid input rather than answered from the start of the list.
#[tokio::test]
async fn list_refuses_a_next_token_it_did_not_mint() {
    let router = router();
    // Rows differ along the FORM of the token: not hex at all, hex of bytes that are no ARN
    // of this service, hex of an ARN of another service, and an odd-length hex string.
    let tokens = [
        "zzzz",
        &::hex::encode("nonsense"),
        &::hex::encode("arn:aws:s3:::bucket"),
        "abc",
    ];
    for token in tokens {
        let (status, error_type, answer) = list(&router, &format!("nextToken={token}")).await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected 400, got {status} / {answer}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {token:?}: expected ValidationException, got {error_type:?} / {answer}"
        );
    }
}

/// RULE: a create the request boundary refuses stores NOTHING — no configuration is minted
/// for it, so the list reports none; and the same call with values the model admits
/// succeeds and is listed.
///
/// Rows differ along WHICH modelled constraint the request broke: an unmodelled token in a
/// list of enums, a list under its own `@length` minimum, an unmodelled scalar enum token, a
/// name outside its `@pattern`, and an unmodelled nested enum token.
/// Sources: `smithy_query(query="shape", shape="RuleSetCategory,RuleSetCategories,ConfigurationLevel,ScanConfigurationName,PeriodicScanFrequency")`.
#[tokio::test]
async fn a_refused_create_stores_no_configuration() {
    // Every row carries the SAME name, so what the list shows is whether the refused call
    // created anything under it.
    const NAME: &str = "refused-configuration";
    let rejected: [(&str, Value); 5] = [
        (
            "an unmodelled rule set category",
            json!({
                "name": NAME,
                "level": "ACCOUNT",
                "configuration": {
                    "ruleSetCategories": ["NOT_A_REAL_RULE_SET_CATEGORY"],
                    "periodicScanConfiguration": { "frequency": "WEEKLY" },
                },
            }),
        ),
        (
            "a rule set category list under its minimum",
            json!({
                "name": NAME,
                "level": "ACCOUNT",
                "configuration": { "ruleSetCategories": [] },
            }),
        ),
        (
            "an unmodelled configuration level",
            json!({
                "name": NAME,
                "level": "ACCOUNTS",
                "configuration": { "ruleSetCategories": ["SAST"] },
            }),
        ),
        (
            "a name outside its pattern",
            json!({
                "name": "a name with spaces",
                "level": "ACCOUNT",
                "configuration": { "ruleSetCategories": ["SAST"] },
            }),
        ),
        (
            "an unmodelled periodic scan frequency",
            json!({
                "name": NAME,
                "level": "ACCOUNT",
                "configuration": {
                    "ruleSetCategories": ["SAST"],
                    "periodicScanConfiguration": { "frequency": "FORTNIGHTLY" },
                },
            }),
        ),
    ];

    let router = router();
    for (case, body) in rejected {
        let (status, error_type, answer) = create_in(&router, "us-east-1", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} / {answer}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected ValidationException, got {error_type:?} / {answer}"
        );
        let (status, _, listed) = list(&router, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListCodeSecurityScanConfigurations: expected 200, got {status} / \
             {listed}"
        );
        assert_eq!(
            listed["configurations"],
            json!([]),
            "case {case:?}: a refused create must store nothing, got {listed}"
        );
    }

    // The same call with values the model admits is served, and the list reports exactly it.
    let body = json!({
        "name": NAME,
        "level": "ACCOUNT",
        "configuration": {
            "ruleSetCategories": ["SAST"],
            "periodicScanConfiguration": { "frequency": "WEEKLY" },
        },
    });
    let (status, error_type, created) = create_in(&router, "us-east-1", &body).await;
    assert_eq!(
        status, 200,
        "case \"admitted values\": expected 200, got {status} / {created} ({error_type:?})"
    );
    let arn = created["scanConfigurationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"admitted values\": no scanConfigurationArn in {created}"));
    let (status, _, listed) = list(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"admitted values\": expected 200, got {status} / {listed}"
    );
    let reported = summaries("admitted values", &listed);
    assert_eq!(
        reported.len(),
        1,
        "case \"admitted values\": expected exactly the created configuration, got {listed}"
    );
    assert_eq!(
        reported[0]["scanConfigurationArn"],
        json!(arn),
        "case \"admitted values\": expected the created ARN, got {listed}"
    );
    assert_eq!(
        reported[0]["name"],
        json!(NAME),
        "case \"admitted values\": expected the name as sent, got {listed}"
    );
    assert_eq!(
        reported[0]["ruleSetCategories"],
        json!(["SAST"]),
        "case \"admitted values\": expected the rule set categories as sent, got {listed}"
    );
    assert_eq!(
        reported[0]["periodicScanFrequency"],
        json!("WEEKLY"),
        "case \"admitted values\": expected the periodic frequency as sent, got {listed}"
    );
}
