Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__lambda.xsvc-elbv2-3-lambda-invoke-permission-removed-is-denied-with-502-until-regranted
Issue-Key: elbv2__lambda.xsvc-elbv2-3-lambda-invoke-permission-removed-is-denied-with-502-until-regranted|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: b4ead8169e48e6047949699a772a41fd75a6bc4d

# ELBv2 Lambda target: revoking and regranting the invoke permission never settles

> _Note: this issue was identified against the code base at commit `b4ead8169e48e6047949699a772a41fd75a6bc4d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTargets

## Description
Cause 1 of 1: an Application Load Balancer with a Lambda target group must answer 502 once the function's elasticloadbalancing invoke permission is removed, and answer normally again once it is regranted.
AWS: recorded behavior only — the recording registers the function as a target, proves a request succeeds, calls lambda:RemovePermission for the elasticloadbalancing.amazonaws.com statement, observes HTTP 502 from the listener, then re-adds the permission and observes success again; on AWS the whole cycle completes inside the suite's 60s budget.
Emulator: the check is cut off with "Error: Check timed out after 60.0s" — neither the denied leg nor the regranted leg ever produces a listener response, so the poll never terminates.
Why: a client cannot observe the permission's effect at all: a revoked grant produces no 502 and a restored grant produces no recovery, so an ALB-to-Lambda authorization mistake is invisible.
Fix: the ELBv2 listener's Lambda-target invocation path, including the resource-policy evaluation and the 502 it must produce when the invoke is refused.
Traces: elbv2__lambda.xsvc-elbv2-3-lambda-invoke-permission-removed-is-denied-with-502-until-regranted.

## Evidence
Expected (AWS): the listener answers 502 while the elasticloadbalancing invoke permission is absent and answers normally after it is regranted.
Actual (emulator): 'Error: Check timed out after 60.0s' — the listener never answers on either leg.

## Steps to Reproduce
1. Create an ALB with a target group of TargetType=lambda, RegisterTargets with a function, and AddPermission for elasticloadbalancing.amazonaws.com.
2. Request the listener (succeeds), RemovePermission, request again expecting 502, AddPermission again, request again.
3. Observe the scenario never settles within 60s.

## Resolution

The rule this issue is about — *a load balancer invokes a `lambda` target group's registered
function as its own service principal, so the invocation is admitted only by that FUNCTION's
resource-based policy, read on EVERY request; an invocation it does not admit is not made and
the load balancer answers its own 502, and one it admits is served from the function's own
response with no re-registration of the target* — holds on this code base, and the replay
reports this issue's one cause as having moved past the frame it was filed on.

The path it needed is in place: `src/state/load_balancer_endpoint.rs` opens the socket a load
balancer publishes as its address, `src/service/listener_socket.rs` hands every request it
accepts to `src/service/data_plane.rs`, and `src/service/lambda_target.rs` reads the registered
function's policy from Lambda itself and decides `lambda:InvokeFunction` for the
`elasticloadbalancing.amazonaws.com` principal through the tree's one policy evaluator, with the
target group's ARN as `aws:SourceArn` — generating the 502, and its `LambdaAccessDenied`
access-log reason, with nothing invoked when the decision is not an allow.

What was NOT pinned was the other half of that rule. The suites covered the STATE of one
documented grant (in place, withdrawn, restored); none covered the SHAPE of the grant, which
decides the same request. `tests/all/alb_lambda_target_invoke_grant.rs` now spans it over a real
connection to the published address, with the function's own SQS side channel proving whether it
ran: a grant narrowed to another target group, to another account or to an organization, a grant
written for another service principal, a grant of another action, and no policy at all each earn
the 502 with nothing invoked; the documented grant, a grant narrowed by an ARN pattern covering
the group, a grant narrowed to no invoking resource, a grant of every action of the service and a
grant narrowed to the invoking account each serve the function's own response.

The recorded check still does not reach a verdict, on a DIFFERENT cause, which is filed and
owned elsewhere: its remaining divergence is the `_poll_until#54` frame carried by
`20261114T003845Z-elbv2-lambda-an-application-load-balancer-with-a.md` and
`20261111T201108Z-elbv2-lambda-removing-lambda-invokefunction-from-a-registered.md`. The blocker
under it is that a load balancer cannot answer on the port its listener declares, so the
published address has to carry the socket's own port — closed as a core-listener blocker by
`20261110T220525Z-elbv2-5-net-new-data-plane-causes-dnsname.md`.
