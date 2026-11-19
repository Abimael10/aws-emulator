// SPDX-License-Identifier: CC0-1.0

//! A `LAMBDA` validator's refusal carries the INVOCATION's own answer, not a sentence
//! AppConfig wrote about it.
//!
//! The rule under test: a validator "provides a syntactic or semantic check to ensure the
//! configuration that you want to deploy functions as intended … the configuration
//! deployment or update can only proceed when the configuration data is valid"
//! (`smithy_query(query="doc",shape="Validator")`), and for a `LAMBDA` validator the
//! verdict belongs to the FUNCTION. So when that function runs and reports an error, the
//! call it gates is refused with `BadRequestException` / HTTP 400
//! (`smithy_query(query="operation",operation="StartDeployment,ValidateConfiguration")`)
//! whose `Message` RELAYS that invocation's own result as Lambda's answer reports it — the
//! `FunctionError` it signalled, the `LogResult` it carried and the response payload, which
//! for a run that raised is the error document the function's runtime wrote. A caller
//! therefore reads the error ITS OWN validator reported, down to the error class, the
//! message and the stack the handler raised with, rather than a restatement of it.
//!
//! Both calls a profile's validators gate go through the one evaluation seam, so each row
//! below is read through `StartDeployment` AND through `ValidateConfiguration`; the rows
//! differ along what the validator raises — the error class and the message, including one
//! that carries quotes and one that carries none at all — because the relayed document is
//! the runtime's rendering of whatever the handler raised. Each row also runs the SAME
//! function against a configuration it finds compliant, so what is asserted is the
//! function's verdict about the content and not the mere presence of a validator.

use serde_json::Value;

use crate::appconfig_workflow::{create_application, create_environment, Answer};
use crate::configuration_profile_validators::{
    assert_bad_request, deployment_numbers, emulator, given_profile, given_strategy,
    given_version, start_deployment, validate_configuration, validator,
};
use crate::deployment_tick_directive::given_function;

/// The fixed text of the relay: the two members of the `Invoke` answer that come before the
/// payload, as an answer to an invocation that asked for no log tail carries them — a run
/// that raised signals the `Unhandled` function error, and `LogResult` is absent.
const RELAYED_PREFIX: &str = "Function Error: Unhandled, Log Result: null, Payload: ";

/// A fragment of AppConfig's OWN account of a validator that did not pass, which a refusal
/// relaying the function's answer does not carry: the verdict is the function's.
const APPCONFIGS_OWN_ACCOUNT: &str = "did not pass the";

/// The configuration document every row's validator finds compliant.
const COMPLIANT: &[u8] = br#"{"compliant":true}"#;

/// The configuration document every row's validator rejects.
const NON_COMPLIANT: &[u8] = br#"{"compliant":false,"replicas":"two"}"#;

/// The source of a validator function: it reads the candidate configuration it was handed,
/// returns for a document it finds compliant, and reports a non-compliant one by RAISING —
/// which is how a validator function reports that verdict.
///
/// `raise` is the row's own statement, so the error class and the message the row expects
/// to read back out of the refusal are the ones its own handler raised with, and nothing
/// about them is assembled here.
fn validator_source(raise: &str) -> String {
    format!(
        "import base64\n\
         import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   document = json.loads(base64.b64decode(event[\"content\"]))\n\
         \x20   if document.get(\"compliant\") is True:\n\
         \x20       return None\n\
         \x20   {raise}\n"
    )
}

/// The payload a relayed refusal carries, as the document it is.
///
/// Asserts the relay's own shape on the way: the fixed text it opens with, that it restates
/// nothing in AppConfig's words, and that the payload is the JSON document a runtime's error
/// report is.
fn relayed_payload(case: &str, answer: &Answer) -> Value {
    assert_bad_request(case, answer);
    let message = answer.string("Message").unwrap_or_default();
    assert!(
        message.starts_with(RELAYED_PREFIX),
        "case {case:?}: expected a refusal opening {RELAYED_PREFIX:?}, got {message:?}"
    );
    assert!(
        !message.contains(APPCONFIGS_OWN_ACCOUNT),
        "case {case:?}: the verdict is the function's, so the refusal relays its answer \
         rather than restating it, got {message:?}"
    );
    let payload = &message[RELAYED_PREFIX.len()..];
    serde_json::from_str(payload).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the relayed payload to be the runtime's error document, got {payload:?} ({error})")
    })
}

/// The invocation the relayed payload names, asserted to be a non-empty id.
fn relayed_request_id(case: &str, payload: &Value) -> String {
    let request_id = payload["requestId"].as_str().unwrap_or_default().to_string();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: the relayed document names the invocation that reported the error, \
         got {payload}"
    );
    request_id
}

/// RULE: a `LAMBDA` validator that reports the content non-compliant refuses the call it
/// gates with the invocation's own answer relayed, and lets a configuration the SAME
/// function finds compliant through.
#[tokio::test]
async fn a_lambda_validator_refusal_relays_the_invocations_own_answer() {
    let (router, engine) = emulator();
    let application = create_application(&router, "relayed-verdict").await;
    let strategy = given_strategy(&router, "relayed-verdict-instant").await;

    // (case, the function's name, the statement its handler rejects with, the error class
    // the runtime reports for it, the message the runtime reports for it).
    let rows: [(&str, &str, &str, &str, &str); 4] = [
        (
            "the runtime's own error class",
            "runtime-error-validator",
            r#"raise RuntimeError("replicas must be an integer")"#,
            "RuntimeError",
            "replicas must be an integer",
        ),
        (
            "a message carrying quotes",
            "value-error-validator",
            r#"raise ValueError("flag 'checkout' declares no default")"#,
            "ValueError",
            "flag 'checkout' declares no default",
        ),
        (
            "an assertion the handler made",
            "assertion-validator",
            r#"assert False, "the document declares no version""#,
            "AssertionError",
            "the document declares no version",
        ),
        (
            "an error raised with no message at all",
            "silent-error-validator",
            "raise RuntimeError()",
            "RuntimeError",
            "",
        ),
    ];

    for (index, (case, name, raise, error_type, error_message)) in rows.into_iter().enumerate() {
        let function =
            given_function(&router, &engine, case, name, &validator_source(raise)).await;
        let profile = given_profile(
            &router,
            &application,
            &format!("relayed-{index}"),
            &[validator("LAMBDA", &function)],
        )
        .await;
        let refused_version =
            given_version(&router, &application, &profile, case, NON_COMPLIANT).await;
        let allowed_version = given_version(&router, &application, &profile, case, COMPLIANT).await;

        // The call the validator gates, refused on the function's own verdict.
        let refused_environment =
            create_environment(&router, &application, &format!("refused-{index}")).await;
        let refused = start_deployment(
            &router,
            &application,
            &refused_environment,
            &profile,
            &strategy,
            &refused_version,
        )
        .await;
        let payload = relayed_payload(case, &refused);
        assert_eq!(
            payload["errorType"].as_str(),
            Some(error_type),
            "case {case:?}: expected the error class the handler raised, got {payload}"
        );
        assert_eq!(
            payload["errorMessage"].as_str(),
            Some(error_message),
            "case {case:?}: expected the message the handler raised with, got {payload}"
        );
        assert!(
            payload["stackTrace"]
                .as_array()
                .is_some_and(|frames| !frames.is_empty()),
            "case {case:?}: the relayed document carries the handler's own stack, got {payload}"
        );
        let deployed = deployment_numbers(&router, &application, &refused_environment).await;
        assert!(
            deployed.is_empty(),
            "case {case:?}: a refused deployment leaves no record, got {deployed:?}"
        );

        // The same function, the same gate, a configuration it finds compliant.
        let allowed_environment =
            create_environment(&router, &application, &format!("allowed-{index}")).await;
        let accepted = start_deployment(
            &router,
            &application,
            &allowed_environment,
            &profile,
            &strategy,
            &allowed_version,
        )
        .await;
        assert_eq!(
            accepted.status,
            201,
            "case {case:?}: the validator found this content compliant, so the deployment \
             must proceed, got {} / {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        assert_eq!(
            deployment_numbers(&router, &application, &allowed_environment).await,
            vec![1],
            "case {case:?}: the deployment the validator let through belongs in the history"
        );

        // `ValidateConfiguration` reads the same seam, so it relays the same verdict — and
        // its own invocation, which is a second run of the function and so a second id.
        let reported =
            validate_configuration(&router, &application, &profile, &refused_version).await;
        let reported_payload = relayed_payload(case, &reported);
        assert_eq!(
            reported_payload["errorType"].as_str(),
            Some(error_type),
            "case {case:?}: ValidateConfiguration relays the same verdict, got \
             {reported_payload}"
        );
        assert_ne!(
            relayed_request_id(case, &payload),
            relayed_request_id(case, &reported_payload),
            "case {case:?}: each refusal relays the answer of its own invocation"
        );
        let compliant_report =
            validate_configuration(&router, &application, &profile, &allowed_version).await;
        assert_eq!(
            compliant_report.status,
            204,
            "case {case:?}: the validator reported this content compliant, so the modelled \
             204 is owed, got {} / {}",
            compliant_report.status,
            String::from_utf8_lossy(&compliant_report.body)
        );
    }
}
