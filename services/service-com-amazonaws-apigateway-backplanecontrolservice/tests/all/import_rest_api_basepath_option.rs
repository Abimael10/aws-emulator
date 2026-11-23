// SPDX-License-Identifier: CC0-1.0

//! How `ImportRestApi` refuses a `basepath` parameter value it does not recognise.
//!
//! The rule under test, one level above the recorded case (`basepath=wrap`):
//!
//! > `ImportRestApi` takes its `parameters` as query parameters and documents exactly three
//! > values for the `basepath` key — "to handle imported `basepath`, set `parameters` as
//! > `basepath=ignore`, `basepath=prepend` or `basepath=split`". A value that is none of
//! > them is refused `400 BadRequestException` with API Gateway's own fixed sentence,
//! > "Unrecognized base path option: ", followed by the value the REQUEST supplied and
//! > nothing else: the sentence names the option it could not recognise rather than
//! > enumerating the options it would have taken, and the import creates no API.
//!
//! Every case drives the real wire path — the operation's own route
//! (`POST /restapis?mode=import`) through the multi-service router — so what is asserted is
//! what a client sees. The emulator, the import call and the API count are this crate's
//! existing harness ([`crate::rest_api_create_from_import`]).
//!
//! The rows differ along what the rule generalises over: the recorded word, a documented
//! value in another case (the values are matched exactly, so a case variant is not one of
//! them), a word of the neighbouring parameter's catalog, two documented values run
//! together, a value that only extends a documented one, and the empty value — with the
//! three documented values driven beside them, so a refusal is about the value and not
//! about the key.

use crate::rest_api_create_from_import::{api_count, import};
use crate::rest_api_lifecycle::{assert_failure, Gateway};

/// The modelled refusal class: `ImportRestApi` models `BadRequestException` at HTTP 400.
const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");

/// The three values the `basepath` key documents, which must still be imported.
const DOCUMENTED: [&str; 3] = ["ignore", "prepend", "split"];

/// A definition that declares a base path, so each documented value has something to apply:
/// Swagger 2.0's own `basePath` member.
const DEFINITION: &str = "{\"swagger\": \"2.0\", \"info\": {\"title\": \"based\"}, \
                          \"basePath\": \"/v1/beta\", \
                          \"paths\": {\"/pets\": {\"get\": {}}}}";

/// The sentence API Gateway refuses an unrecognised `basepath` option with, written out here
/// rather than taken from the handler's own renderer: the FIXED text is what this suite
/// pins, and the one VARIABLE part is the value the request supplied.
fn unrecognized(value: &str) -> String {
    format!("Unrecognized base path option: {value}")
}

#[tokio::test]
async fn an_unrecognized_basepath_option_is_refused_with_the_value_the_request_supplied() {
    let gateway = Gateway::new();
    let before = api_count(&gateway, "the account before any refused import").await;

    // Each value is URL-safe as it stands, so what reaches the parameter is what the row
    // names — which is what the sentence has to quote back.
    let rows = [
        ("the recorded word", "wrap"),
        ("a documented value in another case", "Ignore"),
        ("a documented value in upper case", "SPLIT"),
        ("a word of the endpoint-type catalog", "REGIONAL"),
        ("two documented values run together", "ignore,prepend"),
        ("a value extending a documented one", "prepended"),
        ("the empty value", ""),
    ];

    for (case, value) in rows {
        let answer = import(&gateway, DEFINITION, false, &[("basepath", value)]).await;
        assert_failure(&answer, BAD_REQUEST.0, BAD_REQUEST.1, case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(unrecognized(value).as_str()),
            "case {case:?}: expected the message {:?}, got {:?}",
            unrecognized(value),
            answer.body["message"]
        );

        // A refused import creates no API, so the sentence is the whole of what happened.
        let after = api_count(&gateway, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused import must create no API, expected {before}, got {after}"
        );
    }
}

#[tokio::test]
async fn every_documented_basepath_option_is_still_imported() {
    // The counterpart of the table above: the three values the member documents are
    // admitted, so every refusal there is about the VALUE and not about the key or the
    // definition.
    let gateway = Gateway::new();
    for value in DOCUMENTED {
        let case = format!("basepath={value:?}");
        let answer = import(&gateway, DEFINITION, false, &[("basepath", value)]).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201 from ImportRestApi, got {} with {:?}",
            answer.status, answer.body
        );
    }
}
