// SPDX-License-Identifier: CC0-1.0

//! How this service refuses the RUNTIME a request names for the code it carries.
//!
//! The rule asserted here, one level above the single request that exposed it:
//!
//! > `AppSyncRuntime$name` targets the enumerated `RuntimeName`, whose only value is
//! > `APPSYNC_JS` (`smithy_query(query="shape", shape="AppSyncRuntime,RuntimeName")`), and
//! > no operation that takes a runtime models `ValidationException` — the one client-input
//! > error they model is `BadRequestException` (400). So a `runtime.name` outside that value
//! > set is refused under `BadRequestException`, carrying the aggregated constraint report
//! > that names the member path and the permitted values and withholds the rejected value,
//! > whichever operation the runtime was named on.
//!
//! The rows span what the rule generalises over: the operation the runtime is named on
//! (every one whose input carries an `AppSyncRuntime`) and the rejected value.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router and the request helper are the crate's shared test harness, reused rather than
// copied.
use super::error_contract::{call, create_api, router, Answer};
use super::functions::create_data_source;

/// The one value `RuntimeName` declares.
const APPSYNC_JS: &str = "APPSYNC_JS";

/// The only `runtimeVersion` the runtime has: "Currently, the only allowed version is
/// `1.0.0`" (`AppSyncRuntime$runtimeVersion`).
const RUNTIME_VERSION: &str = "1.0.0";

/// The code every row carries, which is a whole APPSYNC_JS mapping: the refusal is about
/// the runtime the request names, so the code must not be what is wrong with it.
const CODE: &str = "export function request(ctx) { return {} }\n\
                    export function response(ctx) { return ctx.result }\n";

/// The values outside `RuntimeName`'s value set the rows are refused at: a plausible name,
/// a differently-cased one, and the empty string.
const REJECTED: &[&str] = &["NOT_A_RUNTIME", "appsync_js", "NODEJS", ""];

/// One request that names a runtime: the operation's method and path, and the body around
/// the `runtime` member.
struct Named {
    label: &'static str,
    method: &'static str,
    path: String,
    body: Value,
}

/// Every operation whose input carries an `AppSyncRuntime`, each as a request that is
/// well formed apart from the runtime's name.
async fn requests_naming_a_runtime(router: &MultiServiceRouter) -> Vec<Named> {
    let api_id = create_api(router, "runtime-name-refusal").await;
    create_data_source(router, &api_id, "ds_runtime").await;

    // The resolver and the function the two update rows restate, created with the one
    // runtime name the service accepts.
    let created = call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/functions"),
        Some(json!({
            "name": "fn_runtime",
            "dataSourceName": "ds_runtime",
            "code": CODE,
            "runtime": {"name": APPSYNC_JS, "runtimeVersion": RUNTIME_VERSION},
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case seed the function: expected CreateFunction 200, got {created:?}"
    );
    let function_id = created.body["functionConfiguration"]["functionId"]
        .as_str()
        .expect("the create mints a functionId")
        .to_string();
    let attached = call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "field",
            "dataSourceName": "ds_runtime",
            "code": CODE,
            "runtime": {"name": APPSYNC_JS, "runtimeVersion": RUNTIME_VERSION},
        })),
    )
    .await;
    assert_eq!(
        attached.status, 200,
        "case seed the resolver: expected CreateResolver 200, got {attached:?}"
    );

    vec![
        Named {
            label: "EvaluateCode",
            method: "POST",
            path: "/v1/dataplane-evaluatecode".to_string(),
            body: json!({"code": CODE, "context": "{}", "function": "request"}),
        },
        Named {
            label: "CreateFunction",
            method: "POST",
            path: format!("/v1/apis/{api_id}/functions"),
            body: json!({
                "name": "fn_refused",
                "dataSourceName": "ds_runtime",
                "code": CODE,
            }),
        },
        Named {
            label: "UpdateFunction",
            method: "POST",
            path: format!("/v1/apis/{api_id}/functions/{function_id}"),
            body: json!({
                "name": "fn_runtime",
                "dataSourceName": "ds_runtime",
                "code": CODE,
            }),
        },
        Named {
            label: "CreateResolver",
            method: "POST",
            path: format!("/v1/apis/{api_id}/types/Query/resolvers"),
            body: json!({
                "fieldName": "refused",
                "dataSourceName": "ds_runtime",
                "code": CODE,
            }),
        },
        Named {
            label: "UpdateResolver",
            method: "POST",
            path: format!("/v1/apis/{api_id}/types/Query/resolvers/field"),
            body: json!({"dataSourceName": "ds_runtime", "code": CODE}),
        },
    ]
}

/// Send `request` with `name` as the runtime's name.
async fn with_runtime_name(
    router: &MultiServiceRouter,
    request: &Named,
    name: &str,
) -> Answer {
    let mut body = request.body.clone();
    body["runtime"] = json!({"name": name, "runtimeVersion": RUNTIME_VERSION});
    call(router, request.method, &request.path, Some(body)).await
}

/// A `runtime.name` outside the value set is the service's own `BadRequestException`,
/// carrying the constraint report that withholds the rejected value.
#[tokio::test]
async fn a_runtime_name_outside_the_value_set_is_a_bad_request() {
    let router = router();
    let requests = requests_naming_a_runtime(&router).await;

    for request in &requests {
        for rejected in REJECTED {
            let case = format!("{} at runtime.name {rejected:?}", request.label);
            let answer = with_runtime_name(&router, request, rejected).await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (400, "BadRequestException"),
                "case {case:?}: expected 400 BadRequestException, got {answer:?}"
            );
            assert_eq!(
                answer.message,
                format!(
                    "1 validation error detected: Value at 'runtime.name' failed to satisfy \
                     constraint: Member must satisfy enum value set: [{APPSYNC_JS}]"
                ),
                "case {case:?}: expected the member path and the permitted set, got {answer:?}"
            );
            assert!(
                !answer.message.contains(rejected) || rejected.is_empty(),
                "case {case:?}: expected the rejected value to be withheld, got {answer:?}"
            );
        }

        // The same request with the one value the set holds is not refused for its
        // runtime: whatever each operation then answers, it is not this refusal.
        let answer = with_runtime_name(&router, request, APPSYNC_JS).await;
        assert!(
            !answer.message.contains("runtime.name"),
            "case {}: expected the accepted runtime name not to be refused, got {answer:?}",
            request.label
        );
    }
}
