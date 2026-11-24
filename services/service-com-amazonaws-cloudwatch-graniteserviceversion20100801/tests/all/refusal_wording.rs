// SPDX-License-Identifier: CC0-1.0

//! WHICH CLASS a refusal travels under and WHAT IT SAYS, for the two refusals of this service
//! whose condition something else in the model also describes: a dataset's KMS key, for which the
//! model declares three 400 faults, and a metric widget definition, which arrives on a request
//! member the model names.
//!
//! The rule asserted here: a refusal carries the error class AWS answers that condition under and
//! the sentence AWS words for that class — its fixed text as AWS writes it, with the key ARN, the
//! account ids and the document node taken from the request and this emulator's own identity. An
//! error shape's `doc` is prose ABOUT the shape, so no refusal carries it as a message and no
//! shape name this service does not answer names the class; likewise a document's own node is
//! named in the document's vocabulary rather than as a path through the request member it arrived
//! on.
//!
//! Both tables drive the service as a QUERY-COMPATIBLE client does, because that is the client
//! the class is stated twice for (`crate::service::query_compatible_errors`): the body names the
//! class's shape and the `x-amzn-query-error` header carries the code such a client branches on,
//! so a row asserts the class in both places at once and a refusal cannot be re-classed on the
//! way out without a row noticing.
//!
//! The rows span what each rule generalises over — for the key, every condition that withholds an
//! association (a key no KMS holds, a policy that grants this service nothing, a policy that
//! denies one of the operations outright, the wrong KIND of key, a key in a state that admits no
//! cryptographic operation, and a key ARN of another account), and for the definition, every way
//! a payload falls short of the widget structure's root (a document missing the member it
//! requires, whatever else it carries, and a payload that is no widget document at all). Each
//! table carries the positive control that the same call is SERVED when nothing withholds it.
//!
//! KMS is mounted beside CloudWatch and the key set up through KMS's own wire calls
//! (`crate::dataset_kms_key_preconditions`), so what each row is refused on is the key AWS KMS
//! really holds rather than anything this suite states.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::dataset_kms_key_preconditions::{
    documented_policy, given_key_with, reported_key, service_statement, DOCUMENTED_PERMISSIONS,
};
use crate::harness::{build_router, call, call_in_query_mode, error_code, error_message, Emulator};
use crate::sns_action_publish_grant::{emulator_with_kms, key_policy, kms};

/// The modelled 400 faults of `AssociateDatasetKmsKey` whose documentation describes the
/// conditions the rows below are refused on, each with the prose it documents.
///
/// Source: `smithy_query(query="shape",shape="KmsAccessDeniedException,KmsKeyDisabledException,
/// KmsKeyNotFoundException",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`,
/// with the HTML stripped and the indentation each paragraph is wrapped at folded to one space.
/// Neither half of a pair may reach the wire: the shape name is not the class these refusals
/// travel under, and the prose is not a message.
const MODELLED_KEY_FAULTS: [(&str, &str); 3] = [
    (
        "KmsAccessDeniedException",
        "The operation was denied because either the calling principal lacks the required Amazon \
         Web Services Key Management Service (Amazon Web Services KMS) permission on the key, or \
         the key policy does not grant Amazon CloudWatch the permissions it needs to use the key. \
         Verify that the caller has kms:Decrypt permission on the key, and that the key policy \
         grants the CloudWatch service principal the kms:DescribeKey, kms:GenerateDataKey, \
         kms:Encrypt, kms:Decrypt, and kms:ReEncrypt* permissions described in \
         AssociateDatasetKmsKey.",
    ),
    (
        "KmsKeyDisabledException",
        "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key \
         is disabled or pending deletion. Re-enable the key (or restore it, if it is pending \
         deletion) and retry the operation.",
    ),
    (
        "KmsKeyNotFoundException",
        "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key \
         could not be found. Verify that the key Amazon Resource Name (ARN) is correct, that the \
         key exists, and that it is in the same Amazon Web Services Region as the resource.",
    ),
];

/// The class a request this service will not accept is refused in: the shape its body names, and
/// the legacy code a query-compatible client branches on — `None` where the refusal belongs to no
/// class of this service's API, which is answered exactly as the layer that built it wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Class {
    /// The local shape name the body of the answer names.
    shape: &'static str,
    /// What the `x-amzn-query-error` header reports, when the class has a legacy code.
    query_code: Option<&'static str>,
}

/// The class every failure of the key's own dry run travels under: the generic access-denied
/// code, in the status the request's own class carries, which is a name of no API class of this
/// service and so carries no legacy code.
const DENIED: Class = Class {
    shape: "AccessDenied",
    query_code: None,
};

/// The class a request whose VALUES this service will not accept travels under: the
/// request-validation shape in the body, and `ValidationError` for a query-compatible client.
const REQUEST_VALIDATION: Class = Class {
    shape: "ValidationException",
    query_code: Some("ValidationError;Sender"),
};

/// An ARN of the form `AssociateDatasetKmsKeyInput$KmsKeyArn` admits, in `account`.
fn key_arn_in(account: &str) -> String {
    format!("arn:aws:kms:us-east-1:{account}:key/4d3c2b1a-6f5e-4b7a-8d9c-1f0e3a2b5c4d")
}

/// The account this emulator serves, which every caller of it speaks for.
fn caller_account() -> String {
    ::emulator_server::emulator_account_id().to_string()
}

/// What a row names as the key of the association.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// A key whose policy carries only the statement a new key is minted with.
    UnpermissionedKey,
    /// A key whose policy grants the documented permissions and denies one of them outright.
    PartlyDeniedKey,
    /// A permissioned key that is not the kind data can be sealed under.
    AsymmetricKey,
    /// A permissioned key KMS holds in a state that admits no cryptographic operation.
    DisabledKey,
    /// An ARN of the caller's own account that KMS holds no key for.
    UnheldArn,
    /// An ARN whose account segment names the account given, which is not the caller's.
    ArnOfAccount(&'static str),
    /// A key whose policy grants exactly what AWS documents for a dataset's key.
    PermissionedKey,
}

/// Set `named` up in AWS KMS where it is a key, and answer the ARN the association names.
async fn given(emulator: &Emulator, case: &str, named: Named) -> String {
    match named {
        Named::UnpermissionedKey => {
            given_key_with(
                emulator,
                case,
                &json!({"Version": "2012-10-17", "Statement": [
                    crate::sns_action_publish_grant::account_root_statement(),
                ]})
                .to_string(),
                None,
            )
            .await
        }
        Named::PartlyDeniedKey => {
            let document = key_policy(vec![
                service_statement("Allow", json!(DOCUMENTED_PERMISSIONS), json!({})),
                service_statement("Deny", json!("kms:GenerateDataKey"), json!({})),
            ]);
            given_key_with(emulator, case, &document, None).await
        }
        Named::AsymmetricKey => {
            given_key_with(
                emulator,
                case,
                &documented_policy(),
                Some(("RSA_2048", "ENCRYPT_DECRYPT")),
            )
            .await
        }
        Named::DisabledKey => {
            let arn = given_key_with(emulator, case, &documented_policy(), None).await;
            kms(emulator, case, "DisableKey", &json!({"KeyId": arn.clone()})).await;
            arn
        }
        Named::UnheldArn => key_arn_in(&caller_account()),
        Named::ArnOfAccount(account) => key_arn_in(account),
        Named::PermissionedKey => {
            given_key_with(emulator, case, &documented_policy(), None).await
        }
    }
}

/// What the association must answer for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Nothing withholds the association, so it is written and read back: the control.
    Associated,
    /// The dry run did not validate the key, whichever of its checks found that.
    NotValidated,
    /// KMS holds the key in a state that admits no cryptographic operation.
    Disabled,
    /// The ARN names a key of another account, which is a judgement of the request's values.
    AccountMismatch,
}

impl Answer {
    /// The class this answer's refusal travels under.
    fn class(self) -> Class {
        match self {
            Self::Associated => unreachable!("the control is not a refusal"),
            Self::NotValidated | Self::Disabled => DENIED,
            Self::AccountMismatch => REQUEST_VALIDATION,
        }
    }

    /// The whole sentence it states about `key_arn`, for a caller speaking for `caller`.
    fn message(self, key_arn: &str, caller: &str) -> String {
        match self {
            Self::Associated => unreachable!("the control is not a refusal"),
            Self::NotValidated => format!(
                "Unable to validate KMS key {key_arn}. Verify the key exists and the key policy \
                 grants kms:DescribeKey to the CloudWatch service principal."
            ),
            Self::Disabled => format!("KMS key {key_arn} is disabled."),
            Self::AccountMismatch => {
                let key_account = key_arn.split(':').nth(4).unwrap_or_default();
                format!("KMS key account {key_account} does not match caller account {caller}")
            }
        }
    }
}

/// The legacy query code reported for an answer, or the empty string when it reported none.
fn reported_query_code(headers: &http::HeaderMap) -> String {
    headers
        .get("x-amzn-query-error")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Nothing a refusal of this service says may be read off one of the modelled faults above.
fn assert_not_the_models_prose(case: &str, code: &str, message: &str) {
    for (shape, prose) in MODELLED_KEY_FAULTS {
        assert_ne!(
            code, shape,
            "case {case:?}: {shape} is not the class this service answers here, got {code:?}"
        );
        assert_ne!(
            message, prose,
            "case {case:?}: the message must be the sentence AWS words, not {shape}'s \
             documentation, got {message:?}"
        );
    }
}

/// Every condition that withholds a dataset's KMS key is refused in the class AWS answers it
/// under, with the sentence that names what the caller has to repair — and never with a modelled
/// fault's name or documentation. A refused association writes nothing.
#[tokio::test]
async fn a_withheld_dataset_key_is_refused_in_aws_own_class_and_wording() {
    // (the case, what the association names, what it must answer)
    let rows: [(&str, Named, Answer); 8] = [
        (
            "an ARN of the caller's own account KMS holds no key for",
            Named::UnheldArn,
            Answer::NotValidated,
        ),
        (
            "a key whose policy grants this service nothing",
            Named::UnpermissionedKey,
            Answer::NotValidated,
        ),
        (
            "a key whose policy denies one documented operation outright",
            Named::PartlyDeniedKey,
            Answer::NotValidated,
        ),
        (
            "a permissioned key of a kind data cannot be sealed under",
            Named::AsymmetricKey,
            Answer::NotValidated,
        ),
        (
            "a permissioned key that has been disabled",
            Named::DisabledKey,
            Answer::Disabled,
        ),
        (
            "an ARN naming the account AWS's own examples use",
            Named::ArnOfAccount("123456789012"),
            Answer::AccountMismatch,
        ),
        (
            "an ARN naming an account numerically above the caller's",
            Named::ArnOfAccount("999999999999"),
            Answer::AccountMismatch,
        ),
        (
            "a key whose policy grants what AWS documents",
            Named::PermissionedKey,
            Answer::Associated,
        ),
    ];

    let caller = caller_account();
    for (case, named, expected) in rows {
        // One emulator per row, so the dataset each row reads back carries only its own doing.
        let emulator = emulator_with_kms();
        let key_arn = given(&emulator, case, named).await;
        let request = json!({"DatasetIdentifier": "default", "KmsKeyArn": key_arn.clone()});

        if expected == Answer::Associated {
            let (status, answered) =
                call(&emulator.router, "AssociateDatasetKmsKey", &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the association to be accepted, got {status} with \
                 {answered}"
            );
            assert_eq!(
                reported_key(&emulator, case).await,
                Some(key_arn.clone()),
                "case {case:?}: the dataset must report the key it was associated with"
            );
            continue;
        }

        let (status, headers, answered) =
            call_in_query_mode(&emulator.router, "AssociateDatasetKmsKey", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        let class = expected.class();
        let code = error_code(&answered);
        assert_eq!(
            code, class.shape,
            "case {case:?}: expected the body to name the {:?} class, got {answered}",
            class.shape
        );
        let reported = reported_query_code(&headers);
        assert_eq!(
            reported,
            class.query_code.unwrap_or_default(),
            "case {case:?}: expected the legacy query code {:?}, got {reported:?}",
            class.query_code.unwrap_or_default()
        );
        let message = error_message(&answered);
        assert_eq!(
            message,
            expected.message(&key_arn, &caller),
            "case {case:?}: expected the {expected:?} sentence, got {message:?}"
        );
        assert_not_the_models_prose(case, &code, &message);
        assert_eq!(
            reported_key(&emulator, case).await,
            None,
            "case {case:?}: a refused association must leave the dataset with no key"
        );
    }
}

/// The sentence a definition missing what the widget structure requires is refused with, and the
/// one a payload that is no widget document at all carries.
const SHOULD_HAVE_METRICS: &str = "Should have required property 'metrics'";
const BAD_JSON_CONTENT: &str = "MetricWidget property 'metricWidget' has a bad JSON content";

/// The first eight bytes of every PNG (the signature in the PNG specification), for the control.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// A widget definition that falls short of the structure's root is refused in the DOCUMENT's own
/// vocabulary — the condition the document failed and the property it needs — in the
/// request-validation class, and never as a path through the request member it arrived on.
#[tokio::test]
async fn a_widget_definition_is_refused_in_the_documents_own_vocabulary() {
    let router = build_router();

    // (the case, the payload the request carries, the sentence the refusal must state)
    let rows: [(&str, &str, &str); 7] = [
        ("a document with no members at all", "{}", SHOULD_HAVE_METRICS),
        (
            "a document carrying only the graph's size",
            r#"{"width": 400, "height": 300}"#,
            SHOULD_HAVE_METRICS,
        ),
        (
            "a document carrying every member but the metrics",
            r#"{"title": "cpu", "view": "singleValue", "period": 300, "stat": "Sum"}"#,
            SHOULD_HAVE_METRICS,
        ),
        (
            "a document naming the metrics member as null",
            r#"{"metrics": null}"#,
            SHOULD_HAVE_METRICS,
        ),
        (
            "a payload that is no JSON at all",
            "{this is not valid json",
            BAD_JSON_CONTENT,
        ),
        ("a payload that is a JSON array", "[]", BAD_JSON_CONTENT),
        ("a payload that is a JSON number", "12", BAD_JSON_CONTENT),
    ];

    for (case, widget, expected) in rows {
        let (status, headers, answered) = call_in_query_mode(
            &router,
            "GetMetricWidgetImage",
            &json!({"MetricWidget": widget}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            REQUEST_VALIDATION.shape,
            "case {case:?}: expected the body to name the request-validation class, got {answered}"
        );
        let reported = reported_query_code(&headers);
        assert_eq!(
            reported,
            REQUEST_VALIDATION.query_code.unwrap_or_default(),
            "case {case:?}: expected the legacy query code of the request-validation class, got \
             {reported:?}"
        );
        let message = error_message(&answered);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
        // The widget definition is a DOCUMENT: its nodes are named by the structure they belong
        // to, never as a dotted path through the request member the document arrived on.
        assert!(
            !message.contains("MetricWidget."),
            "case {case:?}: the diagnostic must name the document's own node, got {message:?}"
        );
    }
}

/// The control: a definition that carries what the structure requires is RENDERED, so the rows
/// above are refused on the document they carry rather than on the operation being closed.
#[tokio::test]
async fn a_definition_carrying_what_the_structure_requires_is_rendered() {
    let router = build_router();
    let case = "a definition naming one metric";
    let widget = json!({
        "metrics": [["AWS/EC2", "CPUUtilization"]],
        "width": 400,
        "height": 300,
    })
    .to_string();
    let (status, answered) = call(
        &router,
        "GetMetricWidgetImage",
        &json!({"MetricWidget": widget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the definition to be rendered, got {status} with {answered}"
    );
    let encoded = answered
        .get("MetricWidgetImage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let image = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: MetricWidgetImage must be base64-encoded, got {error}")
        });
    assert_eq!(
        image.get(..PNG_MAGIC.len()),
        Some(&PNG_MAGIC[..]),
        "case {case:?}: expected an image opening with the PNG signature, got {:?}",
        image.get(..PNG_MAGIC.len())
    );
}
