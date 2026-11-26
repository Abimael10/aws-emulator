// SPDX-License-Identifier: CC0-1.0

//! WHICH of a message's addresses a send's verification check weighs, end to end over the
//! awsQuery wire.
//!
//! A new Amazon SES account is in the SANDBOX, where it "can only send email to verified
//! identities", and every send of this surface states the other half itself: "the message
//! must be sent from a verified email address or domain." So one rule decides every send:
//!
//! * the identity the message is SENT FROM is weighed always, and so is the address bounces
//!   and complaints are forwarded to (`SendEmailRequest$ReturnPath`: "This email address
//!   must be either individually verified with Amazon SES, or from a domain that has been
//!   verified with Amazon SES");
//! * while the account is in the sandbox the recipients the message addresses are weighed
//!   too — To:, CC: and BCC: alike;
//! * a mailbox of the mailbox simulator belongs to no account, so it needs no identity and
//!   fails no check;
//! * an address the check rejects makes the send fail with `MessageRejected` (HTTP 400),
//!   whose message names the Region the check ran in and EVERY address that failed it, in
//!   the order the request states them;
//! * once the account has production access the recipients are weighed by nothing, since
//!   such an account may address a message to anybody, while its sending identity still is.
//!
//! A bulk send sends one message per `Destination`, so the recipients the sandbox weighs
//! are weighed per destination and reported as that destination's own `Status`, the way
//! every other condition of one destination is.
//!
//! The rows differ along what the rule generalises over: which recipient field carries the
//! address, how many addresses fail, whether an address is verified by its own identity or
//! by a domain above it, whether its verification has gone through, the form the address
//! arrives in, the mailbox simulator, which send operation carries the message, and the
//! account's production access.

use crate::harness::{
    call, element, emulator_and_engine, error_envelope, field, grant_production_access,
    settle_verifications, xml_text, Field, REGION,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// The domain identity every send below is sent from, which authorises every mailbox in it
/// and in its subdomains.
const SENDER_DOMAIN: &str = "sender.example.com";

/// A mailbox of that domain.
const SENDER: &str = "campaigns@sender.example.com";

/// A recipient the account verifies as an address identity of its own.
const VERIFIED_RECIPIENT: &str = "friend@partner.example.net";

/// The domain the account holds no identity for, where every address the check rejects
/// lives.
const STRANGER_DOMAIN: &str = "elsewhere.example.org";

/// The mailbox simulator's accepting mailbox, which belongs to no account.
const SIMULATOR_SUCCESS: &str = "success@simulator.amazonses.com";

/// Another of its mailboxes, so the rule is asserted over the simulator rather than over
/// one of its addresses.
const SIMULATOR_BOUNCE: &str = "bounce@simulator.amazonses.com";

/// The template the templated sends below name.
const TEMPLATE: &str = "receipt";

/// A mailbox of the domain no account holds an identity for.
fn stranger(local: &str) -> String {
    format!("{local}@{STRANGER_DOMAIN}")
}

/// The sentence a send whose addresses failed the verification check is refused with: the
/// Region the check ran in, upper-cased, then every address that failed it.
fn refusal(identities: &[&str]) -> String {
    format!(
        "Email address is not verified. The following identities failed the check in region \
         {}: {}",
        REGION.to_uppercase(),
        identities.join(", ")
    )
}

/// An emulator holding the verified sending domain, the verified recipient and the one
/// stored template, with every requested verification carried through.
async fn emulator_ready() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_and_engine();
    let setup: Vec<(&str, Vec<Field>)> = vec![
        ("VerifyDomainIdentity", vec![field("Domain", SENDER_DOMAIN)]),
        (
            "VerifyEmailIdentity",
            vec![field("EmailAddress", VERIFIED_RECIPIENT)],
        ),
        (
            "CreateTemplate",
            vec![
                field("Template.TemplateName", TEMPLATE),
                field("Template.SubjectPart", "Recipient check"),
                field("Template.TextPart", "this send is weighed on its addresses"),
            ],
        ),
    ];
    for (action, params) in setup {
        let (status, body) = call(&router, action, &params).await;
        assert_eq!(
            status, 200,
            "case setup {action}: expected 200, got {status} with {body}"
        );
    }
    // Asking for a verification only STARTS it, so the store is settled past the span the
    // clock owes those records before any send goes out.
    settle_verifications(&engine);
    (router, engine)
}

/// The recipient fields of a `Destination`, as the form fields that carry them.
fn destination(to: &[String], cc: &[String], bcc: &[String]) -> Vec<Field> {
    let mut params = Vec::new();
    for (name, addresses) in [("ToAddresses", to), ("CcAddresses", cc), ("BccAddresses", bcc)] {
        for (index, address) in addresses.iter().enumerate() {
            params.push(field(
                format!("Destination.{name}.member.{}", index + 1),
                address.clone(),
            ));
        }
    }
    params
}

/// A `SendEmail` request from `source`, with the recipient fields and `ReturnPath` a row
/// states.
fn send_email_params(
    source: &str,
    to: &[String],
    cc: &[String],
    bcc: &[String],
    return_path: Option<&str>,
) -> Vec<Field> {
    let mut params = vec![
        field("Source", source),
        field("Message.Subject.Data", "Recipient check"),
        field("Message.Body.Text.Data", "this send is weighed on its addresses"),
    ];
    params.extend(destination(to, cc, bcc));
    if let Some(return_path) = return_path {
        params.push(field("ReturnPath", return_path));
    }
    params
}

/// The `<Code>` and `<Message>` of a refusal, with the message's escaping undone.
fn refused(body: &str) -> (String, String) {
    let (code, message) = error_envelope(body);
    (code.unwrap_or_default(), message.unwrap_or_default())
}

/// `GetSendQuota`'s `SentLast24Hours`, which is derived from the send log: how a case reads
/// back whether a send reached it.
async fn sent_last_24_hours(router: &MultiServiceRouter) -> f64 {
    let (status, body) = call(router, "GetSendQuota", &[]).await;
    assert_eq!(
        status, 200,
        "case GetSendQuota: expected 200, got {status} with {body}"
    );
    let text = element(&body, "SentLast24Hours")
        .unwrap_or_else(|| panic!("case GetSendQuota: expected SentLast24Hours, got {body}"));
    text.parse::<f64>()
        .unwrap_or_else(|_| panic!("case GetSendQuota: expected a number, got {text:?}"))
}

/// One refused row: the addresses the message names, and the ones the refusal must name.
struct Refused {
    case: &'static str,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    return_path: Option<String>,
    named: Vec<String>,
}

#[tokio::test]
async fn a_destination_the_sandbox_account_has_not_verified_refuses_the_send() {
    let rows = vec![
        Refused {
            case: "an unverified To: address",
            to: vec![stranger("stranger")],
            cc: Vec::new(),
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("stranger")],
        },
        Refused {
            case: "an unverified CC: beside a verified To:",
            to: vec![VERIFIED_RECIPIENT.to_string()],
            cc: vec![stranger("carbon")],
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("carbon")],
        },
        Refused {
            case: "an unverified BCC: beside a verified To:",
            to: vec![VERIFIED_RECIPIENT.to_string()],
            cc: Vec::new(),
            bcc: vec![stranger("blind")],
            return_path: None,
            named: vec![stranger("blind")],
        },
        Refused {
            case: "two unverified addresses, one To: and one CC:",
            to: vec![stranger("first")],
            cc: vec![stranger("second")],
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("first"), stranger("second")],
        },
        Refused {
            case: "the same unverified mailbox in two fields, which is one identity",
            to: vec![stranger("twice")],
            cc: vec![stranger("twice")],
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("twice")],
        },
        Refused {
            case: "an unverified mailbox behind a display name",
            to: vec![format!("Stranger <{}>", stranger("wrapped"))],
            cc: Vec::new(),
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("wrapped")],
        },
        Refused {
            case: "an unverified ReturnPath, every recipient verified",
            to: vec![VERIFIED_RECIPIENT.to_string()],
            cc: Vec::new(),
            bcc: Vec::new(),
            return_path: Some(stranger("bounces")),
            named: vec![stranger("bounces")],
        },
        Refused {
            case: "the mailbox simulator beside an unverified recipient",
            to: vec![SIMULATOR_SUCCESS.to_string(), stranger("beside")],
            cc: Vec::new(),
            bcc: Vec::new(),
            return_path: None,
            named: vec![stranger("beside")],
        },
    ];

    for row in &rows {
        let (router, _engine) = emulator_ready().await;
        let (status, body) = call(
            &router,
            "SendEmail",
            &send_email_params(
                SENDER,
                &row.to,
                &row.cc,
                &row.bcc,
                row.return_path.as_deref(),
            ),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body}",
            row.case
        );
        let named: Vec<&str> = row.named.iter().map(String::as_str).collect();
        let expected = refusal(&named);
        assert_eq!(
            refused(&body),
            ("MessageRejected".to_string(), expected.clone()),
            "case {:?}: expected MessageRejected with {expected:?}, got {body}",
            row.case
        );
        assert_eq!(
            sent_last_24_hours(&router).await,
            0.0,
            "case {:?}: a refused send charges nothing and records nothing",
            row.case
        );
    }
}

#[tokio::test]
async fn every_send_operation_weighs_its_recipients_the_same_way() {
    // The rule is the SEND's, not one operation's: each form a send of this service takes
    // refuses the same unverified recipient in the same sentence.
    let unverified = stranger("whichever");
    let raw = ::aws_smithy_types::base64::encode(format!(
        "From: {SENDER}\r\nTo: {unverified}\r\nSubject: Recipient check\r\n\r\nBody.\r\n"
    ));
    let rows: Vec<(&str, &str, Vec<Field>)> = vec![
        (
            "SendEmail",
            "SendEmail",
            send_email_params(SENDER, std::slice::from_ref(&unverified), &[], &[], None),
        ),
        (
            "SendRawEmail naming its Destinations",
            "SendRawEmail",
            vec![
                field("RawMessage.Data", raw.clone()),
                field("Source", SENDER),
                field("Destinations.member.1", unverified.clone()),
            ],
        ),
        (
            "SendRawEmail reading the message's own To line",
            "SendRawEmail",
            vec![field("RawMessage.Data", raw), field("Source", SENDER)],
        ),
        (
            "SendTemplatedEmail",
            "SendTemplatedEmail",
            vec![
                field("Source", SENDER),
                field("Destination.ToAddresses.member.1", unverified.clone()),
                field("Template", TEMPLATE),
                field("TemplateData", "{}"),
            ],
        ),
    ];

    let expected = refusal(&[unverified.as_str()]);
    for (case, action, params) in rows {
        let (router, _engine) = emulator_ready().await;
        let (status, body) = call(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            refused(&body),
            ("MessageRejected".to_string(), expected.clone()),
            "case {case:?}: expected MessageRejected with {expected:?}, got {body}"
        );
    }
}

#[tokio::test]
async fn a_verified_destination_or_the_mailbox_simulator_is_served_in_the_sandbox() {
    // (case, To:, CC:) — every address is an identity this account verified, a mailbox a
    // verified domain identity covers, or the mailbox simulator's.
    let rows: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        (
            "an address identity of its own",
            vec![VERIFIED_RECIPIENT.to_string()],
            vec![],
        ),
        (
            "a mailbox the verified domain identity covers",
            vec![SENDER.to_string()],
            vec![],
        ),
        (
            "a mailbox in a subdomain of that domain",
            vec!["bot@mail.sender.example.com".to_string()],
            vec![],
        ),
        (
            "the simulator's accepting mailbox",
            vec![SIMULATOR_SUCCESS.to_string()],
            vec![],
        ),
        (
            "another mailbox of the simulator",
            vec![SIMULATOR_BOUNCE.to_string()],
            vec![],
        ),
        (
            "the simulator in any spelling of its domain",
            vec!["success@SIMULATOR.AmazonSES.com".to_string()],
            vec![],
        ),
        (
            "a verified address and the simulator together",
            vec![VERIFIED_RECIPIENT.to_string()],
            vec![SIMULATOR_SUCCESS.to_string()],
        ),
        (
            "a verified ReturnPath is no obstacle either",
            vec![VERIFIED_RECIPIENT.to_string()],
            vec![],
        ),
    ];

    let (router, _engine) = emulator_ready().await;
    let mut ids: Vec<String> = Vec::new();
    let mut charged = 0.0;
    for (case, to, cc) in &rows {
        let return_path = (case.contains("ReturnPath")).then_some(SENDER);
        let (status, body) = call(
            &router,
            "SendEmail",
            &send_email_params(SENDER, to, cc, &[], return_path),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let message_id = element(&body, "MessageId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a MessageId, got {body}"));
        assert!(
            !message_id.is_empty() && !ids.contains(&message_id),
            "case {case:?}: expected an id of its own, got {message_id:?} after {ids:?}"
        );
        ids.push(message_id);
        charged += (to.len() + cc.len()) as f64;
        assert_eq!(
            sent_last_24_hours(&router).await,
            charged,
            "case {case:?}: every recipient of a served send is charged against the quota"
        );
    }
}

#[tokio::test]
async fn an_identity_whose_verification_has_not_gone_through_is_no_destination_yet() {
    // The other side of the same reading: a recipient the account has only ASKED SES about
    // is not one it may send to, and once the verification has gone through it is.
    let (router, engine) = emulator_ready().await;
    let pending = "pending@partner.example.net";
    let (status, body) = call(
        &router,
        "VerifyEmailIdentity",
        &[field("EmailAddress", pending)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup VerifyEmailIdentity: expected 200, got {status} with {body}"
    );

    let request = send_email_params(SENDER, &[pending.to_string()], &[], &[], None);
    let (status, body) = call(&router, "SendEmail", &request).await;
    let expected = refusal(&[pending]);
    assert_eq!(
        status, 400,
        "case a recipient whose verification is pending: expected HTTP 400, got {status} with \
         {body}"
    );
    assert_eq!(
        refused(&body),
        ("MessageRejected".to_string(), expected.clone()),
        "case a recipient whose verification is pending: expected {expected:?}, got {body}"
    );

    // The verification the clock owes that record, materialised rather than slept through.
    settle_verifications(&engine);
    let (status, body) = call(&router, "SendEmail", &request).await;
    assert_eq!(
        status, 200,
        "case the same recipient once verified: expected HTTP 200, got {status} with {body}"
    );
    assert!(
        element(&body, "MessageId").is_some(),
        "case the same recipient once verified: expected a MessageId, got {body}"
    );
}

#[tokio::test]
async fn production_access_weighs_the_sending_identity_and_no_recipient() {
    // The rows the sandbox refuses are served once the account has production access,
    // because such an account may address a message to anybody.
    let served: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        (
            "an address no account verified",
            vec![stranger("stranger")],
            vec![],
        ),
        (
            "several unverified addresses at once",
            vec![stranger("first"), stranger("second")],
            vec![stranger("third")],
        ),
    ];

    let (router, engine) = emulator_ready().await;
    grant_production_access(&engine);
    for (case, to, cc) in &served {
        let (status, body) = call(
            &router,
            "SendEmail",
            &send_email_params(SENDER, to, cc, &[], None),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 once the account has production access, got \
             {status} with {body}"
        );
        assert!(
            element(&body, "MessageId").is_some(),
            "case {case:?}: expected a MessageId, got {body}"
        );
    }

    // The sending identity is still weighed, and it is the ONLY address the refusal names:
    // the recipients of this send were weighed by nothing.
    let stranger_sender = "nobody@unverified.example.org";
    let (status, body) = call(
        &router,
        "SendEmail",
        &send_email_params(stranger_sender, &[stranger("anyone")], &[], &[], None),
    )
    .await;
    let expected = refusal(&[stranger_sender]);
    assert_eq!(
        status, 400,
        "case an unverified sender with production access: expected HTTP 400, got {status} with \
         {body}"
    );
    assert_eq!(
        refused(&body),
        ("MessageRejected".to_string(), expected.clone()),
        "case an unverified sender with production access: expected {expected:?}, got {body}"
    );
}

#[tokio::test]
async fn a_bulk_destination_the_sandbox_account_may_not_send_to_is_the_only_one_refused() {
    // One message per `Destination`, so the gate is weighed per destination: the ones
    // addressing a verified identity or the simulator are sent, and the one addressing a
    // mailbox nobody verified carries the refusal as its own status.
    let (router, _engine) = emulator_ready().await;
    let unverified = stranger("bulk");
    let destinations: Vec<Vec<String>> = vec![
        vec![VERIFIED_RECIPIENT.to_string()],
        vec![unverified.clone()],
        vec![SIMULATOR_SUCCESS.to_string()],
        vec![VERIFIED_RECIPIENT.to_string(), unverified.clone()],
    ];
    let mut params = vec![
        field("Source", SENDER),
        field("Template", TEMPLATE),
        field("DefaultTemplateData", "{}"),
    ];
    for (index, recipients) in destinations.iter().enumerate() {
        for (position, address) in recipients.iter().enumerate() {
            params.push(field(
                format!(
                    "Destinations.member.{}.Destination.ToAddresses.member.{}",
                    index + 1,
                    position + 1
                ),
                address.clone(),
            ));
        }
    }
    let (status, body) = call(&router, "SendBulkTemplatedEmail", &params).await;
    assert_eq!(
        status, 200,
        "case a mixed bulk send: expected HTTP 200 with one status per destination, got {status} \
         with {body}"
    );

    let reported: Vec<(String, String, String)> = crate::harness::elements(&body, "member")
        .into_iter()
        .map(|entry| {
            (
                element(&entry, "Status").unwrap_or_default(),
                element(&entry, "MessageId").unwrap_or_default(),
                element(&entry, "Error")
                    .map(|raw| xml_text(&raw))
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reported.len(),
        destinations.len(),
        "case a mixed bulk send: expected one status per destination, got {reported:?}"
    );

    let expected_error = refusal(&[unverified.as_str()]);
    // (case, the status the destination must carry, the error it must carry)
    let expected: Vec<(&str, &str, String)> = vec![
        ("a verified recipient", "Success", String::new()),
        (
            "a recipient nobody verified",
            "MessageRejected",
            expected_error.clone(),
        ),
        ("the mailbox simulator", "Success", String::new()),
        (
            "a verified recipient beside an unverified one",
            "MessageRejected",
            expected_error,
        ),
    ];
    for (index, (case, status, error)) in expected.iter().enumerate() {
        let (reported_status, message_id, reported_error) = &reported[index];
        assert_eq!(
            reported_status, status,
            "case {case:?}: expected status {status}, got {reported:?}"
        );
        assert_eq!(
            reported_error, error,
            "case {case:?}: expected error {error:?}, got {reported:?}"
        );
        assert_eq!(
            message_id.is_empty(),
            status != &"Success",
            "case {case:?}: a sent destination reports the id it minted and a refused one none, \
             got {reported:?}"
        );
    }
    assert_eq!(
        sent_last_24_hours(&router).await,
        2.0,
        "case a mixed bulk send: only the sent destinations are charged against the quota"
    );
}
