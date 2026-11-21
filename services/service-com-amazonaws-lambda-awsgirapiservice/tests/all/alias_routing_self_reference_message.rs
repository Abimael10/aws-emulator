// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a `RoutingConfig` that names the version the alias itself invokes is
//! refused with, on every operation that takes one.
//!
//! The rule under test: traffic shifting splits an alias's invocations between the version
//! it invokes and a SECOND version, so a request whose
//! `RoutingConfig.AdditionalVersionWeights` names the version the alias will invoke once
//! the call is applied names no second version and is refused with the modelled
//! `InvalidParameterValueException` (HTTP 400, from
//! `smithy_query(query="operation", operation="CreateAlias,UpdateAlias")`), whose message
//! is `Invalid function version <version>. Function version <version> is already included
//! in routing configuration.` — fixed text with that one version, taken from the request,
//! in both places. The version the sentence names is the version the call ASKS the alias to
//! invoke: the one the request states, and the one the alias already holds where the
//! request states none.
//!
//! Every row drives real HTTP requests through the router, and each refusal is followed by
//! a `GetAlias` read, so a refusal that nevertheless wrote something fails here. The rows
//! differ along what the rule generalises over: which operation takes the configuration
//! (`CreateAlias`, `UpdateAlias` stating the version, `UpdateAlias` omitting it), the
//! version named (single and two digit published version numbers), and the weight the
//! configuration gives it (each published bound of `Weight` and a value between them). The
//! control rows name a DIFFERENT published version and must be accepted, so the refusal is
//! shown to be about the self-reference rather than about routing configurations at large.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::alias_update::{
    create_alias, create_function, get_alias, router_on, update_alias, Answer,
};

/// The error code the model declares for a parameter value this operation cannot satisfy,
/// at HTTP 400.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// How many versions the functions under test publish, so a two-digit version number is
/// among the ones an alias can name.
const PUBLISHED_VERSIONS: usize = 10;

/// The bounds the model puts on `Weight`, the value of
/// `AliasRoutingConfiguration$AdditionalVersionWeights`
/// (`@range(min: 0.0, max: 1.0)`, from
/// `smithy_query(query="shape", shape="com.amazonaws.lambda#Weight")`).
const WEIGHT_MIN: f64 = 0.0;
const WEIGHT_MAX: f64 = 1.0;

/// The sentence AWS refuses a routing configuration that names the alias's own version
/// with: fixed text around the one version the request named, twice.
fn expected_message(version: &str) -> String {
    format!(
        "Invalid function version {version}. Function version {version} is already included in \
         routing configuration."
    )
}

/// A routing configuration giving `version` the share `weight`.
fn routing(version: &str, weight: f64) -> Value {
    json!({ "RoutingConfig": { "AdditionalVersionWeights": { version: weight } } })
}

/// Merge the members of `extra` into `body`, so a row states the version and the routing
/// configuration in one request.
fn with_members(body: Value, extra: Value) -> Value {
    let mut merged = body.as_object().cloned().unwrap_or_default();
    for (name, value) in extra.as_object().cloned().unwrap_or_default() {
        merged.insert(name, value);
    }
    Value::Object(merged)
}

/// Assert `answer` is the modelled 400 whose message is the sentence `version` fills.
fn assert_self_reference_refusal(case: &str, version: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_code()),
        (400, INVALID_PARAMETER_VALUE.to_string()),
        "case {case:?}: expected 400 {INVALID_PARAMETER_VALUE}, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.message(),
        expected_message(version),
        "case {case:?}: expected {:?}, got {:?}",
        expected_message(version),
        answer.message()
    );
}

/// One `CreateAlias` row: the version the alias would invoke and the weight the
/// configuration gives that same version.
struct CreateCase {
    case: &'static str,
    alias: &'static str,
    version: &'static str,
    weight: f64,
}

#[tokio::test]
async fn create_alias_refuses_routing_to_the_version_the_alias_invokes() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "ca-self-routing", PUBLISHED_VERSIONS).await;

    let rows = [
        CreateCase {
            case: "the first published version, a share between the bounds",
            alias: "self-one",
            version: "1",
            weight: 0.1,
        },
        CreateCase {
            case: "another published version, the whole share",
            alias: "self-two",
            version: "2",
            weight: WEIGHT_MAX,
        },
        CreateCase {
            case: "a two-digit published version, a zero share",
            alias: "self-ten",
            version: "10",
            weight: WEIGHT_MIN,
        },
        CreateCase {
            case: "a middle published version, a half share",
            alias: "self-seven",
            version: "7",
            weight: 0.5,
        },
    ];

    for row in rows {
        let body = with_members(
            json!({ "Name": row.alias, "FunctionVersion": row.version }),
            routing(row.version, row.weight),
        );
        let answer = create_alias(&router, "ca-self-routing", &body).await;
        assert_self_reference_refusal(row.case, row.version, &answer);

        // A refused create persists nothing, so the alias it would have created is still
        // absent when it is read back.
        let read = get_alias(&router, "ca-self-routing", row.alias).await;
        assert_eq!(
            read.status,
            404,
            "case {:?}: a refused CreateAlias must store no alias, GetAlias answered {} {}",
            row.case,
            read.status,
            read.text()
        );
    }

    // The control: the same request naming a DIFFERENT published version is accepted, so
    // the refusals above are about the self-reference alone.
    let accepted = create_alias(
        &router,
        "ca-self-routing",
        &with_members(
            json!({ "Name": "canary", "FunctionVersion": "1" }),
            routing("2", 0.25),
        ),
    )
    .await;
    assert_eq!(
        accepted.status,
        201,
        "case \"a second version that is not the alias's own\": CreateAlias must succeed, got \
         {} {}",
        accepted.status,
        accepted.text()
    );
}

/// One `UpdateAlias` row: the version the alias is created on, the version the update
/// states (`None` leaves the stored one in place), the version the routing configuration
/// names and the share it gives it.
struct UpdateCase {
    case: &'static str,
    alias: &'static str,
    created_version: &'static str,
    updated_version: Option<&'static str>,
    routed_version: &'static str,
    weight: f64,
}

#[tokio::test]
async fn update_alias_refuses_routing_to_the_version_the_alias_will_invoke() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "ua-self-routing", PUBLISHED_VERSIONS).await;

    let rows = [
        UpdateCase {
            case: "the update states the version its configuration names",
            alias: "states-version",
            created_version: "1",
            updated_version: Some("3"),
            routed_version: "3",
            weight: 0.2,
        },
        UpdateCase {
            case: "the update states no version, so its configuration names the stored one",
            alias: "keeps-version",
            created_version: "4",
            updated_version: None,
            routed_version: "4",
            weight: WEIGHT_MAX,
        },
        UpdateCase {
            case: "the update retargets to a two-digit version its configuration names",
            alias: "states-two-digit",
            created_version: "5",
            updated_version: Some("10"),
            routed_version: "10",
            weight: WEIGHT_MIN,
        },
        UpdateCase {
            case: "the update states no version, so a two-digit stored one is named",
            alias: "keeps-two-digit",
            created_version: "10",
            updated_version: None,
            routed_version: "10",
            weight: 0.75,
        },
    ];

    for row in rows {
        let created = create_alias(
            &router,
            "ua-self-routing",
            &json!({ "Name": row.alias, "FunctionVersion": row.created_version }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {:?}: the alias under test must be created, got {} {}",
            row.case,
            created.status,
            created.text()
        );
        let before = get_alias(&router, "ua-self-routing", row.alias).await;

        let mut body = routing(row.routed_version, row.weight);
        if let Some(version) = row.updated_version {
            body = with_members(body, json!({ "FunctionVersion": version }));
        }
        let answer = update_alias(&router, "ua-self-routing", row.alias, &body).await;
        assert_self_reference_refusal(row.case, row.routed_version, &answer);

        // A refused update changes nothing: the alias still invokes the version it was
        // created on, shifts no traffic, and carries the revision it carried.
        let after = get_alias(&router, "ua-self-routing", row.alias).await;
        assert_eq!(
            (
                after.string("FunctionVersion"),
                after.json().get("RoutingConfig").cloned(),
                after.string("RevisionId")
            ),
            (
                Some(row.created_version.to_string()),
                before.json().get("RoutingConfig").cloned(),
                before.string("RevisionId")
            ),
            "case {:?}: a refused UpdateAlias must leave the alias as it was, got {}",
            row.case,
            after.text()
        );
    }

    // The control: an update whose configuration names a version the alias does NOT invoke
    // is accepted and persisted.
    let accepted = update_alias(
        &router,
        "ua-self-routing",
        "states-version",
        &with_members(
            json!({ "FunctionVersion": "6" }),
            routing("8", 0.3),
        ),
    )
    .await;
    assert_eq!(
        accepted.status,
        200,
        "case \"a second version that is not the one the update targets\": UpdateAlias must \
         succeed, got {} {}",
        accepted.status,
        accepted.text()
    );
    let read = get_alias(&router, "ua-self-routing", "states-version").await;
    assert_eq!(
        (
            read.string("FunctionVersion"),
            read.json()
                .get("RoutingConfig")
                .and_then(|routing| routing.get("AdditionalVersionWeights"))
                .cloned()
        ),
        (
            Some("6".to_string()),
            Some(json!({ "8": 0.3 }))
        ),
        "case \"a second version that is not the one the update targets\": the accepted split \
         must be stored, got {}",
        read.text()
    );
}
