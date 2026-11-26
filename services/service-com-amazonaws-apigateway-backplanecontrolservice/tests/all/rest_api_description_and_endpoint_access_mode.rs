// SPDX-License-Identifier: CC0-1.0

//! What `CreateRestApi` and `CreateDomainName` REFUSE about two of the members a record of
//! this control plane carries, asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder, the domain-name
//! create helper and the PATCH wrappers are this crate's existing harness, reused from
//! `rest_api_lifecycle`, `domain_name_collection` and `patch_updates`.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call:
//!
//! 1. A `description` the request SUPPLIES describes the api, so a `CreateRestApi` carrying
//!    one of no characters is refused with that member's own fixed sentence and NO api is
//!    created; a description carrying any character at all is stored as written, and a
//!    request that supplies none is accepted with the member absent.
//! 2. `endpointAccessMode` is "only available for RestApis/DomainNames that use security
//!    policies that start with `SecurityPolicy_`"
//!    (`smithy_query(query="doc", shape="CreateRestApiRequest,CreateDomainNameRequest",
//!    member="endpointAccessMode")`), so a request that would store a mode on a record whose
//!    `securityPolicy` is not one of those is refused with one fixed sentence and stores
//!    nothing — on either resource, through the create and through the patch.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::create_rest_api::{
    EMPTY_DESCRIPTION, EMPTY_NAME,
};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::types::{
    ENDPOINT_ACCESS_MODE_VALUES, SECURITY_POLICY_VALUES,
};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    admits_endpoint_access_mode, DEFAULT_SECURITY_POLICY, ENDPOINT_ACCESS_MODE_UNSUPPORTED,
    NO_SUCH_DOMAIN_NAME,
};

use crate::domain_name_collection::create_request;
use crate::patch_updates::{op, patch, replace};
use crate::rest_api_lifecycle::{page_items, string_member, Answer, Gateway};

const BAD_REQUEST: &str = "BadRequestException";
const NOT_FOUND: &str = "NotFoundException";

/// Assert `answer` is a refusal with `status`, `code` and exactly `message`.
fn assert_refusal(answer: &Answer, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some(code),
        "case {case}: expected {code}, got {:?} with {:?}",
        answer.error,
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

/// `CreateRestApi` with `body`.
async fn create_api_with(gateway: &Gateway, body: Value) -> Answer {
    gateway.call("POST", "/restapis", Some(body)).await
}

/// Whether any api of the account is named `name`, read through `GetRestApis`: what a
/// refused create must leave false.
async fn an_api_is_named(gateway: &Gateway, name: &str) -> bool {
    let listed = gateway.call("GET", "/restapis?limit=500", None).await;
    assert_eq!(
        listed.status, 200,
        "case GetRestApis: expected 200, got {} with {:?}",
        listed.status, listed.body
    );
    page_items(&listed.body, "GetRestApis")
        .iter()
        .any(|api| api["name"].as_str() == Some(name))
}

#[tokio::test]
async fn a_rest_api_created_with_a_description_of_no_characters_is_refused_and_nothing_is_stored() {
    let gateway = Gateway::new();

    // Rows differ along what the rule generalises over: whether the member is supplied at
    // all, and — when it is — how many characters it carries, which characters, and whether
    // they are blank. Only the value of NO characters describes nothing.
    let rows: Vec<(&str, Option<&str>, Option<&str>)> = vec![
        ("no description at all", None, None),
        ("a description of no characters", Some(""), None),
        ("one character", Some("x"), Some("x")),
        ("a single space", Some(" "), Some(" ")),
        ("a sentence", Some("the pet store's api"), Some("the pet store's api")),
        ("text outside ASCII", Some("l'API du magasin ✅"), Some("l'API du magasin ✅")),
    ];

    for (index, (case, supplied, stored)) in rows.iter().enumerate() {
        let name = format!("described-api-{index}");
        let mut request = json!({ "name": name });
        if let Some(description) = supplied {
            request["description"] = json!(description);
        }
        let answer = create_api_with(&gateway, request).await;

        // A SUPPLIED description of no characters is the one refused row; every other row
        // is created and reports the description it was given.
        if *supplied == Some("") {
            assert_refusal(&answer, 400, BAD_REQUEST, EMPTY_DESCRIPTION, case);
            assert!(
                !an_api_is_named(&gateway, &name).await,
                "case {case}: a refused create must leave no api named {name:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let reported = answer.body["description"].as_str();
        assert_eq!(
            reported, *stored,
            "case {case}: expected the create to report {stored:?}, got {reported:?}"
        );
        // Round trip: the read reports the record the create stored.
        let id = string_member(&answer.body, "id", case);
        let read = gateway.call("GET", &format!("/restapis/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetRestApi, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["description"].as_str(),
            *stored,
            "case {case}: GetRestApi must report {stored:?}, got {:?}",
            read.body["description"]
        );
    }
}

#[tokio::test]
async fn an_empty_name_and_an_empty_description_are_refused_with_their_own_member_sentence() {
    // The two members a `CreateRestApi` refuses as EMPTY carry one sentence each, naming
    // the member and quoting nothing of the request back — so a client reading the message
    // learns which member to fix.
    let gateway = Gateway::new();
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "an empty name",
            json!({ "name": "", "description": "described" }),
            EMPTY_NAME,
        ),
        (
            "an empty description",
            json!({ "name": "empty-description", "description": "" }),
            EMPTY_DESCRIPTION,
        ),
        (
            "both members empty",
            json!({ "name": "", "description": "" }),
            EMPTY_NAME,
        ),
    ];
    for (case, request, message) in &rows {
        let answer = create_api_with(&gateway, request.clone()).await;
        assert_refusal(&answer, 400, BAD_REQUEST, message, case);
    }
}

/// Every `(securityPolicy, endpointAccessMode)` pair the rule is asserted over, with
/// whether the record it describes may exist.
///
/// The policies are the modelled catalog itself
/// (`smithy_query(query="shape", shape="SecurityPolicy")`, as the crate's generated
/// `SECURITY_POLICY_VALUES` holds it) rather than a list this suite invented, so every
/// policy AWS declares is driven — the ones the issue cited and the ones it did not — and
/// the mode values are the whole `EndpointAccessMode` catalog. The request that names no
/// policy at all is a row too: such a record carries the service's default, which is not
/// one of the policies that admit a mode.
fn access_mode_rows() -> Vec<(String, Option<&'static str>, Option<&'static str>, bool)> {
    let mut rows: Vec<(String, Option<&'static str>, Option<&'static str>, bool)> = Vec::new();
    for policy in SECURITY_POLICY_VALUES {
        let admits = admits_endpoint_access_mode(policy);
        for mode in ENDPOINT_ACCESS_MODE_VALUES {
            rows.push((
                format!("securityPolicy {policy} with endpointAccessMode {mode}"),
                Some(policy),
                Some(mode),
                admits,
            ));
        }
        // The same policy with NO mode is always acceptable: the rule is about the member
        // being set, not about the policy.
        rows.push((
            format!("securityPolicy {policy} with no endpointAccessMode"),
            Some(policy),
            None,
            true,
        ));
    }
    for mode in ENDPOINT_ACCESS_MODE_VALUES {
        rows.push((
            format!("no securityPolicy with endpointAccessMode {mode}"),
            None,
            Some(mode),
            // The record carries the service's default policy, which admits no mode.
            admits_endpoint_access_mode(DEFAULT_SECURITY_POLICY),
        ));
    }
    rows.push((
        "neither member".to_string(),
        None,
        None,
        true,
    ));
    rows
}

#[tokio::test]
async fn a_rest_api_takes_an_endpoint_access_mode_only_under_a_policy_that_admits_one() {
    let gateway = Gateway::new();

    for (index, (case, policy, mode, accepted)) in access_mode_rows().iter().enumerate() {
        let name = format!("access-mode-api-{index}");
        let mut request = json!({ "name": name });
        if let Some(policy) = policy {
            request["securityPolicy"] = json!(policy);
        }
        if let Some(mode) = mode {
            request["endpointAccessMode"] = json!(mode);
        }
        let answer = create_api_with(&gateway, request).await;

        if !accepted {
            assert_refusal(
                &answer,
                400,
                BAD_REQUEST,
                ENDPOINT_ACCESS_MODE_UNSUPPORTED,
                case,
            );
            assert!(
                !an_api_is_named(&gateway, &name).await,
                "case {case}: a refused create must leave no api named {name:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let id = string_member(&answer.body, "id", case);
        let read = gateway.call("GET", &format!("/restapis/{id}"), None).await;
        assert_eq!(
            read.body["endpointAccessMode"].as_str(),
            *mode,
            "case {case}: GetRestApi must report the stored mode {mode:?}, got {:?}",
            read.body["endpointAccessMode"]
        );
        let expected_policy = policy.unwrap_or(DEFAULT_SECURITY_POLICY);
        assert_eq!(
            read.body["securityPolicy"].as_str(),
            Some(expected_policy),
            "case {case}: GetRestApi must report the stored policy {expected_policy:?}, got {:?}",
            read.body["securityPolicy"]
        );
    }
}

#[tokio::test]
async fn a_patch_cannot_leave_a_rest_api_with_a_mode_its_policy_does_not_admit() {
    let gateway = Gateway::new();
    // A policy that admits a mode, and one that does not: the first entry of the modelled
    // catalog carrying the prefix, and the service's own default.
    let admitting = SECURITY_POLICY_VALUES
        .iter()
        .copied()
        .find(|policy| admits_endpoint_access_mode(policy))
        .expect("the modelled catalog declares a SecurityPolicy_ policy");
    let mode = ENDPOINT_ACCESS_MODE_VALUES[0];

    // An api created under the default policy takes no mode through the patch either.
    let created = create_api_with(&gateway, json!({ "name": "patched-access-mode" })).await;
    let id = string_member(&created.body, "id", "CreateRestApi");
    let uri = format!("/restapis/{id}");
    let refused = patch(&gateway, &uri, vec![op("add", "/endpointAccessMode", Some(mode))]).await;
    assert_refusal(
        &refused,
        400,
        BAD_REQUEST,
        ENDPOINT_ACCESS_MODE_UNSUPPORTED,
        "add /endpointAccessMode under the default policy",
    );
    let read = gateway.call("GET", &uri, None).await;
    assert!(
        read.body["endpointAccessMode"].is_null(),
        "case add /endpointAccessMode under the default policy: the refused patch must leave \
         the api without one, got {:?}",
        read.body["endpointAccessMode"]
    );

    // Under a policy that admits one, the same op is applied and read back.
    let ordered = patch(
        &gateway,
        &uri,
        vec![
            replace("/securityPolicy", admitting),
            op("add", "/endpointAccessMode", Some(mode)),
        ],
    )
    .await;
    assert_eq!(
        ordered.status, 200,
        "case the policy and the mode in one request: expected 200, got {} with {:?}",
        ordered.status, ordered.body
    );
    assert_eq!(
        ordered.body["endpointAccessMode"].as_str(),
        Some(mode),
        "case the policy and the mode in one request: expected the mode back, got {:?}",
        ordered.body["endpointAccessMode"]
    );

    // And moving the policy back off that set is refused while the mode is still set, so
    // the pair can never be stored through either member.
    let reverted = patch(
        &gateway,
        &uri,
        vec![replace("/securityPolicy", DEFAULT_SECURITY_POLICY)],
    )
    .await;
    assert_refusal(
        &reverted,
        400,
        BAD_REQUEST,
        ENDPOINT_ACCESS_MODE_UNSUPPORTED,
        "replace /securityPolicy back while a mode is set",
    );
    let read = gateway.call("GET", &uri, None).await;
    assert_eq!(
        read.body["securityPolicy"].as_str(),
        Some(admitting),
        "case replace /securityPolicy back while a mode is set: the refused patch must leave \
         the stored policy, got {:?}",
        read.body["securityPolicy"]
    );
}

#[tokio::test]
async fn a_domain_name_takes_an_endpoint_access_mode_only_under_a_policy_that_admits_one() {
    let gateway = Gateway::new();

    for (index, (case, policy, mode, accepted)) in access_mode_rows().iter().enumerate() {
        let domain_name = format!("access-mode-{index}.example.com");
        // An edge-optimized record (the endpoint type a request naming none resolves to)
        // needs no listener, so the table costs no sockets; which endpoint serves a domain
        // name is the `domain_name_collection` suite's subject rather than this one's.
        let mut body = json!({});
        if let Some(policy) = policy {
            body["securityPolicy"] = json!(policy);
        }
        if let Some(mode) = mode {
            body["endpointAccessMode"] = json!(mode);
        }
        let answer = gateway
            .call(
                "POST",
                "/domainnames",
                Some(create_request(&domain_name, body)),
            )
            .await;
        let read_uri = format!("/domainnames/{domain_name}");

        if !accepted {
            assert_refusal(
                &answer,
                400,
                BAD_REQUEST,
                ENDPOINT_ACCESS_MODE_UNSUPPORTED,
                case,
            );
            // A refused create stores nothing, so the name addresses no record at all.
            let read = gateway.call("GET", &read_uri, None).await;
            assert_refusal(&read, 404, NOT_FOUND, NO_SUCH_DOMAIN_NAME, case);
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let read = gateway.call("GET", &read_uri, None).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetDomainName, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["endpointAccessMode"].as_str(),
            *mode,
            "case {case}: GetDomainName must report the stored mode {mode:?}, got {:?}",
            read.body["endpointAccessMode"]
        );
    }
}
