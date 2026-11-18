// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Athena::NamedQuery` really saves that query in
//! Amazon Athena, and the handle the stack publishes for it is the identifier that service
//! minted.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API, and the identifier the stack
//! publishes for it is the one that service ASSIGNED where the service assigns one — so the
//! value `Ref` and the type's identifier attribute resolve to is a handle that service's own
//! read answers for, and deleting the stack releases the resource there.** For a saved query
//! that identifier is the `NamedQueryId` `CreateNamedQuery` mints: `GetNamedQuery`,
//! `UpdateNamedQuery` and `DeleteNamedQuery` each address a query by it and by nothing else, and
//! a workgroup may hold two queries under one name — so the declared `Name` is not a handle at
//! all. Two crates have to agree for that to hold (CloudFormation makes the calls, Athena serves
//! them), so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the template declares the
//! workgroup the query is saved in or leaves it to the account's default one, whether it is
//! written in JSON or YAML, whether it states a description, where the query's name comes from
//! (written in the template, or a parameter it reads), how many queries one stack saves, and the
//! Region the stack is deployed in.
//!
//! The deletion half, the replacement half and the refusal half are rows of the same rule:
//! deleting the stack removes the query Athena holds, a changed declaration of a value a saved
//! query cannot take anew replaces it under a new minted id, and a declaration Athena REFUSES
//! leaves the stack `ROLLBACK_COMPLETE` with nothing saved behind it.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource. The minted ids are
//! never written down — what is asserted about one is that Athena resolves it, that it is not
//! the declared name, and that two queries of one stack carry different ones.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// How long one poll waits before the next, and how many are made.
///
/// A stack's own status transitions and the cleanup a replacement owes are due on their clocks,
/// so the suite waits for the CONDITION rather than for a span, with a deadline generous enough
/// for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 600;

/// The `x-amz-target` prefix of Amazon Athena: the local name of its Smithy service shape
/// (`com.amazonaws.athena#AmazonAthena`), which is how the awsJson protocols build that header.
const ATHENA_TARGET_PREFIX: &str = "AmazonAthena";

/// The workgroup a saved query belongs to when its declaration names none: "if a workgroup is
/// not specified, lists the saved queries for the primary workgroup" (`ListNamedQueries`), which
/// is the workgroup `CreateNamedQuery` saves into as well.
const PRIMARY_WORK_GROUP: &str = "primary";

/// The longest a `NamedQueryId` may be (`NamedQueryId`, `@length` 1 to 128 and pattern `^\S+$`
/// in Athena's own model), which the id a stack publishes for a saved query is held to.
const MAX_NAMED_QUERY_ID: usize = 128;

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_athena_amazonathena::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon Athena operation over the awsJson1_1 wire its model declares, signed for
/// `region`.
async fn athena(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ATHENA_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("athena", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes. A
/// stack that reaches another TERMINAL status fails the suite at once, quoting the answer: the
/// reason a resource failed is in it.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported_status == wanted {
            return body;
        }
        assert!(
            !reported_status.ends_with("_FAILED") && !reported_status.starts_with("ROLLBACK"),
            "stack {stack:?}: expected {wanted:?}, got {reported_status:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Wait for `stack` to reach any TERMINAL status, which is what a deployment that fails ends in.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported_status.ends_with("_IN_PROGRESS") {
            return (reported_status, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached a terminal status; last answer was {body}");
}

/// One output of a `DescribeStacks` answer, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The query Athena holds under `id`, as its own `GetNamedQuery` reports it.
async fn named_query(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    case: &str,
) -> Value {
    let (status, answered) = athena(
        router,
        region,
        "GetNamedQuery",
        &json!({ "NamedQueryId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetNamedQuery on the id the stack published, got \
         {status} with body {answered}"
    );
    answered.get("NamedQuery").cloned().unwrap_or(Value::Null)
}

/// The ids Athena reports for `work_group`, through its own `ListNamedQueries`.
async fn listed_ids(
    router: &MultiServiceRouter,
    region: &str,
    work_group: &str,
    case: &str,
) -> Vec<String> {
    let (status, listed) = athena(
        router,
        region,
        "ListNamedQueries",
        &json!({ "WorkGroup": work_group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListNamedQueries on {work_group:?}, got {status} \
         with body {listed}"
    );
    listed
        .get("NamedQueryIds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id.as_str().map(ToString::to_string))
        .collect()
}

/// Wait until Athena no longer holds a query under `id`, which is what a delete of the stack
/// that saved it owes. The condition is polled rather than timed, with a deadline generous
/// enough for a loaded host.
async fn wait_until_query_is_gone(router: &MultiServiceRouter, region: &str, id: &str, case: &str) {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (status, answered) = athena(
            router,
            region,
            "GetNamedQuery",
            &json!({ "NamedQueryId": id }),
        )
        .await;
        if status != 200 {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from GetNamedQuery for a query Athena no longer \
                 holds, got {status} with body {answered}"
            );
            assert_eq!(
                answered.get("__type").and_then(Value::as_str),
                Some("InvalidRequestException"),
                "case {case:?}: expected InvalidRequestException for a query Athena no longer \
                 holds, got {answered}"
            );
            return;
        }
        last = answered;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: Athena still holds the query; last answer was {last}");
}

/// One saved query a row's template declares, and what the rule says Athena must then hold for
/// it.
struct Query {
    /// The logical id the template declares it under, which is also the output key the row reads
    /// its minted id from (`<logical>Id`) and the one it reads the `Ref` from (`<logical>Ref`).
    logical_id: &'static str,
    /// The `Name` the declaration states, which the saved query is CALLED but not addressed by.
    expected_name: &'static str,
    /// The `Database` it states.
    expected_database: &'static str,
    /// The `QueryString` it states.
    expected_query_string: &'static str,
    /// The `Description` it states, if any.
    expected_description: Option<&'static str>,
    /// The workgroup the query is saved in: the one the declaration names, or the account's
    /// default workgroup where it names none.
    expected_work_group: &'static str,
    /// Whether that workgroup OUTLIVES the stack: the account's default workgroup does, and one
    /// the same template declares is released with the stack — which is what decides whether the
    /// workgroup's own listing can still be read once the stack is deleted.
    work_group_outlives_the_stack: bool,
}

/// One row of the table: a template, and the queries it declares.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the queries are saved in.
    region: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The parameters the row deploys the template with, as `(the name, the value)`.
    parameters: &'static [(&'static str, &'static str)],
    /// The queries it declares.
    queries: &'static [Query],
}

const ROWS: &[Row] = &[
    Row {
        case: "one query with a description, in a workgroup the same stack declares, in JSON",
        region: "us-east-1",
        stack: "athena-named-query-json-stack",
        template: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Analytics": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {
                "Name": "orders-queries",
                "RecursiveDeleteOption": true
              }
            },
            "TopOrders": {
              "Type": "AWS::Athena::NamedQuery",
              "Properties": {
                "Name": "top-orders",
                "Database": "orders",
                "Description": "the orders of one day",
                "QueryString": "SELECT id FROM orders LIMIT 10",
                "WorkGroup": {"Ref": "Analytics"}
              }
            }
          },
          "Outputs": {
            "TopOrdersId": {"Value": {"Fn::GetAtt": ["TopOrders", "NamedQueryId"]}},
            "TopOrdersRef": {"Value": {"Ref": "TopOrders"}}
          }
        }"#,
        parameters: &[],
        queries: &[Query {
            logical_id: "TopOrders",
            expected_name: "top-orders",
            expected_database: "orders",
            expected_query_string: "SELECT id FROM orders LIMIT 10",
            expected_description: Some("the orders of one day"),
            expected_work_group: "orders-queries",
            work_group_outlives_the_stack: false,
        }],
    },
    Row {
        case: "one query whose name a parameter supplies, no description, the default workgroup, \
               in YAML",
        region: "eu-west-1",
        stack: "athena-named-query-yaml-stack",
        template: concat!(
            "Parameters:\n",
            "  QueryName:\n",
            "    Type: String\n",
            "Resources:\n",
            "  Answer:\n",
            "    Type: AWS::Athena::NamedQuery\n",
            "    Properties:\n",
            "      Name: !Ref QueryName\n",
            "      Database: default\n",
            "      QueryString: SELECT 1 AS answer\n",
            "Outputs:\n",
            "  AnswerId:\n",
            "    Value: !GetAtt Answer.NamedQueryId\n",
            "  AnswerRef:\n",
            "    Value: !Ref Answer\n",
        ),
        parameters: &[("QueryName", "parameterised-answer")],
        queries: &[Query {
            logical_id: "Answer",
            expected_name: "parameterised-answer",
            expected_database: "default",
            expected_query_string: "SELECT 1 AS answer",
            expected_description: None,
            expected_work_group: PRIMARY_WORK_GROUP,
            work_group_outlives_the_stack: true,
        }],
    },
    Row {
        case: "two queries of one stack, which only their own minted ids tell apart",
        region: "ap-southeast-1",
        stack: "athena-named-query-pair-stack",
        template: r#"{
          "Resources": {
            "First": {
              "Type": "AWS::Athena::NamedQuery",
              "Properties": {
                "Name": "first-of-the-pair",
                "Database": "sales",
                "QueryString": "SELECT 1"
              }
            },
            "Second": {
              "Type": "AWS::Athena::NamedQuery",
              "Properties": {
                "Name": "second-of-the-pair",
                "Database": "sales",
                "Description": "the second of the pair",
                "QueryString": "SELECT 2"
              }
            }
          },
          "Outputs": {
            "FirstId": {"Value": {"Fn::GetAtt": ["First", "NamedQueryId"]}},
            "FirstRef": {"Value": {"Ref": "First"}},
            "SecondId": {"Value": {"Fn::GetAtt": ["Second", "NamedQueryId"]}},
            "SecondRef": {"Value": {"Ref": "Second"}}
          }
        }"#,
        parameters: &[],
        queries: &[
            Query {
                logical_id: "First",
                expected_name: "first-of-the-pair",
                expected_database: "sales",
                expected_query_string: "SELECT 1",
                expected_description: None,
                expected_work_group: PRIMARY_WORK_GROUP,
                work_group_outlives_the_stack: true,
            },
            Query {
                logical_id: "Second",
                expected_name: "second-of-the-pair",
                expected_database: "sales",
                expected_query_string: "SELECT 2",
                expected_description: Some("the second of the pair"),
                expected_work_group: PRIMARY_WORK_GROUP,
                work_group_outlives_the_stack: true,
            },
        ],
    },
];

/// Every row: the id the stack publishes for each query it declares is one Athena minted, the
/// query it addresses carries the declared values, the id is in the workgroup's own listing, and
/// deleting the stack takes the query away there.
#[tokio::test]
async fn a_stack_saves_the_named_queries_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack = format!("{}-{index}", row.stack);

        let mut members: Vec<(&str, &str)> =
            vec![("StackName", &stack), ("TemplateBody", row.template)];
        let parameter_keys: Vec<(String, String)> = row
            .parameters
            .iter()
            .enumerate()
            .flat_map(|(position, (name, value))| {
                let slot = position + 1;
                [
                    (
                        format!("Parameters.member.{slot}.ParameterKey"),
                        (*name).to_string(),
                    ),
                    (
                        format!("Parameters.member.{slot}.ParameterValue"),
                        (*value).to_string(),
                    ),
                ]
            })
            .collect();
        for (key, value) in &parameter_keys {
            members.push((key.as_str(), value.as_str()));
        }
        let (status, body) = cloudformation(&router, region, "CreateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = wait_for_stack_status(&router, region, &stack, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&described);

        let mut minted: Vec<String> = Vec::new();
        for query in row.queries {
            let case = &format!("{case} / {}", query.logical_id);
            let id = output(&outputs, &format!("{}Id", query.logical_id), case);
            let referenced = output(&outputs, &format!("{}Ref", query.logical_id), case);
            // "Ref returns the named query ID", which is the identifier attribute as well: one
            // resource has one handle, so the two cannot disagree.
            assert_eq!(
                id, referenced,
                "case {case:?}: expected Ref and NamedQueryId to be the same handle, got \
                 {referenced:?} and {id:?}"
            );
            // The handle is Athena's own, inside the bounds its model fixes for one — and it is
            // NOT the name the template declared, which addresses no query at all.
            assert!(
                !id.is_empty()
                    && id.len() <= MAX_NAMED_QUERY_ID
                    && id.chars().all(|character| !character.is_whitespace()),
                "case {case:?}: expected an id inside Athena's own NamedQueryId rule, got {id:?}"
            );
            assert_ne!(
                id, query.expected_name,
                "case {case:?}: expected the published handle to be the minted id rather than \
                 the declared name, got {id:?}"
            );
            assert!(
                !minted.contains(&id),
                "case {case:?}: expected a handle of this query's own, got {id:?} which another \
                 query of the stack already carries ({minted:?})"
            );
            minted.push(id.clone());

            // Athena itself resolves the published id to the query the template declared.
            let reported = named_query(&router, region, &id, case).await;
            let expected: &[(&str, &str, Option<&str>)] = &[
                ("the minted id", "NamedQueryId", Some(id.as_str())),
                ("the declared name", "Name", Some(query.expected_name)),
                (
                    "the declared database",
                    "Database",
                    Some(query.expected_database),
                ),
                (
                    "the declared query string",
                    "QueryString",
                    Some(query.expected_query_string),
                ),
                (
                    "the declared description",
                    "Description",
                    query.expected_description,
                ),
                (
                    "the workgroup it is saved in",
                    "WorkGroup",
                    Some(query.expected_work_group),
                ),
            ];
            for (member_case, member, wanted) in expected {
                let actual = reported.get(*member).and_then(Value::as_str);
                assert_eq!(
                    actual, *wanted,
                    "case {case:?}: expected {member_case} as {member} to be {wanted:?}, got \
                     {actual:?} in {reported}"
                );
            }

            // And the workgroup's own listing reports it.
            let listed = listed_ids(&router, region, query.expected_work_group, case).await;
            assert!(
                listed.contains(&id),
                "case {case:?}: expected the id the stack published in the listing of \
                 {:?}, got {listed:?}",
                query.expected_work_group
            );
        }

        // Deleting the stack releases what Athena holds for it: the query is gone from the
        // service's read and from the workgroup's listing.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        for (query, id) in row.queries.iter().zip(&minted) {
            let case = &format!("{case} / {}", query.logical_id);
            wait_until_query_is_gone(&router, region, id, case).await;
            if query.work_group_outlives_the_stack {
                // The workgroup is still this account's, and its listing no longer carries the
                // query the stack saved in it.
                let listed = listed_ids(&router, region, query.expected_work_group, case).await;
                assert!(
                    !listed.contains(id),
                    "case {case:?}: expected the deleted query to be out of the listing of {:?}, \
                     got {listed:?}",
                    query.expected_work_group
                );
            } else {
                // The workgroup went with the stack that declared it, so there is no listing of
                // it left to read at all: a listing is scoped by a workgroup this account holds,
                // and Athena no longer holds this one.
                let (status, gone) = athena(
                    &router,
                    region,
                    "ListNamedQueries",
                    &json!({ "WorkGroup": query.expected_work_group }),
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the workgroup the stack declared to be released with \
                     it, got HTTP {status} from ListNamedQueries with body {gone}"
                );
            }
        }
    }
}

/// A declaration Athena REFUSES does not report a query nothing saved: the stack rolls back with
/// that service's own refusal, and the account's listing holds nothing for it afterwards.
///
/// The rows differ along WHAT the owning service refuses: a workgroup this account does not hold
/// (there is nowhere to save the query), and a declaration stating none of the `QueryString`
/// `CreateNamedQuery` requires.
#[tokio::test]
async fn a_named_query_declaration_athena_refuses_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";

    let rows: &[(&str, &str)] = &[
        (
            "a workgroup this account does not hold",
            r#"{"Resources": {"Orphan": {"Type": "AWS::Athena::NamedQuery",
               "Properties": {"Name": "orphan", "Database": "default",
               "QueryString": "SELECT 1", "WorkGroup": "no-such-workgroup"}}}}"#,
        ),
        (
            "a declaration stating no QueryString",
            r#"{"Resources": {"Silent": {"Type": "AWS::Athena::NamedQuery",
               "Properties": {"Name": "silent", "Database": "default"}}}}"#,
        ),
    ];

    let before = listed_ids(&router, region, PRIMARY_WORK_GROUP, "before the refusals").await;
    for (index, (case, template)) in rows.iter().enumerate() {
        let stack = format!("athena-named-query-refusal-{index}");
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reported, described) = wait_for_terminal_status(&router, region, &stack).await;
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back on a declaration Athena refuses, got \
             {reported:?} with answer {described}"
        );
        let listed = listed_ids(&router, region, PRIMARY_WORK_GROUP, case).await;
        assert_eq!(
            listed, before,
            "case {case:?}: expected the refused declaration to save nothing, got {listed:?} \
             against {before:?}"
        );
    }
}

/// A stack that CHANGES a value a saved query cannot take anew REPLACES it: Athena publishes no
/// call that moves a query to another database or workgroup, so the stack saves a new query —
/// under a new minted id — and the one it replaced is gone.
#[tokio::test]
async fn a_changed_named_query_declaration_replaces_the_query_athena_holds() {
    let router = build_router();
    let region = "us-west-2";
    let stack = "athena-named-query-replacement-stack";

    let template = |query_string: &str| {
        format!(
            r#"{{
              "Resources": {{
                "Rollup": {{
                  "Type": "AWS::Athena::NamedQuery",
                  "Properties": {{
                    "Name": "rollup",
                    "Database": "reporting",
                    "QueryString": "{query_string}"
                  }}
                }}
              }},
              "Outputs": {{
                "RollupId": {{"Value": {{"Fn::GetAtt": ["Rollup", "NamedQueryId"]}}}}
              }}
            }}"#
        )
    };

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template("SELECT 1"))],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, region, stack, "CREATE_COMPLETE").await;
    let before = output(&outputs_of(&described), "RollupId", "the created query");

    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &template("SELECT 2"))],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(&router, region, stack, "UPDATE_COMPLETE").await;
    let after = output(&outputs_of(&described), "RollupId", "the replacing query");

    assert_ne!(
        after, before,
        "case \"a changed query string\": expected the replacement to publish a new minted id, \
         got {after:?} for both"
    );
    let reported = named_query(&router, region, &after, "the replacing query").await;
    assert_eq!(
        reported.get("QueryString").and_then(Value::as_str),
        Some("SELECT 2"),
        "case \"a changed query string\": expected the replacement to carry the declared SQL, got \
         {reported}"
    );
    wait_until_query_is_gone(&router, region, &before, "the replaced query").await;
}
