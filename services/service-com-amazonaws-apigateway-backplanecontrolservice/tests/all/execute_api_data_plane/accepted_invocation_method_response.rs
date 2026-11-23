// SPDX-License-Identifier: CC0-1.0

//! WHAT THE CALLER IS SERVED for an integration whose invocation of its Lambda function is
//! answered BEFORE the function has produced anything.
//!
//! RULE: `Invoke` answers the asynchronous and the validating invocation with a status and no
//! payload — "Event – Invoke the function asynchronously […] The API response only includes a
//! status code" and "DryRun – Validate parameter values and verify that the user or role has
//! permission to invoke the function"
//! (`smithy_query(query="operation", operation="Invoke")`, member `InvocationType`) — so an
//! `AWS` integration that maps one of those types onto the integration request's
//! `X-Amz-Invocation-Type` header has ONLY that payload-less answer to map, and the method
//! response is what the integration's own records make of it: the status of the integration
//! response that answer selects ("the status code that is used to map the integration response
//! to an existing MethodResponse", `IntegrationResponse$statusCode`) and the headers its
//! `responseParameters` map, with NO body — whatever the background run goes on to return or
//! raise, which the caller was answered before.
//!
//! The rows differ along what the rule generalises over: the invocation type mapped (`Event`,
//! `DryRun`, and the synchronous default as the contrast), the MAPPING FORM (a static value, a
//! method request header the caller supplies, none at all), the method response the selected
//! integration response maps onto (`200`, `201`, `204`), whether that integration response
//! also maps a HEADER onto the method response, and what the background run comes to (a
//! handler that returns, and one that RAISES after it has marked its own run).
//!
//! What makes a run OBSERVABLE is the sibling `lambda_event_invocation_type` suite's device,
//! reused rather than invented a second time: the handler writes one marker item into a
//! DynamoDB table keyed by the resource path it was reached through, so a row asserts what the
//! CALLER held and, separately, whether the function ran at all — by polling for the item,
//! since an accepted invocation's run happens after its caller was answered. The three-service
//! engine, the marker table, the function stand-up, the invoke URL request and the polling all
//! come from that sibling; the wire harness under them is `proxy_binary_media_types`'s. What is
//! this suite's own is the methods it declares and what it asserts about their responses.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{apigw, member, wait_until_active, STAGE};

use super::lambda_event_invocation_type::{
    emulator, given_marker_table, given_marking_function, invocation_uri, invoke, marked, marker,
    wait_until_table_active, Served,
};

/// The function every method below is integrated with.
const FUNCTION: &str = "apigw-accepted-invocation";

/// The method request header a method that maps the invocation type FROM THE REQUEST takes it
/// from, declared on the method and mapped onward by its integration.
const CASE_HEADER: &str = "X-Invocation-Type";

/// The method response header one row's integration response maps a static value onto, which is
/// what shows the method response is BUILT by the mapping even for an answer carrying no run.
const OUTCOME_HEADER: &str = "X-Invocation-Outcome";

/// The value that row maps onto [`OUTCOME_HEADER`], in the single quotes a static
/// `responseParameters` value is written in.
const OUTCOME_VALUE: &str = "accepted";

/// The ONE handler every method is integrated with: it marks its own run under the resource
/// path the integration request names, then either returns a document or RAISES — which is the
/// background outcome a caller of an accepted invocation is answered before.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def lambda_handler(event, context):
    pk = event["pk"]
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={"pk": {"S": pk}},
    )
    if event["raises"]:
        raise RuntimeError("the run failed after it marked itself")
    return {"stored": True, "pk": pk}
"#;

/// One method of the api: what its integration maps onto the invocation type header, what the
/// event tells the handler to do, and what its own integration response maps the answer onto.
struct Method {
    /// The path part, which is also the resource path the handler marks its run by.
    part: &'static str,
    /// The `requestParameters` value mapped onto
    /// `integration.request.header.X-Amz-Invocation-Type`, or `None` for an integration that
    /// maps nothing onto it.
    mapped: Option<&'static str>,
    /// Whether the rendered integration request tells the handler to raise after marking.
    raises: bool,
    /// `IntegrationResponse$statusCode`: the method response this integration's DEFAULT
    /// integration response — the one declaring no `selectionPattern` — maps the answer onto.
    maps_onto: &'static str,
    /// Whether that integration response also maps [`OUTCOME_VALUE`] onto
    /// [`OUTCOME_HEADER`] of the method response.
    maps_header: bool,
}

/// The methods the api carries, each on its own path so one deployment serves every row and
/// each run has a marker key of its own.
const METHODS: &[Method] = &[
    Method {
        part: "accepted",
        mapped: Some("'Event'"),
        raises: false,
        maps_onto: "200",
        maps_header: false,
    },
    Method {
        part: "accepted-mapped-201",
        mapped: Some("'Event'"),
        raises: false,
        maps_onto: "201",
        maps_header: true,
    },
    Method {
        part: "accepted-from-header",
        mapped: Some("method.request.header.X-Invocation-Type"),
        raises: false,
        maps_onto: "204",
        maps_header: false,
    },
    Method {
        part: "accepted-failing-run",
        mapped: Some("'Event'"),
        raises: true,
        maps_onto: "200",
        maps_header: false,
    },
    Method {
        part: "validated",
        mapped: Some("'DryRun'"),
        raises: false,
        maps_onto: "201",
        maps_header: false,
    },
    Method {
        part: "awaited",
        mapped: None,
        raises: false,
        maps_onto: "200",
        maps_header: false,
    },
];

/// Stand the table, the function and the api up, and deploy the api to [`STAGE`]. Answers the
/// api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    given_marker_table(router).await;
    let function_arn = given_marking_function(router, FUNCTION, HANDLER_SOURCE).await;

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "lambda-accepted-invocation" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");

    for method in METHODS {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": method.part })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        let mut put_method = json!({ "authorizationType": "NONE" });
        if method
            .mapped
            .is_some_and(|value| value.starts_with("method.request."))
        {
            // The method declares the request parameter its integration maps onward, which is
            // how a caller-supplied value reaches the integration request.
            let mut declared = serde_json::Map::new();
            declared.insert(
                format!("method.request.header.{CASE_HEADER}"),
                Value::Bool(false),
            );
            put_method["requestParameters"] = Value::Object(declared);
        }
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/POST"),
            Some(put_method),
            201,
        )
        .await;
        let mut integration = json!({
            "type": "AWS",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": invocation_uri(&function_arn),
            // An `AWS` integration's event is the integration request its own template
            // renders: the resource path the run marks itself by, and what that run must do.
            "requestTemplates": {
                "application/json": format!(
                    "{{\"pk\":\"$context.resourcePath\",\"raises\":{}}}",
                    method.raises
                ),
            },
        });
        if let Some(mapped) = method.mapped {
            integration["requestParameters"] =
                json!({ "integration.request.header.X-Amz-Invocation-Type": mapped });
        }
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/POST/integration"),
            Some(integration),
            201,
        )
        .await;
        // The method response this integration response maps onto, and the DEFAULT integration
        // response — the one declaring no `selectionPattern` — which is what maps whatever
        // Lambda answered the invocation with.
        let mut method_response = json!({});
        let mut integration_response = json!({});
        if method.maps_header {
            let parameter = format!("method.response.header.{OUTCOME_HEADER}");
            let mut declared = serde_json::Map::new();
            declared.insert(parameter.clone(), Value::Bool(false));
            method_response["responseParameters"] = Value::Object(declared);
            let mut mapped = serde_json::Map::new();
            mapped.insert(parameter, Value::String(format!("'{OUTCOME_VALUE}'")));
            integration_response["responseParameters"] = Value::Object(mapped);
        }
        apigw(
            router,
            "PutMethodResponse",
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/POST/responses/{}",
                method.maps_onto
            ),
            Some(method_response),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegrationResponse",
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/POST/integration/responses/{}",
                method.maps_onto
            ),
            Some(integration_response),
            201,
        )
        .await;
    }

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    wait_until_active(router, FUNCTION).await;
    wait_until_table_active(router).await;
    api
}

/// One row: the method it calls, the invocation-type header the caller sends, and what the
/// method response must be.
struct Row {
    case: &'static str,
    part: &'static str,
    /// The value the caller sends under [`CASE_HEADER`], for a method that maps the header.
    sent: Option<&'static str>,
    /// The status the selected integration response maps the answer onto.
    status: u16,
    body: Served,
    /// Whether the method response must carry [`OUTCOME_HEADER`] with [`OUTCOME_VALUE`].
    header: bool,
    /// Whether the function's own run must have happened.
    ran: bool,
}

/// RULE: an invocation Lambda answers before the function has produced a payload leaves the
/// integration only that payload-less answer to map, so the caller is served the method
/// response the selected integration response names — its status and its mapped headers —
/// with no body, whatever the background run then returns or raises; an invocation made FOR
/// the function's answer is the one that serves what the run produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invocation_answered_before_its_run_maps_to_a_method_response_with_no_body() {
    let router = emulator();
    let api = deployed(&router).await;

    let rows: &[Row] = &[
        Row {
            case: "the asynchronous invocation, mapped onto the method response keyed 200",
            part: "accepted",
            sent: None,
            status: 200,
            body: Served::Nothing,
            header: false,
            ran: true,
        },
        Row {
            case: "the same invocation mapped onto a method response keyed 201 that also maps \
                   a header",
            part: "accepted-mapped-201",
            sent: None,
            status: 201,
            body: Served::Nothing,
            header: true,
            ran: true,
        },
        Row {
            case: "the asynchronous invocation named by a header the caller sent, mapped onto \
                   the method response keyed 204",
            part: "accepted-from-header",
            sent: Some("Event"),
            status: 204,
            body: Served::Nothing,
            header: false,
            ran: true,
        },
        Row {
            case: "an asynchronous invocation whose background run RAISES after marking itself",
            part: "accepted-failing-run",
            sent: None,
            status: 200,
            body: Served::Nothing,
            header: false,
            ran: true,
        },
        Row {
            case: "the validating invocation, which runs nothing at all",
            part: "validated",
            sent: None,
            status: 201,
            body: Served::Nothing,
            header: false,
            ran: false,
        },
        Row {
            case: "an integration mapping no invocation type, which awaits the run it made",
            part: "awaited",
            sent: None,
            status: 200,
            body: Served::TheRun,
            header: false,
            ran: true,
        },
    ];

    // Every row's CALLER is answered first, so the accepted rows are answered before any marker
    // is looked for: what the caller holds cannot depend on a run that has not happened.
    for row in rows {
        let headers: Vec<(&str, &str)> = row
            .sent
            .map(|value| vec![(CASE_HEADER, value)])
            .unwrap_or_default();
        let served = invoke(&router, &api, row.part, &headers).await;
        assert_eq!(
            served.status, row.status,
            "case {:?}: expected the status the integration response maps onto ({}), got {} \
             with {:?}",
            row.case,
            row.status,
            served.status,
            served.text()
        );
        match row.body {
            Served::Nothing => assert_eq!(
                served.text(),
                "",
                "case {:?}: an invocation answered before its run leaves the method response \
                 with no body, got {:?}",
                row.case,
                served.text()
            ),
            Served::TheRun => assert_eq!(
                served.json(),
                json!({ "stored": true, "pk": format!("/{}", row.part) }),
                "case {:?}: expected the document the run produced, got {:?}",
                row.case,
                served.text()
            ),
        }
        let carried = served.header(OUTCOME_HEADER);
        let expected = row.header.then(|| OUTCOME_VALUE.to_string());
        assert_eq!(
            carried, expected,
            "case {:?}: expected the mapped method response header to be {expected:?}, got \
             {carried:?}",
            row.case
        );
    }

    // ... and only then whether each function RAN, which is the other half of the rule: the
    // accepted rows' runs happen after their callers were answered, so the condition is polled
    // rather than a span waited out.
    for row in rows.iter().filter(|row| row.ran) {
        let pk = format!("/{}", row.part);
        assert!(
            marked(&router, row.case, &pk).await,
            "case {:?}: the function's own run must have written its marker item under {pk:?}",
            row.case
        );
    }
    // The validating invocation ran nothing: its marker is still absent now that every other
    // row's run has been seen, which is what shows there was time for one.
    for row in rows.iter().filter(|row| !row.ran) {
        let pk = format!("/{}", row.part);
        let held = marker(&router, row.case, &pk).await;
        assert!(
            held.is_none(),
            "case {:?}: an invocation that runs nothing must leave no marker item under \
             {pk:?}, got {held:?}",
            row.case
        );
    }
}
