Issue-Source: trace-sweep-divergence
Issue-Trace: sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sqs|ReceiveMessage|2xx->2xx|$.Messages[*].Body
Issue-Commit: f3ce2fa378d1283dd48aeebb96e6d14cd3cac708

# sqs: the SNS notification delivered into a subscribed queue announces a SigningCertURL host the receiving client is not reached on

> _Note: this issue was identified against the code base at commit `f3ce2fa378d1283dd48aeebb96e6d14cd3cac708`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ReceiveMessage

## Description
ONE net-new root cause, found by replaying the wire fixture corpus (simparity-wire-aws-v1) against the emulator built from this checkout: 44 fixtures, 370 steps, 363 passed, 6 mismatched, score 98.1%, emulator_up=true, bootstrap_signal=false. The cluster carries an EMPTY stub_path and an EMPTY owned_by. Its `service` field is `sqs` and the diverging call is SQS ReceiveMessage, because the divergence is observed in the message BODY a subscribed queue hands back; the component at fault is the SNS notification-envelope signing members that produced that body.

=== Cause 1 of 1: the JSON notification envelope's SigningCertURL is built from the authority this process was reached at, while the SAME envelope's UnsubscribeURL is built from the service's regional endpoint — so one document names two hosts, and the certificate a subscriber must fetch is announced at an address the receiving client is not reached on ===
Fixture/step: sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found#task#9.
AWS: recorded behavior only — the SNS JSON notification envelope is not a Smithy shape (ReceiveMessage simply returns it as Messages[0].Body); the Developer Guide section "Verifying the signatures of Amazon SNS messages" documents `SigningCertURL` as "The URL of the certificate that was used to sign the message" and tells a subscriber to FETCH it and extract the public key. What the recording shows: the envelope delivered into a subscribed SQS queue carries Type/MessageId/TopicArn/Message/Timestamp/SignatureVersion/Signature/SigningCertURL/UnsubscribeURL, and the two URL members name the SAME host — the SNS endpoint of the message's Region, `sns.<region>.amazonaws.com` — with SigningCertURL's path being `/SimpleNotificationService-<32 hex characters>.pem`. The specific host name, the certificate id, the Timestamp and the Signature bytes are per-environment/per-run values; the requirement this step records is that ONE envelope announces ONE host for both URL members, and that it is the host the envelope's recipient can reach the service at.
Emulator: in the very same body, UnsubscribeURL still reads `https://sns.<region>.amazonaws.com/?Action=Unsubscribe&SubscriptionArn=...` (it matches the recording), but SigningCertURL reads `https://<loopback-authority>/SimpleNotificationService-<id>.pem` — the authority this request happened to reach the SNS process at, which is neither the regional endpoint the sibling member names nor the data listener the corpus is replayed against (the replay harness classifies the difference `host_not_own`). Code: state/notification_signature.rs `certificate_url(ctx, region)` takes `data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())`, falling back to `served_endpoint_authority(ctx.endpoint())`, and only `unwrap_or_else(|| format!("sns.{region}.amazonaws.com"))` as a last resort; `Signer::of_topic` builds it per request. The Unsubscribe/Subscribe URL members of the same envelope are built from the regional endpoint form instead, so the two addressing rules disagree inside one document.
Why: a subscriber that follows the documented verification procedure reads SigningCertURL out of the message it received and fetches it. The address it is handed is the one some OTHER client used to reach the publishing process, so for the queue consumer it resolves to a host/port that does not serve the certificate — the fetch fails and the notification cannot be verified at all, even though SignatureVersion and Signature are present and correct. A consumer that instead compares the envelope's own members sees one document claiming two different origins, and any check asserting SigningCertURL shares the host of UnsubscribeURL (the AWS invariant) fails.
Fix: the SNS notification-envelope signing members (state/notification_signature.rs `certificate_url` / `Signer::of_topic`, reached from the Publish fan-out in ops/publish.rs) — publish the certificate URL under the SAME authority the envelope's Unsubscribe/Subscribe URL members use for the message's Region, so one envelope names one host, and keep the certificate served at that authority's `/SimpleNotificationService-<id>.pem` so the fetch a subscriber makes is actually answered.
Explicitly NOT part of this finding: the Timestamp and the Signature bytes in the same body also differ from the recording, and both are per-run values (a clock reading and a signature over it) rather than requirements.
Traces: sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator: emulator_up=true ("/__admin/health answered 200 and the :42623 data listener accepts"), bootstrap_signal=false, 44 fixtures, 370 steps, 363 passed, 6 mismatched, 0 errored, 1 blocked, score 98.1%, divergence_count=6. Service token: sqs (bootstrapped). Cluster: stub_path="", owned_by=[], affected_count=1.

sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found#task#9, ReceiveMessage, verdict mismatch, expected_status 200 / actual_status 200 (divergence_key service=sqs|ReceiveMessage|status=200->200|keys=$.Messages[0].Body):
  $.Messages[0].Body: host_not_own

  Expected (recorded) envelope members, abridged:
    "SigningCertURL" : "https://sns.us-west-2.amazonaws.com/SimpleNotificationService-<32 hex>.pem"
    "UnsubscribeURL" : "https://sns.us-west-2.amazonaws.com/?Action=Unsubscribe&SubscriptionArn=arn:aws:sns:us-west-2:..."
  Actual envelope members, abridged:
    "SigningCertURL" : "https://127.0.0.1:43677/SimpleNotificationService-<32 hex>.pem"
    "UnsubscribeURL" : "https://sns.us-west-2.amazonaws.com/?Action=Unsubscribe&SubscriptionArn=arn:aws:sns:us-west-2:..."
  Type, MessageId, TopicArn, Message and SignatureVersion matched exactly. Timestamp and Signature also differ and are per-run values, excluded from the finding. The announced authority is not the data listener the corpus is replayed against either, which is what the harness reports as host_not_own.

  Code facts (state/notification_signature.rs):
    fn certificate_url(ctx, region) -> Option<String> {
        let path = signing_key()?.certificate_path();
        let authority = reached_emulator_endpoint_of_parts(ctx.parts())
            .or_else(|| served_endpoint_authority(ctx.endpoint().as_str()))
            .unwrap_or_else(|| format!("sns.{region}.amazonaws.com"));
        Some(format!("https://{authority}{path}"))
    }
  `Signer::of_topic` resolves `region` from the topic ARN (else the request's region) and then calls the above per request; `certificate_path()` already produces the AWS form `/SimpleNotificationService-<32 hex>.pem`, and the module's own doc comment acknowledges the regional endpoint is "the same last resort the SubscribeURL and UnsubscribeURL of these documents name that endpoint by" — i.e. the sibling members use it first while this member uses it last.

## Steps to Reproduce
0. git_clone the repo, boot the emulator built from that checkout, and call replay_wire_corpus() over simparity-wire-aws-v1; re-replay sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found#task#9 to confirm a fix.

1. CreateQueue, then SNS CreateTopic and Subscribe(Protocol='sqs', Endpoint=<the queue ARN>) with RawMessageDelivery left at its default (envelope delivery).
2. SNS Publish(TopicArn=<the topic>, Message=<any text>).
3. SQS ReceiveMessage on the queue and parse Messages[0].Body as JSON.
4. Read the envelope's "SigningCertURL" and "UnsubscribeURL" and compare their hosts: against AWS both name the SNS endpoint of the message's Region; against the emulator UnsubscribeURL names sns.<region>.amazonaws.com while SigningCertURL names the loopback authority (host and port) the publishing request reached the process at.
5. GET the announced "SigningCertURL" as the queue consumer would: against AWS it answers the PEM certificate; against the emulator the announced host/port is not the one serving the consumer, so the fetch does not reach the certificate and the Signature cannot be verified.
6. Ignore Timestamp and Signature when comparing — both are per-run values.
