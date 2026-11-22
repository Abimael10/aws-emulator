Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-component-tagging-workflow
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-component-tagging-workflow
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|TagResource|2xx->2xx|$headers.content-length
Issue-Key: imagebuilder|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: a7b3a929bdd53afd3c781969eac09fa5fa2267e9

# Image Builder tagging 204 responses omit the Content-Length framing header

> _Note: this issue was identified against the code base at commit `a7b3a929bdd53afd3c781969eac09fa5fa2267e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
This bundle carries TWO distinct causes in the Image Builder emulator's tag plane. Both are the same fault — an empty-bodied success response is written without the Content-Length framing header AWS sends — but they live on two different operations, so BOTH must be closed before this issue is submitted.

(Related but NOT covering: no open issue names the imagebuilder tag plane. The open issues for the same fault in other services — e.g. 20261109T115329Z-mediapackagev2-tagresource-and-untagresource-204-responses-omit-the-content.md, 20261121T211322Z-eks-tagging-204-responses-omit-the-content-length.md, 20261119T014646Z-braket-204-no-content-answers-omit-the-content.md — are scoped to those services and carry no imagebuilder replay key, so the fixture steps named here are not covered by them. The open imagebuilder issues all concern body projection or refusal wording, not response framing.)

Cause 1 of 2: TagResource's empty success response carries no Content-Length header.
AWS: Smithy (com.amazonaws.imagebuilder) TagResource — http POST /tags/{resourceArn}, input {resourceArn (ImageBuilderArn, @httpLabel, required), tags (TagMap, required, length 1..50)}, output shape TagResourceResponse with NO members, errors InvalidParameterException (400), ResourceNotFoundException (404), ServiceException (500). Because the output structure is empty there is no payload to serialize, and for the response status itself the model is only prose-free scaffolding (modeled http code 200, recorded 204) — so the framing is recorded behavior only: the recording shows AWS answering the successful tag mutation with HTTP 204 and a Content-Length header present on the response (its value is not asserted by the fixture; for a body-less reply the only well-formed value is 0). The requirement is that an empty-bodied 2xx reply is framed explicitly rather than left unframed.
Emulator: the same request gets HTTP 204 — status matches — but the response has NO Content-Length header at all ($headers.content-length: missing, expected present, got None). Nothing else in the step diverged.
Why: a client cannot distinguish "the server declared a zero-length body" from "the server sent no framing and the body may be truncated or still coming". HTTP clients, proxies and SDK wire-log/assertion layers that read Content-Length to decide a response is complete — and connection-reuse logic that needs an explicit length to know the message ended before the next response on the same keep-alive connection — behave differently against the emulator than against AWS, and any harness that asserts on the response envelope fails on an otherwise correct reply.
Fix: the restJson1 response serializer / envelope writer for the imagebuilder service crate's TagResource handler — the path that emits a no-content success must set the Content-Length framing header on empty-bodied 2xx replies instead of omitting it.
Traces: imagebuilder-component-tagging-workflow#task#3.

Cause 2 of 2: UntagResource's empty success response carries no Content-Length header either.
AWS: Smithy (com.amazonaws.imagebuilder) UntagResource — http DELETE /tags/{resourceArn}, input {resourceArn (ImageBuilderArn, @httpLabel, required), tagKeys (TagKeyList, @httpQuery "tagKeys", required, length 1..50)}, output shape UntagResourceResponse with NO members, errors InvalidParameterException (400), ResourceNotFoundException (404), ServiceException (500). As with TagResource the framing is recorded behavior only: the recording shows AWS answering the successful untag with HTTP 204 and a Content-Length header present (value not asserted).
Emulator: HTTP 204 with the Content-Length header absent ($headers.content-length: missing, expected present, got None). Status and everything else matched.
Why: same as Cause 1 — the untag reply is unframed, so completeness checks, strict HTTP clients and envelope assertions see a response shape AWS never produces.
Fix: same component as Cause 1 — the empty-response path of the imagebuilder crate's UntagResource handler / its shared no-content serializer. If both operations share one "empty 204" helper, fixing the helper closes both causes, but both fixture steps must be re-replayed to confirm it.
Traces: imagebuilder-component-tagging-workflow#task#5.

## Evidence
Replay of the simparity-wire-aws-v1 wire fixture corpus against the emulator built from this checkout (emulator up, 351 steps, 327 passed, score 93.2%).

Cause 1 — fixture imagebuilder-component-tagging-workflow, step task#3, service imagebuilder, operation TagResource, verdict mismatch, expected status 204 / actual status 204:
  $headers.content-length: missing (expected present — '<shaped-not-asserted>', i.e. the header must exist; got None)

Cause 2 — fixture imagebuilder-component-tagging-workflow, step task#5, service imagebuilder, operation UntagResource, verdict mismatch, expected status 204 / actual status 204:
  $headers.content-length: missing (expected present — '<shaped-not-asserted>'; got None)

In both clusters the status line and every other asserted field agree; the sole divergence is the absent Content-Length response header on a body-less 204. Divergence keys: service=imagebuilder|TagResource|status=204->204|keys=$headers.content-length and service=imagebuilder|UntagResource|status=204->204|keys=$headers.content-length. Neither cluster is blocked by a stub and neither is owned by an open issue.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay fixture imagebuilder-component-tagging-workflow and stop at step task#3 — the fixture creates an Image Builder component, then calls TagResource(resourceArn=<the component build-version ARN>, tags={<key>: <value>}). Inspect the raw response headers: the reply is 204 with no Content-Length header, where AWS sends one.
4. Cause 2: continue the same fixture to step task#5 — UntagResource(resourceArn=<the same component ARN>, tagKeys=[<key>]). Inspect the raw response headers: again 204 with no Content-Length header.
5. Re-replay both fixture#step bindings (the representative above plus the one in trace_ids) to confirm both causes are closed — a shared empty-response helper may fix both at once, but each step must be exercised.
