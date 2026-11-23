Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-domain-lifecycle-and-tags
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-domain-lifecycle-and-tags
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|UntagResource|2xx->2xx|status
Issue-Commit: bf885f34b3f735f6772d083f3c74d2d1df307dc4

# connectcases UntagResource answers HTTP 200 instead of the modeled 204 No Content

> _Note: this issue was identified against the code base at commit `bf885f34b3f735f6772d083f3c74d2d1df307dc4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UntagResource

## Description
ONE net-new wire divergence on the connectcases surface. It is a response-serialization fault: the emulator performs the right tag-removal business logic but renders the success response with the wrong HTTP status line, so a client that reads the status of an empty-output tagging call gets the wrong answer. THIS ISSUE HAS 1 CAUSE.

=== Cause 1 of 1: a successful UntagResource answers HTTP 200 with a serialized empty body where AWS answers 204 with no content ===
AWS: Smithy (com.amazonaws.connectcases#UntagResource) -> input UntagResourceRequest {arn (Arn, @httpLabel, @required, @length min 1 max 500), tagKeys (TagKeyList, @httpQuery "tagKeys", @required, @length min 0 max 50)}, output target smithy.api#Unit, @http DELETE /tags/{arn}, @idempotent; errors AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429), ValidationException (400). The model's http code is reported DEFAULTED (code 200, codeDefaulted: true), so the model does not pin the success status — the recorded behavior does. Recorded behavior: a successful UntagResource against an existing resource ARN answers HTTP 204 with no content, which is the shape of a Unit-output tagging call on this service (its sibling TagResource records the same 204). No error code or message is involved here: the requirement is the status line of the success response, and the absence of a body with it.
Emulator: answers HTTP 200 for the same successful untag (the keys named in tagKeys are in fact removed and a later ListTagsForResource reports the reduced set correctly — only the status line, and the empty structure serialized under it, diverge).
Why: a client that branches on the status of a no-content response is misled. Generated SDKs, HTTP middlewares and caches that distinguish 204 (no entity body) from 200 (entity body present) see a 200 carrying a serialized empty structure where the real service sends no content at all; a user who calibrates assertions, retry policy or response-body handling for connectcases untagging against this emulator writes code that disagrees with AWS, and conformance suites asserting 204 for the empty-output tagging operations fail against the emulator while passing against AWS.
Fix: the UntagResource operation's declared success status in its handler module in the connectcases service crate — the operation's success status, and the http code on its Unit output schema, must carry this operation's recorded 204 rather than falling back to the protocol default 200. The handler's own comment states that dropping the http code makes "a modelled 204 answer 200", which is exactly the observed behavior.
Related-but-not-covering: the open issue 20261114T191411Z-connectcases-response-serialization-diverges-tagresource-success-status-and.md carries the IDENTICAL fault for the sibling TagResource (fixture connectcases-domain-lifecycle-and-tags#task#4) plus a ListTemplates cause; it does not carry a replay key for this UntagResource step and the replay reports this cluster as UNOWNED, so it does not cover this divergence. Fixing both operations at once is natural, but this step must be closed on its own key.
Traces: connectcases-domain-lifecycle-and-tags#task#6 (UntagResource). Handler: services/service-com-amazonaws-connectcases-amazonconnectcases/src/ops/untag_resource.rs lines 81-90 (Unit output SCHEMA built with HttpTrait::new("DELETE", "/tags/{arn}", None) — no code) and line 158 (const SUCCESS_STATUS: u16 = 200u16).

## Evidence
Cause 1 — fixture connectcases-domain-lifecycle-and-tags, step task#6, operation UntagResource, verdict mismatch:
  detail: "status: expected 204, got 200"
  expected_status 204, actual_status 200
  divergence_key: service=connectcases|UntagResource|status=204->200|keys=status
  affected: connectcases-domain-lifecycle-and-tags#task#6 (1 step)
  The tag removal itself succeeds — no body member diverges, only the status line. AWS's 204 carries no content; the emulator's 200 carries a serialized empty output structure.
  Model citation: Smithy UntagResource {arn @httpLabel @required @length 1..500, tagKeys @httpQuery "tagKeys" @required @length 0..50} -> smithy.api#Unit, @http DELETE /tags/{arn} (code 200 but codeDefaulted: true, so the 204 is recorded behavior), @idempotent.
  Code citation: services/service-com-amazonaws-connectcases-amazonconnectcases/src/ops/untag_resource.rs:86-90 (HttpTrait::new("DELETE", "/tags/{arn}", None)) and :158 (const SUCCESS_STATUS: u16 = 200u16).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Fixture connectcases-domain-lifecycle-and-tags, step task#6: CreateDomain, TagResource on the domain ARN with a tag map, then DELETE /tags/{domainArn}?tagKeys=<one of the written keys> (UntagResource).
3. Observe the response status: the emulator answers HTTP 200 with a serialized empty body; AWS answers HTTP 204 with no content.
4. Confirm the divergence is status-only: a subsequent ListTagsForResource on the same ARN reports the tag set with the named keys removed, as expected.
