// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function can MAIL with: the AWS SDK its managed runtime supplies
//! builds a client for the classic (2010-12-01) Amazon SES API and the send it makes with that
//! client is served by that service itself.
//!
//! RULE: a managed runtime's bundled AWS SDK answers to EVERY service token the runtime's call
//! seam binds a request for, so a handler that builds a client for one at module scope imports
//! and runs, and the call it then makes is decided and served by the named service — the
//! operation's own output members on a success, that service's OWN error code raised as a
//! client error on a refusal — never a client-side report that the service is unknown. A token
//! the catalog omits fails the handler at client construction, before credentials, a policy or
//! an endpoint is consulted, so every downstream signal of the work that handler exists to do
//! (what it stored, the message id it was issued, the structured line it logs) is absent with
//! nothing to say why.
//!
//! The classic email API is this rule's hardest case and the one almost every existing
//! mail-sending handler is written against: its model declares `awsQuery` (its requests carry
//! their input as the parameters of a form-encoded `Action` and answer XML), it signs under the
//! name `ses` while its endpoint prefix is `email`, and it shares both with three other emulated
//! services — so a runtime that merely LISTS the token without binding its protocol is caught
//! here too.
//!
//! Six crates have to agree for the rule to hold — CloudFormation archives the inline source
//! and provisions the function, IAM holds the execution role the handler's own calls are signed
//! as, Lambda runs the handler and supplies it the SDK, Amazon S3 holds the report it stored,
//! Amazon SES decides the send and issues the message id, and Amazon CloudWatch Logs holds the
//! line the handler then wrote — so the suite lives here: a crate-local suite only ever sees one
//! side of it. The Lambda crate's own
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/handler_ses_sdk.rs` drives
//! the binding classes of this one service's operations across that seam; what only this suite
//! reaches is the WHOLE scheduled pipeline, where the stored object, the issued message id and
//! the logged line are read back from the three services that own them.
//!
//! The rows differ along what the rule generalises over: the BINDING FORM of the send the
//! handler makes (a structured message with a text body, a structured message with an HTML body
//! addressed to several recipients across `To` and `Cc`, and a send carrying the message's own
//! raw bytes) and whether Amazon SES SERVES or REFUSES it (a sender identity the account
//! verified against one it never did). Each row asserts all three sides: what the handler read
//! back, what the bucket holds, and what the log group holds.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_lambda_function_log_destination::{
    dispatch, emulator, given_stack_function, lambda, logs_policy, messages_until_condition,
    trust_policy, DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT, REGION,
};
use crate::query_wire::{form_encode, opt_element};

/// The bucket every row's report is stored in.
const BUCKET: &str = "xsvc-scheduled-report-ses";

/// The stack that provisions the execution role and the reporting function.
const STACK: &str = "xsvc-scheduled-report-ses-stack";

/// The logical ids that stack declares.
const ROLE_ID: &str = "ReporterRole";
const FUNCTION_ID: &str = "Reporter";

/// The managed runtime the function is created with, and the module and attribute
/// CloudFormation archives its inline source under.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The API version every `awsQuery` request of the classic email API carries
/// (`smithy_query(query="service_info")` on that service's model: version 2010-12-01).
const SES_API_VERSION: &str = "2010-12-01";

/// The identity the account verifies, which the served rows send from.
const SENDER: &str = "reports@example.com";

/// An identity no call here ever verifies, which the refused row sends from.
const STRANGER: &str = "stranger@example.net";

/// The recipients the rows address.
const RECIPIENT: &str = "owner@example.org";
const SECOND_RECIPIENT: &str = "auditor@example.org";
const COPIED: &str = "archive@example.org";

/// The state the classic plane reports for an identity the account accepts as a sender
/// (`IdentityVerificationAttributes$VerificationStatus`, whose value set is `Pending`,
/// `Success`, `Failed`, `TemporaryFailure`, `NotStarted`).
const VERIFIED: &str = "Success";

/// The error code Amazon SES refuses a send whose sender it does not accept with — one of
/// `SendEmail`'s own modelled refusals, and the one its documentation gives for a send "from an
/// address or domain that is not verified".
const MESSAGE_REJECTED: &str = "MessageRejected";

/// The two `event` values the handler reports, which are what a reader of its records branches
/// on.
const SENT: &str = "report_sent";
const FAILED: &str = "report_failed";

/// How often this suite looks again at something another service owes, and how many looks it
/// makes. The budget is far longer than any span these services choose, so a loaded host is
/// slower without the rule being any different; nothing here asserts on elapsed time.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 2400;

/// The handler every row runs.
///
/// It builds both clients AT MODULE SCOPE — which is where a runtime whose SDK does not answer
/// to a token fails the import outright — stores the report, mails it, and reports what the
/// send answered or the refusal it raised, printing that record so a reader of the function's
/// log group sees the same thing its caller does.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
from botocore.exceptions import ClientError

S3 = boto3.client("s3")
SES = boto3.client("ses")


def handler(event, context):
    key = "reports/" + event["id"] + ".json"
    body = json.dumps({"reportId": event["id"], "form": event["form"]}).encode("utf-8")
    S3.put_object(Bucket=event["bucket"], Key=key, Body=body)
    record = {"reportKey": key, "bytes": len(body), "form": event["form"]}
    try:
        if event["form"] == "raw":
            raw = (
                "From: " + event["sender"] + "\r\n"
                + "To: " + ", ".join(event["to"]) + "\r\n"
                + "Subject: " + event["subject"] + "\r\n"
                + "\r\n"
                + event["content"]
            ).encode("utf-8")
            answered = SES.send_raw_email(
                Source=event["sender"],
                Destinations=event["to"],
                RawMessage={"Data": raw},
            )
        else:
            destination = {"ToAddresses": event["to"]}
            if event["cc"]:
                destination["CcAddresses"] = event["cc"]
            part = "Html" if event["form"] == "html" else "Text"
            answered = SES.send_email(
                Source=event["sender"],
                Destination=destination,
                Message={
                    "Subject": {"Data": event["subject"]},
                    "Body": {part: {"Data": event["content"]}},
                },
            )
    except ClientError as refused:
        record["event"] = "report_failed"
        record["stage"] = "ses_send_failed"
        record["errorCode"] = refused.response["Error"]["Code"]
        record["detail"] = refused.response["Error"]["Message"]
        print(json.dumps(record))
        return record
    record["event"] = "report_sent"
    record["sesMessageId"] = answered["MessageId"]
    print(json.dumps(record))
    return record
"#;

/// One row: the send the handler makes, and whether Amazon SES serves it.
struct Row {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The handler's branch, which decides the binding form of the send.
    form: &'static str,
    /// The `Source` the send is made from.
    sender: &'static str,
    /// The envelope's `To` addresses.
    to: &'static [&'static str],
    /// The envelope's `Cc` addresses, where the form carries any.
    cc: &'static [&'static str],
    /// The body the message carries, in the part the form names.
    content: &'static str,
    /// Whether Amazon SES serves this send, so the handler is issued a message id.
    served: bool,
}

/// The rows, spanning the send's binding form and both outcomes.
const ROWS: &[Row] = &[
    Row {
        case: "a structured send with a text body, from a verified identity",
        form: "text",
        sender: SENDER,
        to: &[RECIPIENT],
        cc: &[],
        content: "one scheduled report, sent by a handler",
        served: true,
    },
    Row {
        case: "a structured send with an html body, across To and Cc",
        form: "html",
        sender: SENDER,
        to: &[RECIPIENT, SECOND_RECIPIENT],
        cc: &[COPIED],
        content: "<p>one scheduled report, sent by a handler</p>",
        served: true,
    },
    Row {
        case: "a send carrying the message's own raw bytes",
        form: "raw",
        sender: SENDER,
        to: &[RECIPIENT],
        cc: &[],
        content: "one scheduled report, as raw bytes",
        served: true,
    },
    Row {
        case: "a send from an identity the account never verified",
        form: "text",
        sender: STRANGER,
        to: &[RECIPIENT],
        cc: &[],
        content: "one scheduled report, from a stranger",
        served: false,
    },
];

/// One classic-email-API call, which must be served; answers its XML body.
async fn ses(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    fields: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={SES_API_VERSION}");
    for (name, value) in fields {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "ses",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from {action}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One Amazon S3 call on the path-style address, as the account's own credential.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Vec<u8>) {
    let answered = dispatch(
        router,
        "s3",
        method,
        path,
        &[("content-type", "application/octet-stream")],
        Bytes::new(),
    )
    .await;
    (answered.status, answered.body.to_vec())
}

/// What the role may do: the basic-execution log grant the crate states once, plus the write
/// this handler makes to the report bucket's objects and the two sends it may make.
fn role_policy() -> Value {
    let mut statements = logs_policy()["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    statements.push(json!({
        "Effect": "Allow",
        "Action": "s3:PutObject",
        // Amazon S3's own ARN form for the objects of one bucket, which carries neither a
        // Region nor an account.
        "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
    }));
    statements.push(json!({
        "Effect": "Allow",
        // The two actions a mail-sending handler is granted: one per send operation it calls.
        "Action": ["ses:SendEmail", "ses:SendRawEmail"],
        "Resource": "*",
    }));
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The stack the suite deploys: the execution role, and the reporting function pointed at it.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "scheduled-report",
                        "PolicyDocument": role_policy()
                    }]
                }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// Record `identity` with the classic plane and look until the verification the clock owes it
/// has fallen due, so a send from it is one the account accepts.
async fn given_verified_identity(router: &MultiServiceRouter, case: &str, identity: &str) {
    ses(
        router,
        case,
        "VerifyEmailIdentity",
        &[("EmailAddress", identity.to_string())],
    )
    .await;
    let mut last = String::new();
    for _ in 0..LOOKS {
        last = ses(
            router,
            case,
            "GetIdentityVerificationAttributes",
            &[("Identities.member.1", identity.to_string())],
        )
        .await;
        if opt_element(&last, "VerificationStatus").as_deref() == Some(VERIFIED) {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: {identity:?} never reached {VERIFIED}, last read {last}");
}

/// The same for a DOMAIN, which authorises every mailbox in it: what a SANDBOX account
/// needs before it may send TO one of those mailboxes, since such an account "can only send
/// email to verified identities".
async fn given_verified_domain(router: &MultiServiceRouter, case: &str, domain: &str) {
    ses(
        router,
        case,
        "VerifyDomainIdentity",
        &[("Domain", domain.to_string())],
    )
    .await;
    let mut last = String::new();
    for _ in 0..LOOKS {
        last = ses(
            router,
            case,
            "GetIdentityVerificationAttributes",
            &[("Identities.member.1", domain.to_string())],
        )
        .await;
        if opt_element(&last, "VerificationStatus").as_deref() == Some(VERIFIED) {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: {domain:?} never reached {VERIFIED}, last read {last}");
}

/// How many messages the account has sent in the last day, as its own figures report it
/// (`GetSendQuota$SentLast24Hours`).
async fn sent_last_24_hours(router: &MultiServiceRouter, case: &str) -> f64 {
    let quota = ses(router, case, "GetSendQuota", &[]).await;
    opt_element(&quota, "SentLast24Hours")
        .and_then(|sent| sent.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: GetSendQuota reported no figures, got {quota}"))
}

#[tokio::test]
async fn a_handlers_own_send_through_the_classic_ses_api_is_served_end_to_end() {
    let router = emulator();
    let setup = "the account the report pipeline runs in";

    // The bucket the reports go to, and the sender the account accepts.
    let (status, created) = s3(&router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        status,
        200,
        "case {setup:?}: expected the report bucket to be created, got {status} with {:?}",
        String::from_utf8_lossy(&created)
    );
    given_verified_identity(&router, setup, SENDER).await;
    // The account is in the sandbox, where it may only send to identities it has verified,
    // so the domain every row addresses is verified as well: what these rows are about is
    // the handler's own send and what it reports of it.
    given_verified_domain(&router, setup, "example.org").await;

    let before = sent_last_24_hours(&router, setup).await;
    let function = given_stack_function(&router, setup, STACK, &template()).await;
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let id = format!("report-{index}");
        let subject = format!("the scheduled report {id}");

        let invoked = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            Some(json!({
                "id": id,
                "bucket": BUCKET,
                "form": row.form,
                "sender": row.sender,
                "to": row.to,
                "cc": row.cc,
                "subject": subject,
                "content": row.content,
            })),
        )
        .await;
        assert_eq!(
            invoked.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} with {:?}",
            invoked.status,
            invoked.text()
        );
        // A module whose client could not be built never imports, which is what AWS Lambda
        // reports as a function error — so a catalog missing the token fails right here.
        assert_eq!(
            invoked.function_error, None,
            "case {case:?}: expected the handler to import and run, got FunctionError {:?} with \
             {:?}",
            invoked.function_error,
            invoked.text()
        );
        let record = invoked.json();

        // 1. What the handler read back from Amazon SES.
        let expected_event = if row.served { SENT } else { FAILED };
        assert_eq!(
            record["event"].as_str(),
            Some(expected_event),
            "case {case:?}: expected the handler to report {expected_event:?}, got {record}"
        );
        if row.served {
            assert!(
                record["sesMessageId"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()),
                "case {case:?}: expected the service's own message id, got {record}"
            );
        } else {
            assert_eq!(
                record["errorCode"].as_str(),
                Some(MESSAGE_REJECTED),
                "case {case:?}: expected the service's own refusal code, got {record}"
            );
            assert!(
                record["detail"].as_str().is_some_and(|text| !text.is_empty()),
                "case {case:?}: expected the refusal to carry the service's own message, got \
                 {record}"
            );
            // And that message as Amazon SES words this refusal: fixed text, with the Region
            // the check ran in (upper cased, because an identity is verified per Region — the
            // Region the shared harness signs and addresses every request for) and the
            // identities that failed it taken from the request.
            let refusal = format!(
                "Email address is not verified. The following identities failed the check in \
                 region {}: {}",
                REGION.to_uppercase(),
                row.sender
            );
            assert_eq!(
                record["detail"].as_str(),
                Some(refusal.as_str()),
                "case {case:?}: expected the refusal to be worded {refusal:?}, got {record}"
            );
        }

        // 2. What the bucket holds: the report really was stored, on the refused row too — the
        //    send is decided after the handler has done its own work, not before it ran.
        let key = record["reportKey"].as_str().unwrap_or_default();
        assert!(
            !key.is_empty(),
            "case {case:?}: expected the handler to report the key it stored, got {record}"
        );
        let (status, stored) = s3(&router, "GET", &format!("/{BUCKET}/{key}")).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected the stored report at {key:?}, got {status} with {:?}",
            String::from_utf8_lossy(&stored)
        );
        assert_eq!(
            u64::try_from(stored.len()).ok(),
            record["bytes"].as_u64(),
            "case {case:?}: expected the bucket to hold the {} bytes the handler reported, got \
             {}",
            record["bytes"],
            stored.len()
        );
        let held: Value = serde_json::from_slice(&stored).unwrap_or(Value::Null);
        assert_eq!(
            held["reportId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the stored report to be this run's, got {held}"
        );

        // 3. What the function's log group holds: the structured line a metric filter over this
        //    pipeline reads, carrying the same verdict the caller was handed.
        let marker = format!("\"event\": \"{expected_event}\"");
        let messages = messages_until_condition(&router, &group, |held| {
            held.iter()
                .any(|line| line.contains(key) && line.contains(&marker))
        })
        .await;
        assert!(
            messages
                .iter()
                .any(|line| line.contains(key) && line.contains(&marker)),
            "case {case:?}: expected a {expected_event:?} line for {key:?} in {group:?}, got \
             {messages:?}"
        );
    }

    // And the account's OWN figures count the sends it served, so the message ids the handler
    // read back were issued by this service rather than made up for it. Exactly the RECIPIENTS
    // of the served rows: "for every message that you send, the total number of recipients
    // (including each recipient in the To:, CC: and BCC: fields) is counted against the maximum
    // number of emails you can send in a 24-hour period" (`SendEmail`), and the emulator is
    // built fresh here — so a refused send that was counted, and a served one that was not,
    // both fail here.
    let served = ROWS
        .iter()
        .filter(|row| row.served)
        .map(|row| row.to.len() + row.cc.len())
        .sum::<usize>() as f64;
    let after = sent_last_24_hours(&router, "the account's own send figures").await;
    assert!(
        (after - before - served).abs() < 0.5,
        "case the account's own send figures: expected exactly {served} more recipients sent to \
         than the {before} before the runs, got {after}"
    );
}
