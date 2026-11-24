// SPDX-License-Identifier: CC0-1.0

//! Each MEMBER a response template's override object names is applied on its OWN type:
//! `#set($context.responseOverride.status = …)` and
//! `#set($context.responseOverride.header.<name> = …)` stated by one template are two
//! independent statements.
//!
//! The status a response is sent with is a NUMBER, where a header is a parameter and carries
//! TEXT. So the RULE read here: an assignment whose value is not of the assigned member's type
//! states nothing for THAT member and nothing else — a status assigned a quoted literal, a
//! value read off the call or a boolean leaves the status the integration response declares,
//! while every header override the same template states is still applied, whichever order they
//! are written in.
//!
//! The sibling suite beside this one reads the status half of the rule over the forms the
//! assigned expression takes; what the rows here vary is which members one template states at
//! once — a discarded status beside one header, beside two, beside a header read off the call,
//! and each of them with the header before the status as well as after — so what is asserted
//! is that discarding one member's assignment leaves the others untouched.
//!
//! Every case drives the real wire path: a request to the api's own invoke host through the
//! multi-service router, with the control plane driven through the same emulator.

use super::{deploy, invoke, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, Gateway};

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The status the integration response and the method response DECLARE, which is what a
/// response template that states no status of its own — or states one of the wrong type — is
/// sent with.
const DECLARED_STATUS: u16 = 200;

/// The body every response template below renders, so a row can tell a served response from a
/// gateway refusal whatever status it carries.
const SERVED_BODY: &str = r#"{"served":true}"#;

/// The header a row's template overrides, lowercased as a response header compares.
const OVERRIDDEN_HEADER: &str = "x-overridden";

/// A second header, for the row stating two of them beside a discarded status.
const SECOND_HEADER: &str = "x-also-overridden";

/// One row: the response template the method's integration response carries, the status the
/// stage is expected to answer with, and what each header the row cares about is expected to
/// carry (`None` for a header the answer must not carry at all).
struct Case {
    case: &'static str,
    /// The path part the row's own method sits on, which is also its resource name.
    route: &'static str,
    template: String,
    status: u16,
    headers: Vec<(&'static str, Option<&'static str>)>,
}

/// The rows of the table, one per set of members a single template states at once.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "a quoted status beside a header override, which keeps the declared status \
                   and applies the header",
            route: "quotedheader",
            template: format!(
                "#set($context.responseOverride.status = \"201\")\n\
                 #set($context.responseOverride.header.X-Overridden = \"string-literal\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("string-literal"))],
        },
        Case {
            case: "a numeric status beside a header override, which applies both",
            route: "numericheader",
            template: format!(
                "#set($context.responseOverride.status = 201)\n\
                 #set($context.responseOverride.header.X-Overridden = \"numeric-literal\")\n\
                 {SERVED_BODY}"
            ),
            status: 201,
            headers: vec![(OVERRIDDEN_HEADER, Some("numeric-literal"))],
        },
        Case {
            case: "the header stated BEFORE a quoted status, which is applied all the same",
            route: "headerfirst",
            template: format!(
                "#set($context.responseOverride.header.X-Overridden = \"before\")\n\
                 #set($context.responseOverride.status = \"503\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("before"))],
        },
        Case {
            case: "two headers beside a discarded status, which are both applied",
            route: "twoheaders",
            template: format!(
                "#set($context.responseOverride.status = '404')\n\
                 #set($context.responseOverride.header.X-Overridden = \"one\")\n\
                 #set($context.responseOverride.header.X-Also-Overridden = \"two\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![
                (OVERRIDDEN_HEADER, Some("one")),
                (SECOND_HEADER, Some("two")),
            ],
        },
        Case {
            case: "a status read off the call beside a header read off the same call, where \
                   the header carries the text and the status stays declared",
            route: "fromcall",
            template: format!(
                "#set($context.responseOverride.status = $input.params('code'))\n\
                 #set($context.responseOverride.header.X-Overridden = \
                 $input.params('code'))\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("201"))],
        },
        Case {
            case: "a status interpolating a number the template bound, which is text, beside \
                   a header",
            route: "interpolated",
            template: format!(
                "#set($code = 201)\n\
                 #set($context.responseOverride.status = \"$code\")\n\
                 #set($context.responseOverride.header.X-Overridden = \"interpolated\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("interpolated"))],
        },
        Case {
            case: "a status that is a number but not an integral one, beside a header, which \
                   keeps the declared status and still applies the header",
            route: "fractional",
            template: format!(
                "#set($context.responseOverride.status = 201.5)\n\
                 #set($context.responseOverride.header.X-Overridden = \"fractional\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("fractional"))],
        },
        Case {
            case: "a boolean status beside a header, which keeps the declared status",
            route: "booleanheader",
            template: format!(
                "#set($context.responseOverride.status = true)\n\
                 #set($context.responseOverride.header.X-Overridden = \"boolean\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, Some("boolean"))],
        },
        Case {
            case: "a quoted status and no header at all, which overrides nothing",
            route: "quotedalone",
            template: format!(
                "#set($context.responseOverride.status = \"201\")\n{SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![(OVERRIDDEN_HEADER, None), (SECOND_HEADER, None)],
        },
        Case {
            case: "a header override and no status assignment at all, which leaves the \
                   declared status",
            route: "headeralone",
            template: format!(
                "#set($context.responseOverride.header.X-Overridden = \"only-header\")\n\
                 {SERVED_BODY}"
            ),
            status: DECLARED_STATUS,
            headers: vec![
                (OVERRIDDEN_HEADER, Some("only-header")),
                (SECOND_HEADER, None),
            ],
        },
    ]
}

#[tokio::test]
async fn each_overridden_member_is_applied_on_its_own_type() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "override-member-independence").await;
    let rows = rows();
    // One `MOCK` method per row, each answering the DECLARED status with its own response
    // template: what varies between the rows is only which members that template states.
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
        // The query string carries a value that SPELLS a status, so the row reading one off
        // the call has a status number to state and still states none — while the header
        // reading the same value does carry it.
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
        for (name, expected) in &case.headers {
            let actual = answered.header(name);
            assert_eq!(
                actual, *expected,
                "case {:?}: expected the header {name} to be {expected:?}, got {actual:?}",
                case.case
            );
        }
    }
}
