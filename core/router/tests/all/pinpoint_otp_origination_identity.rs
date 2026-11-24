// SPDX-License-Identifier: CC0-1.0

//! A Pinpoint OTP send is answered from the origination identity the ACCOUNT holds, and
//! that identity is leased by another service.
//!
//! `SendOTPMessageRequestParameters$OriginationIdentity` is "the origination identity used
//! to send OTP from". Such an identity is not a Pinpoint resource: it is a number leased
//! to the account through the SMS and voice API (`RequestPhoneNumber`), so the only place
//! the two services meet is the assembled emulator, where they share one engine. This
//! suite drives that path over the wire: lease a number through SMS and voice, send an OTP
//! from it through Pinpoint, give the lease back, and send again.
//!
//! The rule it pins: an OTP send's per-destination verdict follows whether the request's
//! `OriginationIdentity` names an identity the calling account holds — reported for the
//! destination as `SUCCESSFUL`/200 when it does and as `PERMANENT_FAILURE`/400 with the
//! service's fixed origination-identity sentence when it does not — and the destination's
//! result carries a `MessageId` either way.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the
//! same reason `boot_serve.rs` does.

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;
use std::sync::Arc;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// A credential scope naming Pinpoint's signing name (`mobiletargeting`).
const PINPOINT_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/mobiletargeting/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// A credential scope naming the SMS and voice signing name (`sms-voice`).
const SMS_VOICE_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/sms-voice/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// The sentence Pinpoint reports for a destination whose send names an origination
/// identity the account cannot send from. Entirely fixed: no request or state value is
/// interpolated into it.
const ORIGINATION_IDENTITY_NOT_USABLE: &str = "The provided number does not exist or does \
     not belong to the account, or its resource policy does not allow Pinpoint to use it";

/// The destination every send in this suite addresses.
const DESTINATION: &str = "+12065550100";

#[test]
fn an_otp_send_follows_the_origination_identity_the_account_leases() {
    std::thread::Builder::new()
        .name("pinpoint-otp-origination".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                /// Dispatch one request and answer its status and parsed body.
                async fn call(
                    router: &smithy_server_runtime::MultiServiceRouter,
                    method: &str,
                    path: &str,
                    authorization: &str,
                    target: Option<&str>,
                    body: &str,
                ) -> (u16, Value) {
                    let mut builder = http::Request::builder()
                        .method(method)
                        .uri(format!("https://127.0.0.1:9297{path}"))
                        .header("host", "127.0.0.1:9297")
                        .header("authorization", authorization)
                        .header(
                            "content-type",
                            if target.is_some() {
                                "application/x-amz-json-1.0"
                            } else {
                                "application/json"
                            },
                        );
                    if let Some(target) = target {
                        builder = builder.header("x-amz-target", target);
                    }
                    let request = builder
                        .body(bytes::Bytes::from(body.as_bytes().to_vec()))
                        .expect("the request is well formed");
                    let response = router.dispatch(request).await;
                    let status = response.status().as_u16();
                    let bytes = response.into_body();
                    let parsed = if bytes.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
                    };
                    (status, parsed)
                }

                // The Pinpoint application the codes are sent for.
                let (status, body) = call(
                    &router,
                    "POST",
                    "/v1/apps",
                    PINPOINT_AUTHORIZATION,
                    None,
                    r#"{"Name":"otp-origination-identity"}"#,
                )
                .await;
                assert_eq!(status, 201, "CreateApp must answer 201, got {body:?}");
                let app = body["Id"]
                    .as_str()
                    .expect("CreateApp answers with the minted Id")
                    .to_string();

                // Before any number is leased, nothing in the account can send an OTP.
                let (status, body) = send_otp(&router, &app, "+15559990000", "before-lease").await;
                assert_eq!(status, 200, "SendOTPMessage must answer 200, got {body:?}");
                assert_refused(&body, "before any number is leased");

                // Lease one number through the service that owns the account's
                // origination identities.
                let (status, body) = call(
                    &router,
                    "POST",
                    "/",
                    SMS_VOICE_AUTHORIZATION,
                    Some("PinpointSMSVoiceV2.RequestPhoneNumber"),
                    r#"{"IsoCountryCode":"US","MessageType":"TRANSACTIONAL",
                        "NumberCapabilities":["SMS"],"NumberType":"LONG_CODE"}"#,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "RequestPhoneNumber must answer 200, got {body:?}"
                );
                let leased_number = body["PhoneNumber"]
                    .as_str()
                    .expect("RequestPhoneNumber answers with the leased number")
                    .to_string();
                let leased_id = body["PhoneNumberId"]
                    .as_str()
                    .expect("RequestPhoneNumber answers with the lease's id")
                    .to_string();
                let leased_arn = body["PhoneNumberArn"]
                    .as_str()
                    .expect("RequestPhoneNumber answers with the lease's ARN")
                    .to_string();

                // Every form that names the lease sends the code: the number, its id and
                // its ARN.
                for (case, identity) in [
                    ("the leased number", leased_number.as_str()),
                    ("the lease's id", leased_id.as_str()),
                    ("the lease's ARN", leased_arn.as_str()),
                ] {
                    let (status, body) = send_otp(&router, &app, identity, case).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: SendOTPMessage must answer 200, got {body:?}"
                    );
                    let outcome = &body["Result"][DESTINATION];
                    assert_eq!(
                        (
                            outcome["DeliveryStatus"].as_str(),
                            outcome["StatusCode"].as_i64()
                        ),
                        (Some("SUCCESSFUL"), Some(200)),
                        "case {case:?}: a send from an identity the account leases is accepted, got {outcome:?}"
                    );
                    assert!(
                        !outcome["MessageId"].as_str().unwrap_or_default().is_empty(),
                        "case {case:?}: the destination carries a MessageId, got {outcome:?}"
                    );
                }

                // A number this account never leased is still refused while that lease is
                // held, so the verdict is about the identity the request named.
                let (status, body) = send_otp(&router, &app, "+15559990000", "another number").await;
                assert_eq!(status, 200, "SendOTPMessage must answer 200, got {body:?}");
                assert_refused(&body, "a number the account never leased");

                // Give the lease back: the identity resolves to nothing again, so the
                // next send is refused — the verdict is read from the owning service's
                // inventory on every send, not cached by Pinpoint.
                let (status, body) = call(
                    &router,
                    "POST",
                    "/",
                    SMS_VOICE_AUTHORIZATION,
                    Some("PinpointSMSVoiceV2.ReleasePhoneNumber"),
                    &format!(r#"{{"PhoneNumberId":"{leased_id}"}}"#),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "ReleasePhoneNumber must answer 200, got {body:?}"
                );
                for (case, identity) in [
                    ("the released number", leased_number.as_str()),
                    ("the released lease's id", leased_id.as_str()),
                ] {
                    let (status, body) = send_otp(&router, &app, identity, case).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: SendOTPMessage must answer 200, got {body:?}"
                    );
                    assert_refused(&body, case);
                }
            });
        })
        .expect("spawn the OTP origination-identity thread")
        .join()
        .expect("the OTP origination-identity check must pass");
}

/// One `SendOTPMessage` from `identity`, under a reference naming the case.
async fn send_otp(
    router: &smithy_server_runtime::MultiServiceRouter,
    app: &str,
    identity: &str,
    case: &str,
) -> (u16, Value) {
    let body = serde_json::json!({
        "BrandName": "Example Corp",
        "Channel": "SMS",
        "DestinationIdentity": DESTINATION,
        "OriginationIdentity": identity,
        "ReferenceId": format!("otp-{case}"),
    })
    .to_string();
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://127.0.0.1:9297/v1/apps/{app}/otp"))
        .header("host", "127.0.0.1:9297")
        .header("authorization", PINPOINT_AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.into_bytes()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// What an OTP send reports for its destination when the account cannot send from the
/// identity the request named: the refusal of the number, carrying a `MessageId` all the
/// same.
fn assert_refused(body: &Value, case: &str) {
    let outcome = &body["Result"][DESTINATION];
    assert_eq!(
        (
            outcome["DeliveryStatus"].as_str(),
            outcome["StatusCode"].as_i64()
        ),
        (Some("PERMANENT_FAILURE"), Some(400)),
        "case {case:?}: an identity the account does not hold is refused for the destination, got {outcome:?}"
    );
    assert_eq!(
        outcome["StatusMessage"].as_str(),
        Some(ORIGINATION_IDENTITY_NOT_USABLE),
        "case {case:?}: the destination reports the origination-identity refusal, got {outcome:?}"
    );
    assert!(
        !outcome["MessageId"].as_str().unwrap_or_default().is_empty(),
        "case {case:?}: the destination carries a MessageId even when nothing was delivered, got {outcome:?}"
    );
}
