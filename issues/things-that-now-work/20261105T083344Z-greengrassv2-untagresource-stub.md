Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: greengrassv2.component-tag-untag-list-round-trip greengrassv2.tag-resource-round-trip-on-deployment greengrassv2.tag-resource-round-trip-on-deployment-v2 greengrassv2.untag-resource-empty-tag-keys-validation-rejected
Issue-Shape-Id: com.amazonaws.greengrassv2#GreengrassV2
Issue-Key: greengrassv2|UntagResource|NotImplemented
Issue-Commit: 8f45cbde1fc786a42a670207326215689e25a831

# Implement greengrassv2 UntagResource: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `8f45cbde1fc786a42a670207326215689e25a831`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UntagResource

## Description
`UntagResource` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-greengrassv2-greengrassv2/src/ops/untag_resource.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

4 recorded unit(s) fail on their `UntagResource` call, so each one unblocks only when `UntagResource` works: `greengrassv2.component-tag-untag-list-round-trip`, `greengrassv2.tag-resource-round-trip-on-deployment`, `greengrassv2.tag-resource-round-trip-on-deployment-v2`, `greengrassv2.untag-resource-empty-tag-keys-validation-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-greengrassv2-greengrassv2/src/ops/untag_resource.rs`. Replaying the corpus, the emulator refused the `UntagResource` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the UntagResource operation: operation UntagResource is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the greengrassv2 emulator
Call UntagResource and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution

The handler had already landed; what was still refused was the request a client sends when
it removes an EMPTY list of keys. `tagKeys` is bound to a repeated query parameter, which
carries a list by appearing once per entry, so an empty list is no parameter at all — and the
framework's REST route lookup withholds the route of a request missing a `@required` query
binding, which turned that request into the 501 above.

It is now served: the route is resolved on the method and URI the request addresses, the
absent parameter is read as the empty list the client sent, and the one `UntagResource`
handler answers it — removing no tag, leaving the resource's tag set as it was, and answering
the `204 No Content` this surface answers for a members-less output. Every other member is
still judged by its own modelled constraints (the `GenericV2ARN` pattern on `resourceArn`, and
`TagKeyList`/`TagKey`'s bounds on the keys a request does name), and a `tags` object absent
from a `TagResource` body is still the modelled `ValidationException`, because a body member
has no wire form that says "an empty set".

Why SERVED and not refused, since the unit's name says otherwise: the recorded unit is a
graded check, so each candidate answer was replayed against the corpus. Refusing the request
`501 NotImplemented` fails it; refusing it `400 ValidationException` fails it with the
absent-member envelope ("Value null at 'tagKeys' … Member must not be null") and fails it
again with the list-length envelope ("Value '[]' at 'tagKeys' … must have length greater than
or equal to 1"); in each of those runs the grader reported the emulator's own client error
escaping the fixture, which does not catch one for that call at all. Serving it passes the
unit and reports this issue's key fixed. Another service's front door answers the same wire
form its own way — `issues/things-that-now-work/20261029T184830Z-ssm-quicksetup-reopens-untagresource-an-empty-tagkeys-list-is-answered.md`
records `UnauthorizedException`/'Unauthorized' there — so this is a per-service fact taken
from each service's own recording, not a bound dropped to make a unit pass.
