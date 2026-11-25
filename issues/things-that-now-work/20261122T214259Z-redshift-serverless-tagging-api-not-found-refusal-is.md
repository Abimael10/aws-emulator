Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-serverless.namespace-tags-at-create-round-trip-and-missing-arn
Issue-Shape-Id: com.amazonaws.redshiftserverless#RedshiftServerless
Issue-Key: redshift-serverless.namespace-tags-at-create-round-trip-and-missing-arn|run#19|AssertionError|assert err == want_err, f"ListTagsForResource on a missing namespace returned {err!r}"
Issue-Commit: 667711c55307ac8bb9b0b208bd1f622cac293819

# redshift-serverless tagging-API not-found refusal is worded by the emulator ('Resource <arn> not found.') instead of AWS's 'Namespace not found'

> _Note: this issue was identified against the code base at commit `667711c55307ac8bb9b0b208bd1f622cac293819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
One net-new cause in redshift-serverless.

Cause 1 of 1: the ResourceNotFoundException the tagging APIs raise for a resource ARN that resolves to no stored resource carries an emulator-authored message that echoes the request's ARN, instead of AWS's fixed, resource-type-worded text. Failing check ids: redshift-serverless.namespace-tags-at-create-round-trip-and-missing-arn

AWS: Smithy (services/service-com-amazonaws-redshiftserverless-redshiftserverless/smithy-model/com-amazonaws-redshiftserverless-redshiftserverless.json) models ListTagsForResource and TagResource with input member resourceArn (AmazonResourceName, @length 1..1011, @required) and both declare ResourceNotFoundException (client error, HTTP 404) with a @required `message` member and an optional `resourceName`. The model documents the error only as "The resource could not be found." and does not state the message text, so the message is recorded behavior: for a namespace-form ARN (arn:aws:redshift-serverless:<region>:<account>:namespace/<namespaceId>) whose id was never minted, the recorded refusal is HTTP 404, error code `ResourceNotFoundException`, message exactly `Namespace not found`. That message is ALL fixed text — it is worded from the resource TYPE segment of the ARN (`namespace`) and contains no variable part at all; it does not interpolate the ARN, the namespace id, the account or the region. The same refusal is expected from TagResource against the same ARN.

Emulator: the refusal is raised with the right code and status, but with a message the emulator composes from the request's ARN: `Resource arn:aws:redshift-serverless:<region>:<account>:namespace/<uuid> not found.` (replayed value: 'Resource arn:aws:redshift-serverless:us-west-2:000000000000:namespace/5a793d6e-3816-4af0-a803-1b656f0bb0df not found.' — the uuid/region/account are per-run values, the diverging parts are the `Resource `/` not found.` framing and the interpolated ARN where AWS has the bare `Namespace not found`). The text is produced by the shared helper `crate::state::tagged_resource_not_found(&input.resource_arn)`, so TagResource and UntagResource word their not-found refusals the same wrong way, not just ListTagsForResource.

Why: a client that branches on the refusal text — error-message assertions in tests, log/alert matchers, and UIs that surface the service's wording — sees a message no real Redshift Serverless endpoint produces, and a caller that distinguishes "the namespace does not exist" from "this ARN is not a Redshift Serverless resource" by the message gets the wrong classification. Because the message leaks the full request ARN it is also strictly more verbose than AWS's, so parity checks on the refusal fail even though the status and code are right.

Fix: the not-found message builder shared by the tagging handlers — `crate::state::tagged_resource_not_found` in services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/state.rs, as called from ops/list_tags_for_resource.rs (and the TagResource/UntagResource handlers that use the same helper). It must word the message from the ARN's resource-type segment, emitting AWS's fixed text for that type (`Namespace not found` for a namespace-form ARN), rather than interpolating the request's ARN into a `Resource ... not found.` template. Nothing about the resolution logic (`tags_for_resource_arn`) is wrong — only the message wording.

Note: this is NOT covered by the fixed issue 20261114T171103Z-redshift-serverless-listtagsforresource-answers-an-empty-tag-list.md (the tag round-trip in this same check now passes), nor by 20261029T191808Z-redshift-serverless-refusal-messages-diverge-from-aws-wording.md, which concerned CreateSnapshot/CreateUsageLimit wording and not the tagging APIs' not-found text.

Traces: redshift-serverless.namespace-tags-at-create-round-trip-and-missing-arn — after the create-time/TagResource/UntagResource tag round-trip succeeds, the check calls ListTagsForResource with the namespace ARN whose id is replaced by a fresh uuid and asserts ('ResourceNotFoundException', 'Namespace not found').

## Evidence
Check: redshift-serverless.namespace-tags-at-create-round-trip-and-missing-arn (verdict: mismatch)

Assertion site (check source, lines 586-594):
    missing_arn = arn[: -len(ns["namespaceId"])] + str(uuid.uuid4())
    err = _error(lambda: rss.list_tags_for_resource(resourceArn=missing_arn))
    want_err = ("ResourceNotFoundException", "Namespace not found")
    assert err == want_err, f"ListTagsForResource on a missing namespace returned {err!r}"
    err = _error(lambda: rss.tag_resource(resourceArn=missing_arn, tags=[{"key": "k", "value": "v"}]))
    assert err == want_err, f"TagResource on a missing namespace returned {err!r}"

Expected (AWS, recorded): HTTP 404, Code='ResourceNotFoundException', Message='Namespace not found' (entirely fixed text).

Actual (emulator): HTTP 404, Code='ResourceNotFoundException', Message='Resource arn:aws:redshift-serverless:us-west-2:000000000000:namespace/5a793d6e-3816-4af0-a803-1b656f0bb0df not found.'

Failure output:
    AssertionError: ListTagsForResource on a missing namespace returned ('ResourceNotFoundException', 'Resource arn:aws:redshift-serverless:us-west-2:000000000000:namespace/5a793d6e-3816-4af0-a803-1b656f0bb0df not found.')

Source of the diverging text: services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/ops/list_tags_for_resource.rs lines 156-168 construct ResourceNotFoundException with message = crate::state::tagged_resource_not_found(&input.resource_arn).

## Steps to Reproduce
1. CreateNamespace(namespaceName=<fresh name>, tags=[{key:team,value:analytics},{key:env,value:dev}]) and take namespaceArn / namespaceId from the response.
2. Build a missing ARN by replacing the namespaceId segment of namespaceArn with a fresh uuid4 (same partition, region and account; still namespace-form).
3. Call ListTagsForResource(resourceArn=<that missing ARN>).
   Expected: HTTP 404, ResourceNotFoundException, Message exactly 'Namespace not found'.
   Actual: HTTP 404, ResourceNotFoundException, Message 'Resource <the ARN from step 2> not found.'
4. Call TagResource(resourceArn=<that missing ARN>, tags=[{key:k,value:v}]) — same expected refusal, same diverging wording from the shared helper.
