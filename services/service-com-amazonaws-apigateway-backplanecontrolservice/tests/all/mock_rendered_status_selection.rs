// SPDX-License-Identifier: CC0-1.0

//! What a `MOCK` integration of a deployed stage answers when its request template COMPUTES
//! the result from the call it is serving.
//!
//! The RULE under test, one level above the recorded case:
//!
//! > A `MOCK` integration has no back end, so what it produced is its own request template
//! > RENDERED against the call being served — the references read off that call's path
//! > parameters, query string, headers and context, and the directives that bind a name or
//! > choose a branch performed — and the `statusCode` the rendering names is the result the
//! > integration responses are selected against: the one whose `selectionPattern` ("the
//! > regular expression (regex) pattern used to choose an integration response based on the
//! > response from the back end … the HTTP status code is matched") covers that result maps
//! > it onto that response's OWN `statusCode` ("the status code that is used to map the
//! > integration response to an existing MethodResponse"), its body template and its own
//! > `responseParameters` header mappings, while a response declaring no pattern, or the
//! > empty one, is the mapping a result no pattern covers takes.
//!
//! What this suite adds to the ones that own the neighbouring halves of that rule: every
//! row's result is DERIVED by the rendering, so the rows differ along HOW the template derives
//! it (a reference to a query parameter, a `#set` binding of one, a `#if`/`#else` choice over
//! one, a reference to a header) and along WHICH value the request carries for it (one each
//! branch covers, one of the patterned class that is not the branch's own status, one no
//! pattern covers, and none at all), with the branches differing in how the catch-all spells
//! its pattern (absent, and the empty string). The angles no other suite covers are the CLASS
//! pattern mapping a result of its class onto the branch's own status, the per-integration-
//! response `responseParameters` header of the branch that was selected, and the two
//! spellings of the catch-all.
//!
//! The halves this suite does NOT assert, and the suite that owns each:
//!
//! * selection over a LITERAL request template, and the mapped header of the selected branch
//!   —  [`crate::execute_api_data_plane::an_integration_responses_selection_pattern_is_evaluated_server_side`];
//! * a derived result naming a stored response or none, and the rendering of a parameter
//!   itself — [`crate::stage_mapping_templates::the_rendered_request_template_names_the_integration_response_the_call_maps_to`],
//!   which drives the same reference form, so the rows here that carry a result the catch-all
//!   answers are that suite's case under this suite's two-branch configuration;
//! * a `#if`/`#else` request template whose branches are selected by an exact pattern —
//!   [`crate::execute_api_data_plane::canary_release_and_vtl_transformation`];
//! * the same selection reported by `TestInvokeMethod`, including a request whose query
//!   string the template derives the result from —
//!   [`crate::test_invoke_method::a_patterned_integration_response_maps_the_result_a_mock_produced`]
//!   and the parity rows in [`crate::test_invoke_method_data_plane_parity`];
//! * what `$context.httpMethod` renders, and the verb a `#if` over it branches on —
//!   [`crate::stage_verb_precedence`].
//!
//! Every case drives the real wire path — the api's own invoke host through the multi-service
//! router, with the control plane driven through the same emulator ([`Gateway`]) — so what is
//! asserted is what a client of the published invoke URL sees.

use serde_json::json;

use crate::execute_api_data_plane::{
    branch_body, branch_records, deploy, integration_response, invoke, method_response,
    put_integration, put_method, CATCH_ALL_ROUTE, PATTERNED_ROUTE, ROUTE_HEADER,
};
use crate::rest_api_lifecycle::{create_api, resource_under, Gateway};

/// The stage the api is deployed to; one is enough, because no row turns on the stage name.
const STAGE: &str = "prod";

/// The content type the request templates are keyed by, and the one every call declares.
const JSON: &str = "application/json";

/// One way a request template derives the result, with the resource it is put on.
struct Form {
    /// What the form is reported as when a row of it fails.
    name: &'static str,
    /// The resource the method carrying this template is put on.
    path_part: &'static str,
    /// The `requestTemplates` entry of the `MOCK`, which is what the integration produced.
    request_template: &'static str,
    /// How the catch-all branch spells its `selectionPattern`: absent, or the empty string.
    /// Both are a response chosen by no result in particular, so both are the catch-all.
    empty_default_pattern: bool,
    /// The requests this form is driven with.
    cases: &'static [Case],
}

/// One request a form is driven with, and the branch the api's records answer it from.
struct Case {
    /// What the row is reported as when it fails.
    name: &'static str,
    /// The query string the request carries, with its leading `?`, or empty for none.
    query: &'static str,
    /// The headers the request carries beside the content type.
    headers: &'static [(&'static str, &'static str)],
    expected_status: u16,
    /// The branch the result selects, which names both the body and the mapped header.
    expected_route: &'static str,
}

/// The forms, each with the requests that exercise what it derives the result from.
const FORMS: &[Form] = &[
    Form {
        name: "a reference to a query parameter",
        path_part: "reference",
        request_template: r#"{"statusCode": $input.params('code')}"#,
        empty_default_pattern: false,
        cases: &[
            Case {
                name: "a result the unpatterned branch answers",
                query: "?code=200",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
            Case {
                name: "a result the patterned branch covers",
                query: "?code=500",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "another result of the patterned class maps onto that branch's status",
                query: "?code=503",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "a result no pattern covers maps to the catch-all",
                query: "?code=404",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
            Case {
                name: "no parameter to render, so the rendering names no result",
                query: "",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
        ],
    },
    Form {
        name: "a #set binding of a query parameter",
        path_part: "bound",
        request_template: "#set($code = $input.params('code'))\n{\"statusCode\": $code}",
        empty_default_pattern: true,
        cases: &[
            Case {
                name: "a result the unpatterned branch answers",
                query: "?code=200",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
            Case {
                name: "a result the patterned branch covers",
                query: "?code=500",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "another result of the patterned class maps onto that branch's status",
                query: "?code=502",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "no parameter to bind, so the rendering names no result",
                query: "",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
        ],
    },
    Form {
        name: "a #if/#else choice over a query parameter",
        path_part: "select",
        request_template: "#if($input.params('scope') == \"internal\") {\"statusCode\": 200} \
                           #else {\"statusCode\": 500} #end",
        empty_default_pattern: true,
        cases: &[
            Case {
                name: "the value the condition admits",
                query: "?scope=internal",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
            Case {
                name: "a value the condition does not admit",
                query: "?scope=external",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "another value the condition does not admit",
                query: "?scope=partner",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "no value at all",
                query: "",
                headers: &[],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "the admitted value again, after the other branch was served",
                query: "?scope=internal",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
        ],
    },
    Form {
        // The parameter is read as the request spells the header, which is the spelling
        // `$input.params` is given here: that a header name is matched case-insensitively is
        // its own rule and not one of this suite's rows.
        name: "a reference to a header",
        path_part: "header-derived",
        request_template: r#"{"statusCode": $input.params('X-Code')}"#,
        empty_default_pattern: false,
        cases: &[
            Case {
                name: "a header the unpatterned branch answers",
                query: "",
                headers: &[("X-Code", "200")],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
            Case {
                name: "a header the patterned branch covers",
                query: "",
                headers: &[("X-Code", "504")],
                expected_status: 500,
                expected_route: PATTERNED_ROUTE,
            },
            Case {
                name: "no header to render, so the rendering names no result",
                query: "",
                headers: &[],
                expected_status: 200,
                expected_route: CATCH_ALL_ROUTE,
            },
        ],
    },
];

/// Stand one `MOCK` `GET` method up on a fresh resource under `root`: the method, the
/// integration carrying the form's request template, and the two branches the rendered result
/// is selected between ([`branch_records`]).
async fn mock_branching_method(gateway: &Gateway, api: &str, root: &str, form: &Form) {
    let resource = resource_under(gateway, api, root, form.path_part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { JSON: form.request_template },
        }),
    )
    .await;
    for (status, declared, mapped) in branch_records(form.empty_default_pattern) {
        method_response(gateway, api, &resource, "GET", status, declared).await;
        integration_response(gateway, api, &resource, "GET", status, mapped).await;
    }
}

#[tokio::test]
async fn the_rendered_mock_result_selects_the_branch_the_stage_serves() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "mock-rendered-status").await;
    for form in FORMS {
        mock_branching_method(&gateway, &api, &root, form).await;
    }
    deploy(&gateway, &api, STAGE).await;

    let header_name = ROUTE_HEADER.to_ascii_lowercase();
    for form in FORMS {
        for case in form.cases {
            let case_name = format!("{} / {}", form.name, case.name);
            let mut headers = vec![("content-type", JSON)];
            headers.extend_from_slice(case.headers);
            let answered = invoke(
                &gateway,
                &api,
                "GET",
                &format!("/{STAGE}/{}{}", form.path_part, case.query),
                &headers,
            )
            .await;
            assert_eq!(
                answered.status, case.expected_status,
                "case {case_name:?}: expected {} on the wire, got {} with {:?}",
                case.expected_status, answered.status, answered.body
            );
            let expected_body = branch_body(case.expected_route);
            assert_eq!(
                answered.body, expected_body,
                "case {case_name:?}: expected the selected branch's body {expected_body:?}, \
                 got {:?}",
                answered.body
            );
            assert_eq!(
                answered.header(&header_name),
                Some(case.expected_route),
                "case {case_name:?}: expected the selected branch's {ROUTE_HEADER} {:?}, got \
                 {:?}",
                case.expected_route,
                answered.header(&header_name)
            );
        }
    }
}
