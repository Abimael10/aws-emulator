Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied
Issue-Key: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied|_poll_signed_post#8|Timeout|time.sleep(5)
Issue-Commit: abfae19fd21aa073ab8e8a9bd7a2f677b9ea769b

# apigateway__events__sqs: a denied execute-api target's dead letter is never observed by the denial check

> _Note: this report is split out of
> `issues/things-that-now-work/20261023T210711Z-apigateway-events-sqs-all-four-eventbridge-to-rest.md`,
> whose other three checks that fix resolved. It carries the one key of that group that still
> fails, so the group's report can be closed without claiming this check passes. It was
> written against commit `abfae19fd21aa073ab8e8a9bd7a2f677b9ea769b`, so the code base you are
> working in can have drifted._

## Operation
PutEvents (delivery to an `execute-api` target under a role that may not invoke it)

## Description
Cause 1 of 1: the check `apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied`
ends `Check timed out after 60.0s`, inside its dead-letter poll
(`found = _receive(sqs, dlq_url, lambda m: _marker_of(m) in attempted, want=1, …)`), so the
denial it grades cannot be observed.

AWS: EventBridge invokes an API Gateway target AS the role the target names ("for EC2
instances, Kinesis data streams, Step Functions state machines and API Gateway APIs,
EventBridge relies on IAM roles that you specify in the `RoleARN` argument in `PutTargets`"),
so a role that may not `execute-api:Invoke` the target's own ARN produces no request the stage
serves, and the event goes to the target's dead-letter queue with the delivery's error code and
message on the message's attributes.

Emulator: the delivery IS refused and IS dead-lettered — the role's trust is read and its
policies are evaluated for `execute-api:Invoke` on the target's own ARN, and a role no readable
document grants it fails the delivery with `NO_PERMISSIONS`, whose event is written to the
target's dead-letter queue with `RULE_ARN`, `TARGET_ARN`, `ERROR_CODE` and `ERROR_MESSAGE`
(pinned end to end by `core/router/tests/all/events_rest_api_stage_target.rs`,
`a_delivery_to_a_stage_that_fails_is_dead_lettered_with_what_it_came_to`). What could not be
established is why THIS recorded check's poll never sees a message: for its target no delivery
appears to be reached at all.

Why: the one signal a caller has that a mis-scoped target role stopped a delivery is the
dead-letter record, and for this configuration the emulator produces none that the check can
find, so a denied delivery is indistinguishable from an event nothing matched.

## Evidence
What was established while fixing the sibling checks, so the next reader does not repeat it:

* The three sibling checks of the group pass, so matching on a custom bus, `PutTargets` with an
  `execute-api` ARN plus `HttpParameters`, the `InputTransformer` rendering, the signed request
  at the stage, the retry sequence and the dead-letter write (with its `CONNECTION_FAILURE`
  condition and its message) all work for them. One of them reads a dead letter off the same
  code path and finds it.
* An instrumented run (not landed) that dead-lettered EVERY outcome class of the delivery pass
  — including the two early returns, the target kind this emulator has no invocation for and an
  input it could not reshape — and that bypassed the dead-letter queue's own policy check still
  produced no message this check could find. So the write is not being refused and the outcome
  is not being mis-classified: the handover is not reached for that target at all.
* That leaves the setup this check performs before publishing (its rule, its target or its
  publication) being read differently here than by AWS, or the dead-letter message's body not
  carrying the marker the check filters on. The check's own source is not readable from the
  fixer session, so which of those it is was not settled.

## Steps to Reproduce
1. Deploy a REST API stage whose method is reachable (the sibling checks' configuration).
2. Create a rule matching a published event and a target naming
   `arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/POST/<path>/*` with a `RoleArn`
   whose policies do NOT allow `execute-api:Invoke` on that ARN, plus a `DeadLetterConfig`.
3. `PutEvents` a matching event, then poll the dead-letter queue for the event.
   -> EXPECT one message carrying the published event and the delivery's error code.
   -> OBSERVE nothing within the check's budget.
4. Or replay the check id in trace_ids.
