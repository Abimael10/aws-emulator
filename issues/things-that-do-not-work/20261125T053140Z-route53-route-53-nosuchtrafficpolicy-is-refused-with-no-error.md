Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53.traffic-policy-metadata-and-comment-update route53.traffic-policy-version-lifecycle
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53.traffic-policy-metadata-and-comment-update|run#21|AssertionError|assert err.get("Message") == "The policy does not exist.", err
Issue-Key: route53.traffic-policy-version-lifecycle|run#27|AssertionError|assert err.get("Message") == "The policy does not exist.", err
Issue-Commit: 3d1a1d384dfcb950d8b44fcf4acfc270984665bb

# Route 53 NoSuchTrafficPolicy is refused with no Error.Message: AWS sends the fixed sentence 'The policy does not exist.'

> _Note: this issue was identified against the code base at commit `3d1a1d384dfcb950d8b44fcf4acfc270984665bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTrafficPolicy

## Description
One net-new root cause, observed on the two operations in this suite that refuse a traffic policy version the store does not hold. NOTE on history: a previously resolved behavioral issue (20261109T063005Z-route53-route-53-nosuchtrafficpolicy-is-answered-with-an-empty.md) carried these same two checks, and a later resolved WIRE issue (20261121T214447Z-route53-route-53-gettrafficpolicy-404-nosuchtrafficpolicy-body-carries-an.md) asked for the Message element to be REMOVED from this very error class; the code today is messageless again and both behavioral checks mismatch. Neither resolved issue covers this failure, so it is filed here, and the two recordings must be reconciled rather than flip-flopped (see Hypothesis under Fix).

Cause 1 of 1: the shared NoSuchTrafficPolicy refusal constructor emits no Message child, so every operation that raises it answers with an empty reason.

AWS: Smithy (services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json): NoSuchTrafficPolicy is a structure with errorKind=client, httpError=404, one optional member `message` (target ErrorMessage, empty member doc); shape doc "No traffic policy exists with the specified ID." — that is the RULE, not the wire sentence. It is bound to GetTrafficPolicy (errors: InvalidInput 400, NoSuchTrafficPolicy 404) and to UpdateTrafficPolicyComment (errors: ConcurrentModification 400, InvalidInput 400, NoSuchTrafficPolicy 404). The model does not state the message text, so for the text this is RECORDED BEHAVIOR ONLY: both recordings show that when a request names an (Id, Version) pair the account does not hold — a version never created, or a version a previous DeleteTrafficPolicy removed — Route 53 refuses with Error.Type=Sender, Error.Code=NoSuchTrafficPolicy and Error.Message="The policy does not exist.". That message is entirely FIXED text: it has NO variable parts — it never echoes the policy id, the version, the policy name, or the operation, so nothing in it comes from the request or from emulator state.

Emulator: Type and Code already match; the Message element is absent, and botocore normalizes the missing child to the empty string. Replayed Error dict for both checks: {'Message': '', 'Code': 'NoSuchTrafficPolicy', 'Type': 'Sender'}. The omission is deliberate in the code: the one shared constructor builds NoSuchTrafficPolicy { message: None } and documents it as "MESSAGELESS … the `Error` element carries `Type` and `Code` and no `Message` child", so the serializer emits no element. That premise is wrong for this error class on these paths.

Why: the Message is the only human-readable part of a 404 that AWS explains. A client that logs or surfaces Error.Message shows an empty reason, and any caller or test that branches on, asserts, or string-matches "The policy does not exist." diverges even though the code is right — not-found handling written against AWS fails against the emulator (and the reverse: code written against the emulator learns there is never a message here).

Fix: the one shared refusal constructor for this error class in the service's state module — `no_such_traffic_policy()` in services/service-com-amazonaws-route53-awsdnsv20130401/src/state.rs — must populate `message` with AWS's fixed sentence so every operation that raises NoSuchTrafficPolicy words it identically (GetTrafficPolicy and UpdateTrafficPolicyComment are the two exercised here; CreateTrafficPolicyVersion, DeleteTrafficPolicy and ListTrafficPolicyVersions share the same constructor). Hypothesis (neither model nor recording states it): the wire fixture that wanted NO Message addressed an id the account holds no policy for at all, whereas both behavioral checks address an id the account DOES hold at a version it does not — if that distinction is real, the message belongs on the version-not-found path and the discriminator must live in the handler, not in the shared constructor. The sibling `no_such_traffic_policy_instance()` is messageless on the identical stated premise, but no check here grounds it, so it is out of scope and only flagged for re-check.

Traces: route53.traffic-policy-metadata-and-comment-update (GetTrafficPolicy with Version = created version + 99) and route53.traffic-policy-version-lifecycle (UpdateTrafficPolicyComment on a version just removed by DeleteTrafficPolicy).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against the booted emulator — 2 mismatches, both AFTER Error.Type and Error.Code already matched:

1. [mismatch] route53.traffic-policy-metadata-and-comment-update, check line 58:
   assert err.get("Message") == "The policy does not exist.", err
   AssertionError: {'Message': '', 'Code': 'NoSuchTrafficPolicy', 'Type': 'Sender'}
   Path: CreateTrafficPolicy(Name, Document) -> GetTrafficPolicy(Id, Version=version+99).

2. [mismatch] route53.traffic-policy-version-lifecycle, check line 78:
   assert err.get("Message") == "The policy does not exist.", err
   AssertionError: {'Message': '', 'Code': 'NoSuchTrafficPolicy', 'Type': 'Sender'}
   Path: CreateTrafficPolicy -> CreateTrafficPolicyVersion -> DeleteTrafficPolicy(v2) -> UpdateTrafficPolicyComment(Id, Version=v2).

Expected (AWS): the modeled NoSuchTrafficPolicy refusal (httpError 404) with Error.Type=Sender, Error.Code=NoSuchTrafficPolicy and Error.Message="The policy does not exist." — wholly fixed text, no request- or state-derived parts.
Actual (emulator): same Type and Code, with NO Message element in the Error at all (botocore reports '').

Source of the omission in the checkout:
- services/service-com-amazonaws-route53-awsdnsv20130401/src/state.rs, `pub fn no_such_traffic_policy() -> NoSuchTrafficPolicy { NoSuchTrafficPolicy { message: None } }` (around line 3398), with the comment block above it declaring the error class MESSAGELESS.
- Reached from services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/get_traffic_policy.rs:162 and services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/update_traffic_policy_comment.rs:161 and :168.

Model evidence: smithy_query(query="shape", shape="NoSuchTrafficPolicy") -> {type: structure, errorKind: client, httpError: 404, members: {message: ErrorMessage}, doc: "No traffic policy exists with the specified ID."}.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the behavioral checks route53.traffic-policy-metadata-and-comment-update and route53.traffic-policy-version-lifecycle; both mismatch on the Message assertion.
3. By hand, with a route53 client pointed at the emulator:
   a. create_traffic_policy(Name=<unique>, Document='{"AWSPolicyFormatVersion":"2015-10-01","RecordType":"A","Endpoints":{"e1":{"Type":"value","Value":"192.0.2.1"}},"StartEndpoint":"e1"}') and note Id and Version.
   b. get_traffic_policy(Id=<id>, Version=<version>+99) and inspect the ClientError's response['Error'].
   c. create_traffic_policy_version(Id=<id>, Document=<any valid document>), delete_traffic_policy(Id=<id>, Version=<v2>), then update_traffic_policy_comment(Id=<id>, Version=<v2>, Comment='x') and inspect response['Error'].
4. In both (b) and (c) the Error carries Code=NoSuchTrafficPolicy, Type=Sender and Message='' (no Message element on the wire) instead of AWS's fixed 'The policy does not exist.'.
