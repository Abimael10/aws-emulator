// SPDX-License-Identifier: CC0-1.0

//! The window a hybrid activation's `ExpirationDate` has to fall in.
//!
//! RULE: `CreateActivation` mints an activation only for an expiry that is ahead of now and
//! no further ahead than the documented lead — "You can specify a date up to 30 days in
//! advance" (`smithy_query(query="doc", shape="CreateActivationRequest",
//! member="ExpirationDate")`) — and refuses a date outside that window with
//! `ValidationException`, minting nothing: an activation already expired, or one expiring
//! past the window, is one no machine could ever register with.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and every refusal is
//! checked against what `DescribeActivations` reports, so a refusal that persisted an
//! activation anyway cannot pass.
//!
//! The table's rows differ along the one axis the rule generalises over: how far from now
//! the requested expiry sits — far in the past, a moment in the past, now itself, a moment
//! ahead, the lead itself, and a moment and a year past the lead.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// How far ahead of now an activation may be asked to expire, in seconds: the documented
/// "up to 30 days in advance".
const MAX_LEAD_SECONDS: f64 = 30.0 * 24.0 * 60.0 * 60.0;

/// How Systems Manager words a refusal of an expiry outside that window — entirely fixed
/// text, naming neither the date asked for nor the bound.
const OUT_OF_WINDOW: &str = "expirationDate must be greater than now and lesser than 30 days.";

/// How much slack a row allows for the clock running on between building the request and
/// the handler reading it, so no row's outcome depends on how fast the host is: a row that
/// must be accepted sits at least this far inside the window, and one that must be refused
/// at least this far outside it.
const SLACK_SECONDS: f64 = 60.0;

/// Now, as the epoch seconds an `awsJson1_1` timestamp travels as.
fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Every activation `DescribeActivations` reports for this Region.
async fn reported(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, response) = call(router, "DescribeActivations", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeActivations must be served, got {status} with {response}"
    );
    response["ActivationList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One row: how far from now the expiry the create asks for sits, and whether the service
/// mints an activation for it.
struct Row {
    case: &'static str,
    /// Seconds from now, which the row's request carries as its `ExpirationDate`.
    offset: f64,
    minted: bool,
}

/// The rows: both sides of each end of the window, the ends themselves, and far outside it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an expiry a year in the past",
            offset: -365.0 * 24.0 * 60.0 * 60.0,
            minted: false,
        },
        Row {
            case: "an expiry a day in the past",
            offset: -24.0 * 60.0 * 60.0,
            minted: false,
        },
        Row {
            case: "an expiry a minute in the past",
            offset: -SLACK_SECONDS,
            minted: false,
        },
        Row {
            case: "an expiry an hour ahead",
            offset: 60.0 * 60.0,
            minted: true,
        },
        Row {
            case: "an expiry a day ahead, the documented default's own lifetime",
            offset: 24.0 * 60.0 * 60.0,
            minted: true,
        },
        Row {
            case: "an expiry just inside the documented lead",
            offset: MAX_LEAD_SECONDS - SLACK_SECONDS,
            minted: true,
        },
        Row {
            case: "an expiry just past the documented lead",
            offset: MAX_LEAD_SECONDS + SLACK_SECONDS,
            minted: false,
        },
        Row {
            case: "an expiry a year past the documented lead",
            offset: 365.0 * 24.0 * 60.0 * 60.0,
            minted: false,
        },
    ]
}

/// RULE: the window decides whether an activation is minted at all.
#[tokio::test]
async fn an_expiry_outside_the_documented_window_mints_nothing() {
    for Row {
        case,
        offset,
        minted,
    } in rows()
    {
        let router = router();
        let (status, response) = call(
            &router,
            "CreateActivation",
            &json!({
                "IamRole": "SSMServiceRole",
                "RegistrationLimit": 1,
                "ExpirationDate": epoch_now() + offset,
            }),
        )
        .await;

        if minted {
            assert_eq!(
                status, 200,
                "case {case:?}: an expiry inside the window mints an activation, got {status} \
                 with {response}"
            );
            assert!(
                response["ActivationId"].as_str().is_some()
                    && response["ActivationCode"].as_str().is_some(),
                "case {case:?}: a create that succeeds answers with the id and the code, got \
                 {response}"
            );
            let held = reported(&router, case).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: the activation minted is the one reported, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case:?}: an expiry outside the window is refused, got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            OUT_OF_WINDOW,
            "case {case:?}: expected the service's own sentence, got {response}"
        );
        assert!(
            response["ActivationId"].is_null() && response["ActivationCode"].is_null(),
            "case {case:?}: a refused create answers with no activation, got {response}"
        );
        let held = reported(&router, case).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused create mints nothing, got {held:?}"
        );
    }
}

/// RULE: the window is held against the REQUESTED date only — a create naming none is
/// served, because the date the service picks for it is inside the window by construction.
#[tokio::test]
async fn a_create_naming_no_expiry_is_served() {
    let router = router();
    let case = "no ExpirationDate at all";
    let (status, response) = call(
        &router,
        "CreateActivation",
        &json!({ "IamRole": "SSMServiceRole" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a create naming no expiry is served, got {status} with {response}"
    );
    let held = reported(&router, case).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: the activation minted is the one reported, got {held:?}"
    );
    let expires = held[0]["ExpirationDate"].as_f64().unwrap_or_default();
    let created = held[0]["CreatedDate"].as_f64().unwrap_or_default();
    assert!(
        expires > created && expires - created <= MAX_LEAD_SECONDS,
        "case {case:?}: the date the service picks is inside the window, got {:?}",
        held[0]
    );
    assert_eq!(
        held[0]["Expired"].as_bool(),
        Some(false),
        "case {case:?}: an activation inside the window is not expired, got {:?}",
        held[0]
    );
}
