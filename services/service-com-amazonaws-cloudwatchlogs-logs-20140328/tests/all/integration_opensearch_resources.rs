// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OpenSearch Service resources a CloudWatch Logs integration's
//! setup creates: the ENDPOINTS a read reports for the two of them a client addresses, and
//! the constraint set a refusal states for an `Arn`-typed member of the create's
//! configuration.
//!
//! The rules under test, one per test below:
//!
//! 1. A read of an integration reports, for each OpenSearch Service resource the setup
//!    created that a client ADDRESSES — the application that hosts the vended logs dashboards
//!    and the collection those dashboards query — the endpoint it is reached at: derived from
//!    that resource's own identity and the Region it lives in, in the form the member's
//!    `@pattern` admits (`^https://[\.\-_/#:A-Za-z0-9]+\.com$` on both
//!    `OpenSearchApplicationEndpoint` and `OpenSearchCollectionEndpoint`, from
//!    `smithy_query(query="shape",shape="OpenSearchApplicationEndpoint,OpenSearchCollectionEndpoint")`),
//!    the same on every read of one integration and different for two integrations.
//! 2. A clause refusing an `Arn`-typed member of the configuration states, at a SINGLE ARN
//!    member, the one constraint that value failed, and at a LIST of ARNs the element shape's
//!    WHOLE constraint set — the characters an ARN may be long at most, the characters it
//!    takes at least and the expression it must satisfy — in the order AWS states them,
//!    whichever of them an element broke.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a member
//! no serializer wrote cannot pass.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    INTEGRATION_TYPE_OPENSEARCH, MAX_ARN_CHARS, MIN_ARN_CHARS,
};

use crate::harness::{Logs, REGION};

/// The operations under test.
const PUT: &str = "PutIntegration";
const GET: &str = "GetIntegration";

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

/// A `PutIntegration` body creating `name` over the configuration `config`.
fn put_body(name: &str, config: Value) -> Value {
    json!({
        "integrationName": name,
        "integrationType": INTEGRATION_TYPE_OPENSEARCH,
        "resourceConfig": {"openSearchResourceConfig": config},
    })
}

/// The configuration every endpoint case creates its integration over, optionally naming an
/// EXISTING OpenSearch Service application rather than letting the setup create one.
fn config(application_arn: Option<&str>) -> Value {
    let mut config = json!({
        "dataSourceRoleArn": role_arn(),
        "dashboardViewerPrincipals": [role_arn()],
        "retentionDays": 7,
    });
    if let Some(application_arn) = application_arn {
        config["applicationArn"] = json!(application_arn);
    }
    config
}

/// One string member of a document.
fn member<'a>(document: &'a Value, key: &str) -> Option<&'a str> {
    document.get(key).and_then(Value::as_str)
}

/// The endpoint a read reports for the resource `resource` of the integration `name`.
async fn endpoint_of(logs: &Logs, name: &str, resource: &str, key: &str) -> String {
    let reported = logs.ok(GET, &json!({"integrationName": name})).await;
    let details = reported
        .get("integrationDetails")
        .and_then(|details| details.get("openSearchIntegrationDetails"))
        .cloned()
        .unwrap_or_else(|| panic!("expected openSearchIntegrationDetails, got {reported}"));
    details
        .get(resource)
        .and_then(|reported| member(reported, key))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("expected {resource}.{key}, got {details}"))
}

/// Whether `endpoint` is a value the modelled endpoint pattern admits:
/// `^https://[\.\-_/#:A-Za-z0-9]+\.com$`, applied character by character rather than through a
/// second regex engine.
fn in_the_modelled_endpoint_form(endpoint: &str) -> bool {
    let Some(host) = endpoint.strip_prefix("https://") else {
        return false;
    };
    !host.is_empty()
        && host.ends_with(".com")
        && host.chars().all(|character| {
            character.is_ascii_alphanumeric() || ".-_/#:".contains(character)
        })
}

/// RULE 1: both resources a client addresses report the endpoint they are reached at, derived
/// from the resource's own identity and the Region it lives in.
///
/// The rows span what the rule generalises over: the integration names the endpoint's first
/// label is derived from (the shortest one, one over every character class the name pattern
/// admits — `/` and `#` among them, which no host label may carry — and a mixed-case one), and
/// whether the create named an EXISTING application (whose own Region the application endpoint
/// then names) or let the setup create one in the integration's Region.
#[tokio::test]
async fn a_read_reports_the_endpoint_of_each_resource_a_client_addresses() {
    // The Region an existing application lives in, deliberately not the Region the
    // integrations below are created in.
    const OTHER_REGION: &str = "ap-northeast-1";
    // (case, the integration name, an existing applicationArn, the Region the application
    //  endpoint must name)
    let rows: [(&str, &str, Option<String>, &str); 5] = [
        ("the shortest name", "a", None, REGION),
        (
            "a name over every character class",
            "Ops.team-logs_2/dash#1",
            None,
            REGION,
        ),
        ("a mixed-case name", "DashBoards", None, REGION),
        (
            "an existing application in another Region",
            "dash",
            Some(format!(
                "arn:aws:opensearch:{OTHER_REGION}:{}:application/vended-logs",
                account()
            )),
            OTHER_REGION,
        ),
        (
            "an existing application in the integration's own Region",
            "dash",
            Some(format!(
                "arn:aws:opensearch:{REGION}:{}:application/vended-logs",
                account()
            )),
            REGION,
        ),
    ];
    for (case, name, application_arn, application_region) in rows {
        // One integration per account, so each row gets its own account's store.
        let logs = Logs::new();
        let created = logs
            .ok(PUT, &put_body(name, config(application_arn.as_deref())))
            .await;
        assert_eq!(
            member(&created, "integrationName"),
            Some(name),
            "case {case:?}: the create answers the integration it created, got {created}"
        );

        let application = endpoint_of(&logs, name, "application", "applicationEndpoint").await;
        let collection = endpoint_of(&logs, name, "collection", "collectionEndpoint").await;
        for (resource, endpoint) in [("application", &application), ("collection", &collection)] {
            assert!(
                in_the_modelled_endpoint_form(endpoint),
                "case {case:?}: expected {resource} endpoint in the modelled form, got \
                 {endpoint:?}"
            );
        }
        // Each endpoint names the Region its own resource lives in: the application's is the
        // Region of the application the create named, the collection's the Region the
        // integration — and so the collection the setup created for it — was created in.
        assert!(
            application.contains(&format!(".{application_region}.")),
            "case {case:?}: expected the application endpoint in {application_region}, got \
             {application:?}"
        );
        assert!(
            collection.contains(&format!(".{REGION}.")),
            "case {case:?}: expected the collection endpoint in {REGION}, got {collection:?}"
        );
        // The two resources are reached at two addresses, each carrying its own resource's
        // identity rather than the integration's bare name.
        assert_ne!(
            application, collection,
            "case {case:?}: the application and the collection are two resources, got \
             {application:?} for both"
        );

        // A READ CHANGES NOTHING: a second read reports the same two endpoints, and each
        // endpoint is the one reported beside that resource's own ARN and id.
        let read_again = endpoint_of(&logs, name, "collection", "collectionEndpoint").await;
        assert_eq!(
            read_again, collection,
            "case {case:?}: expected the same collection endpoint on a second read, got \
             {read_again:?} after {collection:?}"
        );
    }

    // Two integrations are reached at two addresses: the endpoint carries the resource's
    // identity, so it distinguishes one account's integration from another's.
    let first = Logs::new();
    first.ok(PUT, &put_body("alpha", config(None))).await;
    let second = Logs::new();
    second.ok(PUT, &put_body("beta", config(None))).await;
    let alpha = endpoint_of(&first, "alpha", "collection", "collectionEndpoint").await;
    let beta = endpoint_of(&second, "beta", "collection", "collectionEndpoint").await;
    assert_ne!(
        alpha, beta,
        "case \"two integrations\": expected two endpoints, got {alpha:?} for both"
    );
}

/// The clause a configuration member at `member` is refused with.
fn config_clause(member: &str, requirement: &str) -> String {
    format!(
        "Value at 'resourceConfig.openSearchResourceConfig.{member}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// The requirement sentence for each constraint of the set, written out as AWS states it.
fn max_length_requirement() -> String {
    format!("Member must have length less than or equal to {MAX_ARN_CHARS}")
}
/// See [`max_length_requirement`].
fn min_length_requirement() -> String {
    format!("Member must have length greater than or equal to {MIN_ARN_CHARS}")
}
/// See [`max_length_requirement`].
fn pattern_requirement() -> String {
    r"Member must satisfy regular expression pattern: [\w#+=/:,.@-]*\*?".to_string()
}

/// The whole set a clause at a LIST of ARNs states, in the order AWS states it: the maximum
/// length, the minimum length and the expression.
fn arn_element_set() -> String {
    format!(
        "Member must satisfy constraint: [{}, {}, {}]",
        max_length_requirement(),
        min_length_requirement(),
        pattern_requirement()
    )
}

/// An ARN-shaped value of `characters` characters, every one of them inside the expression an
/// ARN must satisfy, for a case about the LENGTH bounds alone.
fn arn_of_length(characters: usize) -> String {
    let prefix = format!("arn:aws:iam::{}:role/", account());
    let mut value = prefix.clone();
    value.push_str(&"a".repeat(characters.saturating_sub(prefix.chars().count())));
    value.truncate(characters);
    assert_eq!(
        value.chars().count(),
        characters,
        "the case's own value is {characters} characters long"
    );
    value
}

/// RULE 2: the clause at a single ARN member states the constraint that value failed, and the
/// clause at a list of ARNs states the element shape's whole constraint set.
///
/// The rows span what the rule generalises over: WHICH constraint of the set the value breaks
/// (shorter than the minimum, longer than the maximum, outside the expression), each at the
/// single-ARN member and at the list member, at each bound and one past it, and a request
/// breaking one at each member at once.
#[tokio::test]
async fn an_arn_member_is_refused_with_the_constraint_set_of_an_arn() {
    assert_eq!(
        (MIN_ARN_CHARS, MAX_ARN_CHARS),
        (20, 2048),
        "the bounds this suite is written against"
    );
    let role = role_arn();
    let element_set = arn_element_set();
    // (case, the configuration, the clauses the refusal must state in order)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a role ARN one character short of an ARN",
            json!({
                "dataSourceRoleArn": arn_of_length(MIN_ARN_CHARS - 1),
                "dashboardViewerPrincipals": [role.clone()],
                "retentionDays": 7,
            }),
            vec![config_clause("dataSourceRoleArn", &min_length_requirement())],
        ),
        (
            "a role ARN one character past the maximum",
            json!({
                "dataSourceRoleArn": arn_of_length(MAX_ARN_CHARS + 1),
                "dashboardViewerPrincipals": [role.clone()],
                "retentionDays": 7,
            }),
            vec![config_clause("dataSourceRoleArn", &max_length_requirement())],
        ),
        (
            "a role ARN long enough but outside the expression",
            json!({
                "dataSourceRoleArn": "arn:aws:iam::0000 0000:role/App",
                "dashboardViewerPrincipals": [role.clone()],
                "retentionDays": 7,
            }),
            vec![config_clause("dataSourceRoleArn", &pattern_requirement())],
        ),
        (
            "a viewer principal one character short of an ARN",
            json!({
                "dataSourceRoleArn": role.clone(),
                "dashboardViewerPrincipals": [role.clone(), arn_of_length(MIN_ARN_CHARS - 1)],
                "retentionDays": 7,
            }),
            vec![config_clause("dashboardViewerPrincipals", &element_set)],
        ),
        (
            "a viewer principal one character past the maximum",
            json!({
                "dataSourceRoleArn": role.clone(),
                "dashboardViewerPrincipals": [arn_of_length(MAX_ARN_CHARS + 1)],
                "retentionDays": 7,
            }),
            vec![config_clause("dashboardViewerPrincipals", &element_set)],
        ),
        (
            "a viewer principal long enough but outside the expression",
            json!({
                "dataSourceRoleArn": role.clone(),
                "dashboardViewerPrincipals": ["arn:aws:iam::0000 0000:user/Viewer"],
                "retentionDays": 7,
            }),
            vec![config_clause("dashboardViewerPrincipals", &element_set)],
        ),
        (
            "one member of each kind, counted into one envelope",
            json!({
                "applicationArn": arn_of_length(MAX_ARN_CHARS + 1),
                "dataSourceRoleArn": "role",
                "dashboardViewerPrincipals": ["arn:aws:iam::0000 0000:user/Viewer"],
                "kmsKeyArn": "arn:aws:kms::0000 0000:key/k",
                "retentionDays": 7,
            }),
            vec![
                config_clause("applicationArn", &max_length_requirement()),
                config_clause("dashboardViewerPrincipals", &element_set),
                config_clause("dataSourceRoleArn", &min_length_requirement()),
                config_clause("kmsKeyArn", &pattern_requirement()),
            ],
        ),
    ];
    for (case, configuration, clauses) in rows {
        let logs = Logs::new();
        let (status, answered) = logs.call(PUT, &put_body("dash", configuration)).await;
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
    }

    // Each bound itself is INSIDE the set: a value AT the minimum and one AT the maximum are
    // not refused for their length or their characters. What becomes of each afterwards is
    // another question — the shortest is too short to parse as an ARN at all, the longest is a
    // role ARN and the create is served — but neither is answered a length or an expression
    // clause.
    for (case, characters) in [
        ("a role ARN at the minimum", MIN_ARN_CHARS),
        ("a role ARN at the maximum", MAX_ARN_CHARS),
    ] {
        let logs = Logs::new();
        let (status, answered) = logs
            .call(
                PUT,
                &put_body(
                    "dash",
                    json!({
                        "dataSourceRoleArn": arn_of_length(characters),
                        "dashboardViewerPrincipals": [role.clone()],
                        "retentionDays": 7,
                    }),
                ),
            )
            .await;
        let said = member(&answered, "message").unwrap_or_default().to_string();
        assert!(
            !said.contains("length") && !said.contains("regular expression"),
            "case {case:?}: a value at a bound is inside it, got {status} {said:?}"
        );
    }
}
