// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a FUNCTION URL request the endpoint REFUSED leaves behind on the
//! function's `AWS/Lambda` series, read back where a client reads it — with CloudWatch mounted
//! beside Lambda on one engine.
//!
//! RULE: a function's `AWS/Lambda` series measures the INVOCATIONS Lambda performed of it, so a
//! request a function URL endpoint refuses before the handler runs — the front door turning away
//! a caller it did not authenticate, and the URL's own authorizer turning away a caller the
//! function's resource-based policy does not admit — is answered the endpoint's own fixed `403`
//! document and contributes nothing to `Invocations`, `Errors` or `Duration`. A client alarming
//! on a function's error rate therefore never sees a refused request, which never reached the
//! function, as a failure of it.
//!
//! The two halves are one contract and are asserted on the same requests: a refusal that served
//! the API's own error envelope instead of the endpoint's document, and a refusal that was
//! nonetheless metered as an invocation, are both ways of reporting a run that never happened.
//! What the refusal DOCUMENT is, per class, is pinned over its own table in
//! `function_url_unauthenticated_refusal.rs` and `function_url_authorizer_refusal.rs`; what the
//! series carries for a request that WAS served is pinned in `function_url_invocation_series.rs`.
//! What only this suite reaches is the pair: the refused request, and the series that must know
//! nothing of it.
//!
//! The rows differ along what the rule generalises over: WHY the endpoint refused (no credential
//! at all, a credential this emulator holds no identity for, and an authorization decision that
//! admitted nobody), the `AuthType` the URL enforces, the addressing the client reached the
//! endpoint by, and the method, path and body the refused request carried. The closing test
//! drives refusals and then one served request through ONE function, so a meter that counted
//! requests rather than runs cannot pass either test.
//!
//! Every series name, dimension and unit here is written as a literal taken from the AWS Lambda
//! metric reference rather than read from this crate's constants, so a series published under a
//! name no AWS reader would ask for fails here instead of agreeing with itself. Nothing waits on
//! the wall clock: the measurements of a served request are published by the run it is answered
//! off, so each read happens once the requests it is about have been answered.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{make_activation_due, ROLE};
use crate::function_url_data_plane::{
    control, given_configured_url, grant, send, statement, Answer, Endpoint,
    AUTHORIZER_FORBIDDEN_MESSAGE, FRONT_DOOR_FORBIDDEN_MESSAGE,
};
use crate::function_url_invocation_series::{
    datapoints, function_dimensions, resource_dimensions, total,
};
use crate::function_url_unauthenticated_refusal::{
    borrowed, headers, Credential, UNKNOWN_CREDENTIAL_BODY,
};
use crate::synchronous_invoke_run::package;

/// The names the AWS Lambda metric reference gives the series under test, and the units it
/// reports them in. The dimension sets they are reported per are the ones
/// `function_url_invocation_series.rs` reads the same series by.
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`INVOCATIONS_SERIES`].
const DURATION_SERIES: &str = "Duration";
/// See [`INVOCATIONS_SERIES`].
const ERRORS_SERIES: &str = "Errors";
/// See [`INVOCATIONS_SERIES`].
const MILLISECONDS_UNIT: &str = "Milliseconds";
/// See [`INVOCATIONS_SERIES`].
const COUNT_UNIT: &str = "Count";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function here is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `Timeout` every function here is given, in seconds. Generous on purpose, because nothing
/// here is about it: a handler stopped at this deadline would be reported as a run that errored,
/// so a tight value would turn a loaded host into a failure the handler never raised. Well inside
/// the range the model allows the member (`smithy_query(query="shape", shape="Timeout")`: 1 to
/// 5400).
const GENEROUS_TIMEOUT_SECS: u32 = 300;

/// The `MemorySize` every function here is given, in MB.
const MEMORY_SIZE_MB: u32 = 512;

/// The emulator's own endpoint, which a client that resolves no AWS hostname reaches.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved path segment a data plane is named behind on that endpoint
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), and the label that names
/// this plane under it.
const EMULATOR_NAMESPACE: &str = "_aws";
/// See [`EMULATOR_NAMESPACE`].
const PLANE_LABEL: &str = "lambda-url";

/// The action a function URL's own invocation is granted under.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`), which is the
/// principal an `AWS_IAM` endpoint's grant names.
const ACCOUNT: &str = "000000000000";

/// The member either fixed refusal document carries its sentence in.
const REFUSAL_MEMBER: &str = "Message";

/// The member this service's AWS-JSON error envelope names an error code in, which no function
/// URL refusal carries either.
const ENVELOPE_DISCRIMINATOR: &str = "__type";

/// What the handler answers, so a request that reached the function is visibly served BY THE
/// FUNCTION and a refused one visibly is not.
const MARKER: &str = "the-url-handler-ran";

/// The handler every function here runs.
fn source() -> String {
    format!(
        "def lambda_handler(event, context):\n    \
         return {{'statusCode': 200, 'body': '{MARKER}'}}\n"
    )
}

/// Lambda and CloudWatch on one engine: the series Lambda publishes is CloudWatch's, so it is
/// read in-process through CloudWatch's own surface — the shape `core/router/src/lib.rs` mounts
/// them in. Request authentication is OFF, so this router builds the resources and reads the
/// series; what the rows below are admitted by is the authenticating router's to say.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// The same emulator with request authentication ON, as it runs when it is started with it: the
/// router every refused request below is sent through.
fn authenticating(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = emulator(engine);
    router.set_request_authentication(true);
    router
}

/// Create the function `name`, whose package RUNS, and bring it to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) {
    let created = control(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": GENEROUS_TIMEOUT_SECS,
            "MemorySize": MEMORY_SIZE_MB,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(&source())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateFunction {name:?} must succeed, got {}",
        created.summary()
    );
    // The activation the CLOCK owes this function, brought forward instead of waited out.
    make_activation_due(engine);
}

/// Which addressing a row reaches the endpoint by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressing {
    /// The host `FunctionUrlConfig$FunctionUrl` publishes.
    PublishedHost,
    /// The reserved local-endpoint path on the emulator's own endpoint, which a client that
    /// resolves no AWS hostname reaches the same URL by.
    LocalEndpointPath,
}

/// The host and the request target one addressing of `endpoint` reaches `path` at.
fn addressed(endpoint: &Endpoint, addressing: Addressing, path: &str) -> (String, String) {
    match addressing {
        Addressing::PublishedHost => (endpoint.host.clone(), path.to_string()),
        Addressing::LocalEndpointPath => (
            OWN_ENDPOINT.to_string(),
            format!(
                "/{EMULATOR_NAMESPACE}/{PLANE_LABEL}/{}{path}",
                endpoint.url_id
            ),
        ),
    }
}

/// Which refusal of the endpoint a row must read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// The FRONT DOOR's own refusal of a caller it did not authenticate: the fixed word, naming
    /// nothing of the request and nothing of authorization, because no authorization decision
    /// was reached for a caller that presented no credential at all.
    FrontDoor,
    /// The refusal of a caller whose CREDENTIAL this emulator holds no identity for, which says
    /// that much in the lower-case member beside the class: the client was not recognized. The
    /// sentence is the one `function_url_unauthenticated_refusal.rs` states for that class, so
    /// the two cannot drift apart.
    UnheldCredential,
    /// The URL's OWN AUTHORIZER refusing a caller the function's resource-based policy does not
    /// admit: the fixed word and the troubleshooting pointer AWS appends to it.
    Authorizer,
}

/// The document a refused request must read, asserted whole where the document is fixed whole.
///
/// Every class is a `403` carrying the endpoint's own document and never this service's AWS-JSON
/// API envelope — no `__type` discriminator — and never anything the handler produced.
fn assert_refused(case: &str, refusal: Refusal, answer: &Answer) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403, got {}",
        answer.summary()
    );
    assert!(
        !answer.text().contains(MARKER),
        "case {case:?}: expected nothing of the function's own answer, got {}",
        answer.summary()
    );
    let document = answer.json();
    assert_eq!(
        document.get(ENVELOPE_DISCRIMINATOR),
        None,
        "case {case:?}: a function URL refusal carries no {ENVELOPE_DISCRIMINATOR} \
         discriminator, got {}",
        answer.text()
    );
    match refusal {
        Refusal::FrontDoor => assert_eq!(
            document,
            json!({ REFUSAL_MEMBER: FRONT_DOOR_FORBIDDEN_MESSAGE }),
            "case {case:?}: expected the front door's whole document, got {}",
            answer.text()
        ),
        Refusal::Authorizer => assert_eq!(
            document,
            json!({ REFUSAL_MEMBER: AUTHORIZER_FORBIDDEN_MESSAGE }),
            "case {case:?}: expected the authorizer's whole document, got {}",
            answer.text()
        ),
        Refusal::UnheldCredential => {
            assert_eq!(
                answer.text(),
                UNKNOWN_CREDENTIAL_BODY,
                "case {case:?}: expected the whole document of a credential this emulator holds \
                 no identity for, {UNKNOWN_CREDENTIAL_BODY}, got {}",
                answer.text()
            );
            assert_eq!(
                document.get(REFUSAL_MEMBER),
                None,
                "case {case:?}: a credential refusal carries no {REFUSAL_MEMBER} member, got {}",
                answer.text()
            );
        }
    }
}

/// Every series under test, with the unit the AWS Lambda metric reference reports it in.
const SERIES: &[(&str, &str)] = &[
    (INVOCATIONS_SERIES, COUNT_UNIT),
    (DURATION_SERIES, MILLISECONDS_UNIT),
    (ERRORS_SERIES, COUNT_UNIT),
];

/// Assert that nothing at all was published about `function` on any of the three series, over
/// both dimension sets a Lambda invocation is metered on.
async fn assert_unmetered(router: &MultiServiceRouter, case: &str, function: &str) {
    for (series, unit) in SERIES {
        for dimensions in [function_dimensions(function), resource_dimensions(function)] {
            let points = datapoints(router, case, series, unit, &dimensions).await;
            assert!(
                points.is_empty(),
                "case {case:?}: {series} of {dimensions} must hold no datapoint for a request \
                 the endpoint refused, got {points:?}"
            );
        }
    }
}

/// One refused request: how it is credentialled, where it is sent, and which refusal it reads.
struct Row {
    case: &'static str,
    /// Which of the two endpoints it is sent to: the `AWS_IAM` URL or the `NONE` URL whose
    /// function grants its invocation to nobody.
    restricted: bool,
    credential: Credential,
    addressing: Addressing,
    method: &'static str,
    /// The path within the function URL the client asks for.
    path: &'static str,
    /// The body it sends, empty for a request sending none.
    body: &'static str,
    refusal: Refusal,
}

/// The rows, spanning the ways a function URL endpoint refuses a request before the handler runs.
const ROWS: &[Row] = &[
    Row {
        case: "no credential at all, on the AWS_IAM endpoint's published host",
        restricted: true,
        credential: Credential::None,
        addressing: Addressing::PublishedHost,
        method: "GET",
        path: "/probe",
        body: "",
        refusal: Refusal::FrontDoor,
    },
    Row {
        case: "no credential at all, on the local-endpoint path, carrying a body",
        restricted: true,
        credential: Credential::None,
        addressing: Addressing::LocalEndpointPath,
        method: "POST",
        path: "/orders?via=direct",
        body: r#"{"order":"a"}"#,
        refusal: Refusal::FrontDoor,
    },
    Row {
        case: "a credential this emulator holds no identity for, on the published host",
        restricted: true,
        credential: Credential::UnknownKey,
        addressing: Addressing::PublishedHost,
        method: "GET",
        path: "/probe",
        body: "",
        refusal: Refusal::UnheldCredential,
    },
    Row {
        case: "an anonymous caller of a NONE endpoint its function grants to nobody",
        restricted: false,
        credential: Credential::None,
        addressing: Addressing::PublishedHost,
        method: "GET",
        path: "/probe",
        body: "",
        refusal: Refusal::Authorizer,
    },
    Row {
        case: "the same anonymous caller on the local-endpoint path, with another method",
        restricted: false,
        credential: Credential::None,
        addressing: Addressing::LocalEndpointPath,
        method: "DELETE",
        path: "/orders/7",
        body: "",
        refusal: Refusal::Authorizer,
    },
];

/// The function whose `AWS_IAM` URL refuses a caller the front door did not authenticate.
const RESTRICTED_FUNCTION: &str = "url-unmetered-iam";
/// The function whose `NONE` URL is granted to nobody, so its own authorizer refuses the
/// anonymous caller.
const PUBLIC_FUNCTION: &str = "url-unmetered-public";

/// RULE: a request a function URL refuses before the handler runs reads the endpoint's own fixed
/// `403` document and is metered on nothing — neither `Invocations`, nor `Errors`, nor
/// `Duration`, on either dimension set.
#[tokio::test]
async fn a_refused_function_url_request_is_metered_on_no_series() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = emulator(&engine);

    given_function(&setup, &engine, RESTRICTED_FUNCTION).await;
    let restricted = given_configured_url(
        &setup,
        RESTRICTED_FUNCTION,
        None,
        "AWS_IAM",
        &Value::Null,
    )
    .await;
    // The grant that WOULD admit a signed caller, so the rows' refusals are the front door's and
    // not a missing authorization.
    grant(
        &setup,
        RESTRICTED_FUNCTION,
        None,
        &statement("iam-url", INVOKE_FUNCTION_URL, ACCOUNT, "AWS_IAM"),
    )
    .await;

    given_function(&setup, &engine, PUBLIC_FUNCTION).await;
    // A `NONE` URL granted to NOBODY: nothing authenticates its caller, so no `Principal` element
    // names it and the URL's own authorizer refuses it.
    let public =
        given_configured_url(&setup, PUBLIC_FUNCTION, None, "NONE", &Value::Null).await;

    let router = authenticating(&engine);
    for row in ROWS {
        let case = row.case;
        let endpoint = if row.restricted { &restricted } else { &public };
        let (host, target) = addressed(endpoint, row.addressing, row.path);
        let credentialled = headers(row.credential);
        let mut sent = borrowed(&credentialled);
        if !row.body.is_empty() {
            sent.push(("content-type", "application/json"));
        }
        let answer = send(
            &router,
            row.method,
            &host,
            &target,
            row.body.as_bytes(),
            &sent,
        )
        .await;
        assert_refused(case, row.refusal, &answer);
    }

    for function in [RESTRICTED_FUNCTION, PUBLIC_FUNCTION] {
        assert_unmetered(
            &setup,
            &format!("every refused request of {function}"),
            function,
        )
        .await;
    }
}

/// The function the closing test refuses and then serves.
const SERVED_FUNCTION: &str = "url-unmetered-then-served";

/// RULE, read the other way round on ONE function: the series counts the invocations that
/// REACHED the function, so after refusals and one served request it reports exactly that one
/// run — which is what tells a meter that counts runs from one that counts requests.
#[tokio::test]
async fn the_series_counts_the_served_request_and_none_of_the_refused_ones() {
    let case = "three refused requests and one served";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = emulator(&engine);
    given_function(&setup, &engine, SERVED_FUNCTION).await;
    let endpoint =
        given_configured_url(&setup, SERVED_FUNCTION, None, "NONE", &Value::Null).await;
    let router = authenticating(&engine);

    // Granted to nobody yet: the URL's own authorizer refuses the anonymous caller, three times
    // over, each on a request of its own.
    for (index, (method, path)) in [("GET", "/probe"), ("POST", "/orders"), ("GET", "/")]
        .iter()
        .enumerate()
    {
        let answer = send(&router, method, &endpoint.host, path, &[], &[]).await;
        assert_refused(
            &format!("{case}: refused request {index}"),
            Refusal::Authorizer,
            &answer,
        );
    }
    assert_unmetered(&setup, &format!("{case}: before the grant"), SERVED_FUNCTION).await;

    // What makes a `NONE` endpoint public is the function's own resource-based policy granting
    // the endpoint's action to everyone. From here the same unsigned client is SERVED.
    grant(
        &setup,
        SERVED_FUNCTION,
        None,
        &statement("public-url", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    let served = send(&router, "GET", &endpoint.host, "/probe", &[], &[]).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the granted endpoint must serve the handler's answer, got {}",
        served.summary()
    );
    assert!(
        served.text().contains(MARKER),
        "case {case:?}: expected the function's own answer, got {}",
        served.summary()
    );

    // One run reached the function, so each series reports on exactly that one.
    for dimensions in [
        function_dimensions(SERVED_FUNCTION),
        resource_dimensions(SERVED_FUNCTION),
    ] {
        let invocations = datapoints(&setup, case, INVOCATIONS_SERIES, COUNT_UNIT, &dimensions)
            .await;
        let counted = total(&invocations, "Sum");
        assert_eq!(
            counted, 1.0,
            "case {case:?}: {INVOCATIONS_SERIES} of {dimensions} must sum to the one run that \
             reached the function, got {counted} over {invocations:?}"
        );

        let durations =
            datapoints(&setup, case, DURATION_SERIES, MILLISECONDS_UNIT, &dimensions).await;
        let measured = total(&durations, "SampleCount");
        assert_eq!(
            measured, 1.0,
            "case {case:?}: {DURATION_SERIES} of {dimensions} must hold one measurement, got \
             {measured} over {durations:?}"
        );

        let errors = datapoints(&setup, case, ERRORS_SERIES, COUNT_UNIT, &dimensions).await;
        let reported = total(&errors, "SampleCount");
        assert_eq!(
            reported, 1.0,
            "case {case:?}: {ERRORS_SERIES} of {dimensions} must report on the one run, got \
             {reported} over {errors:?}"
        );
        let failed = total(&errors, "Sum");
        assert_eq!(
            failed, 0.0,
            "case {case:?}: {ERRORS_SERIES} of {dimensions} must sum to 0 when the one run \
             returned, got {failed} over {errors:?}"
        );
    }
}
