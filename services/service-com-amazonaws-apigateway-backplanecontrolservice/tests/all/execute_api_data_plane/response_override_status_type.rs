// SPDX-License-Identifier: CC0-1.0

//! What a RESPONSE TEMPLATE may override about the status the method response is sent with:
//! `#set($context.responseOverride.status = …)`.
//!
//! The variable names the status a response is SENT with, and a status is a NUMBER — where
//! every other member an override names (`$context.responseOverride.header.<name>`,
//! `$context.requestOverride.querystring.<name>`) names a parameter, which carries text. So
//! the RULE read here: the served status is the one the override's expression states only when
//! that expression comes to a number; an expression coming to a value of any other type — a
//! quoted literal that happens to spell a status number included — states no status at all,
//! and the response is sent with the status its integration response declares.
//!
//! The rows differ along what the assigned expression IS: a numeric literal, the same number
//! quoted either way, a number bound by the template's own `#set`, a value read off the call,
//! a boolean, and no override at all. Every case drives the real wire path — a request to the
//! api's own invoke host through the multi-service router, with the control plane driven
//! through the same emulator — so what is asserted is the status line a client of the
//! published invoke URL reads.

use super::{deploy, invoke, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, Gateway};

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The status the integration response and the method response DECLARE, which is what a
/// response template that states no status of its own is sent with.
const DECLARED_STATUS: u16 = 200;

/// The body every response template below renders, so a row can tell a served response from
/// a gateway refusal whatever status it carries.
const SERVED_BODY: &str = r#"{"served":true}"#;

/// One row: the response template a method's integration response carries, and the status the
/// stage is expected to answer with.
struct Case {
    case: &'static str,
    /// The path part the row's own method sits on, which is also its resource name.
    route: &'static str,
    template: String,
    status: u16,
}

/// The response template that overrides the status with `expression`, followed by the body.
fn overriding(expression: &str) -> String {
    format!("#set($context.responseOverride.status = {expression})\n{SERVED_BODY}")
}

/// The rows of the table, one per form the assigned expression takes.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "no override at all, which leaves the declared status",
            route: "plain",
            template: SERVED_BODY.to_string(),
            status: DECLARED_STATUS,
        },
        Case {
            case: "a numeric literal, which is the status the response is sent with",
            route: "created",
            template: overriding("201"),
            status: 201,
        },
        Case {
            case: "another numeric literal, in the 4xx class",
            route: "gone",
            template: overriding("410"),
            status: 410,
        },
        Case {
            case: "a number the template bound with #set, which is still a number",
            route: "bound",
            template: format!("#set($code = 302)\n{}", overriding("$code")),
            status: 302,
        },
        Case {
            case: "a DOUBLE-quoted literal spelling a status, which states no status",
            route: "quoted",
            template: overriding("\"201\""),
            status: DECLARED_STATUS,
        },
        Case {
            case: "a SINGLE-quoted literal spelling a status, which states no status",
            route: "apostrophed",
            template: overriding("'503'"),
            status: DECLARED_STATUS,
        },
        Case {
            case: "a quoted status the template bound with #set, which is text",
            route: "boundtext",
            template: format!("#set($code = \"404\")\n{}", overriding("$code")),
            status: DECLARED_STATUS,
        },
        Case {
            case: "a status spelled by the caller's own query string, which is text",
            route: "fromquery",
            template: overriding("$input.params('code')"),
            status: DECLARED_STATUS,
        },
        Case {
            case: "a boolean, which is no status either",
            route: "boolean",
            template: overriding("true"),
            status: DECLARED_STATUS,
        },
    ]
}

#[tokio::test]
async fn an_override_of_the_status_is_applied_only_where_it_states_a_number() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "response-override-status").await;
    let rows = rows();
    // One `MOCK` method per row, each answering the DECLARED status with its own response
    // template: what varies between the rows is only what that template states.
    for case in &rows {
        mock_resource(
            &gateway,
            &api,
            &root,
            case.route,
            "GET",
            &DECLARED_STATUS.to_string(),
            json_template(&case.template),
        )
        .await;
    }
    deploy(&gateway, &api, STAGE).await;

    for case in &rows {
        // The query string carries a value that SPELLS a status, so the row reading it off
        // the call has a status number to state and still states none.
        let answered = invoke(
            &gateway,
            &api,
            "GET",
            &format!("/{STAGE}/{}?code=201", case.route),
            &[("content-type", "application/json")],
        )
        .await;
        assert_eq!(
            answered.status, case.status,
            "case {:?}: expected the stage to answer {}, got {} with {:?}",
            case.case, case.status, answered.status, answered.body
        );
        assert_eq!(
            answered.body, SERVED_BODY,
            "case {:?}: expected the template's own body, got {:?}",
            case.case, answered.body
        );
    }
}
