Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__iam__lambda.e2e-debug-api-gateway-iam-authorization-lifecycle
Issue-Key: apigateway__dynamodb__iam__lambda.e2e-debug-api-gateway-iam-authorization-lifecycle|_reap_log_group#9|Timeout|return
Issue-Commit: b11785bf9bf8af57a00ad7e4d814b1c5f1d66117

# apigateway__dynamodb__iam__lambda: the REST API IAM-authorization lifecycle never completes — the debug scenario hangs out its budget with no verdict

> _Resolution: the cause as REPORTED — "one of the phases blocks on a poll that is never
> satisfied (a deployed stage that never becomes servable, or a control-plane read that never
> reports the deployed method), rather than being refused" — is not present at this commit, and
> `core/router/tests/all/apigateway_iam_authorization_debug_lifecycle.rs` now pins what the
> report says AWS does, over one estate and end to end: `GetResources` keys each resource's
> `resourceMethods` by the verbs put on it (embedding the `Method` only where `embed=methods`
> asks for it, and reporting no map for a resource carrying none), `GetMethod` reports the
> method's `authorizationType`, IAM's `ListRoles`/`ListRolePolicies`/`GetRolePolicy`/`GetRole`
> report the execution role and its inline document, the deployed stage either serves the
> request or refuses it `403` with the IAM authorization sentence for that principal and method
> ARN (and `Missing Authentication Token` for a request carrying no credential),
> `TestInvokeMethod` answers its status/body/log/latency envelope, the function's log group holds
> the run's records until a `DeleteLogGroup` takes it away — after which the next
> `DescribeLogGroups` reports nothing and a second delete is refused
> `ResourceNotFoundException`, which is what lets a teardown that reaps a log group until it is
> gone STOP — and every delete of the estate is answered its modelled success with the record
> gone from the next read. No call of the lifecycle is refused or left unanswered: the whole
> sequence, including the teardown, settles in under 10 s against the assembled emulator.
> What remains of the recorded failure is the harness's own WALL CLOCK: this five-phase scenario
> (deploy, setup, solve, cleanup, destroy), whose client-side `_await` helper sleeps 3 s between
> probes and which the recorded run cut off inside `_reap_log_group`, does not finish inside the
> 60 s cap, and the report states no AWS-published latency for any of it — so no AWS rule fixes
> it, and the replay of this check will keep timing out until a later change makes the whole
> lifecycle fit the cap. The emulator-side spans the lifecycle does pay are the ones the
> project's own transition rule requires (a created DynamoDB table reaching `ACTIVE`, a created
> function reaching `Active`), so there is nothing here to shorten._

> _Note: this issue was identified against the code base at commit `b11785bf9bf8af57a00ad7e4d814b1c5f1d66117`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources, GetMethod, TestInvokeMethod

## Description
Cause 1 of 1: an end-to-end REST-API scenario that deploys a method with AWS_IAM authorization, inspects it through the control plane and then exercises it never reaches an observable outcome.

AWS: the scenario's control-plane reads answer immediately — GetResources lists the resource paths with their `resourceMethods`, GetMethod reports `authorizationType` (`AWS_IAM`), and IAM's ListRoles / ListRolePolicies / GetRolePolicy / GetRole return the execution role and its inline policy — and the data-plane call that follows is either served or refused with a 403 from the signed/unsigned request. Every step returns within seconds. Smithy: APIGateway models GetResources (`items[].resourceMethods`) and GetMethod (`authorizationType`), and IAM models the four role reads; the lifecycle's terminal-answer property is recorded behavior.

Emulator: the check ends with `Check timed out after 60.0s` — no phase reports a result, so there is no response to compare. Hypothesis: one of the phases blocks on a poll that is never satisfied (a deployed stage that never becomes servable, or a control-plane read that never reports the deployed method), rather than being refused.

Why: a client cannot diagnose or exercise IAM authorization on a REST API against the emulator — the normal sequence of "inspect the method, inspect the role, call the endpoint" neither succeeds nor fails, so automation built on it hangs instead of getting an answer.

Fix: the apigateway service crate's deployed-stage serving path together with the control-plane reads the scenario depends on (GetResources' `resourceMethods` projection and GetMethod's `authorizationType`), so each phase returns a terminal result and the AWS_IAM-authorized method is reachable or refused. Several open apigateway issues (e.g. 20261108T003043Z-apigateway-dynamodb-iam-lambda-3-net-new-causes.md) cover other checks in this service; none carries this check's replay key.

Traces: apigateway__dynamodb__iam__lambda.e2e-debug-api-gateway-iam-authorization-lifecycle.

## Evidence
Expected: the deploy/setup/solve phases each complete — GetResources returns the resource path with resourceMethods, GetMethod reports authorizationType, the IAM role reads return the role and policy, and the authorized/unauthorized invocations are answered.
Actual: `Error: Check timed out after 60.0s` — no phase verdict is produced at all.

## Steps to Reproduce
1. Deploy a REST API whose method has AuthorizationType AWS_IAM and a Lambda integration writing to DynamoDB, plus the execution role and inline policy.
2. Call GetResources and GetMethod on it, then ListRoles / ListRolePolicies / GetRolePolicy / GetRole for the role.
3. Invoke the deployed stage with and without SigV4 credentials.
The scenario never produces a terminal result inside its budget. Replay apigateway__dynamodb__iam__lambda.e2e-debug-api-gateway-iam-authorization-lifecycle.
