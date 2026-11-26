Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-group-configuration-and-topology-negative
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-group-configuration-and-topology-negative autoscaling-group-dependent-operations-on-nonexistent-group
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|AttachLoadBalancers|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 4e36bbfa0b5661aa2fd9826663b014bc119c6585

# autoscaling AttachLoadBalancers validates load-balancer names before the group lookup, so a missing group is refused with the load-balancer sentence

> _Note: this issue was identified against the code base at commit `4e36bbfa0b5661aa2fd9826663b014bc119c6585`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachLoadBalancers

## Description
One net-new root cause in the Auto Scaling group-scoped refusal path, on AttachLoadBalancers. This bundle carries 1 cause and it must be closed.

Cause 1 of 1: AttachLoadBalancers naming an Auto Scaling group that does not exist is refused with the emulator's "the load balancers may not be valid" sentence instead of AWS's group-not-found message, because the load-balancer-name check runs before (or instead of) the group lookup.

AWS: Smithy (com.amazonaws.autoscaling#AttachLoadBalancers, 2011-01-01) models AutoScalingGroupName (XmlStringMaxLen255, required, @length 1..255) and LoadBalancerNames (required list), and only InstanceRefreshInProgressFault (awsQueryError 'InstanceRefreshInProgress', httpError 400), ResourceContentionFault (500) and ServiceLinkedRoleFailure (500) as errors. No not-found fault is modeled, so an unknown AutoScalingGroupName is refused with the awsQuery service-common ValidationError. Recorded behavior only for the text and the ordering: HTTP 400, Code 'ValidationError', Message 'AutoScalingGroup name not found - null'. Fixed text of that template: the whole sentence 'AutoScalingGroup name not found - null' — on this operation AWS emits the literal 'null' where its sibling operations interpolate a value, so there is no variable part here taken from the request or from state (the same literal-'null' quirk AWS shows on DetachLoadBalancerTargetGroups). What the requirement states is that the group is resolved FIRST: because the named group does not exist, the LoadBalancerNames in the request are never examined.

Emulator: HTTP 400 with Code 'ValidationError' but Message 'Provided Load Balancers may not be valid. Please ensure they exist and try again.' — an emulator-authored sentence AWS does not emit for this request, reporting a defect in the LoadBalancerNames list for a request whose only defect is the nonexistent AutoScalingGroupName. The group lookup is never reached (or its refusal is overwritten by the load-balancer check).

Why: a client relying on AWS's behavior reads the refusal to decide what to fix. Against AWS the message says the Auto Scaling group is gone, so the correct reaction is to stop and re-create or re-resolve the group; against the emulator the message says the load balancers are suspect, so the client re-checks, re-creates or re-lists Classic Load Balancers that are in fact fine and retries the attach against a group that will never exist. Error-message matching used to distinguish 'group gone' from 'bad load balancer' — the only way to tell them apart, since both are ValidationError — inverts, and the attach loop never terminates.

Emulator fix: the AttachLoadBalancers handler's validation in the autoscaling service crate — resolve the request's AutoScalingGroupName against group state and raise the group-not-found ValidationError refusal (with AWS's exact message text for this operation) BEFORE validating LoadBalancerNames; the load-balancer-validity sentence must only be served when the group exists.

Traces: autoscaling-group-configuration-and-topology-negative#task#9 (cluster of 2; also autoscaling-group-dependent-operations-on-nonexistent-group#task#11). Related but NOT covering this cluster: open issue 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md (group-scoped describes and DeleteNotificationConfiguration templates, plus NextToken precedence) and 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md (the DetachLoadBalancerTargetGroups '... - null' variant); neither carries this cluster's replay key and neither names AttachLoadBalancers' ordering defect.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, bootstrap_signal=false, '/__admin/health answered 200'): 90 fixtures replayed, 1066 steps, 883 passed, score 82.8%, 67 divergence clusters. 66 of those clusters are already owned by open issues and none is stub-blocked; the cluster below is the only unowned one.

autoscaling-group-configuration-and-topology-negative#task#9 — service=autoscaling, operation AttachLoadBalancers, expected HTTP 400 / actual HTTP 400:
  $.ErrorResponse.Error.Message: value_mismatch — expected 'AutoScalingGroup name not found - null', got 'Provided Load Balancers may not be valid. Please ensure they exist and try again.'
  divergence_key: service=autoscaling|AttachLoadBalancers|status=400->400|keys=$.ErrorResponse.Error.Message
  Cluster of 2: also autoscaling-group-dependent-operations-on-nonexistent-group#task#11.
  The HTTP status (400) and the error Code ('ValidationError') agree; only the Message diverges. The expected message has no per-run values in it — AWS emits the literal 'null' tail on this operation — so nothing in the expectation is environment- or account-specific.

Model evidence (Smithy com.amazonaws.autoscaling, 2011-01-01, services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json):
- AttachLoadBalancers: input AutoScalingGroupName (XmlStringMaxLen255, required, length 1..255) and LoadBalancerNames (required list); errors InstanceRefreshInProgressFault (awsQueryError 'InstanceRefreshInProgress', httpError 400), ResourceContentionFault (500), ServiceLinkedRoleFailure (500). No group-not-found fault is modeled, so the refusal is the awsQuery service-common ValidationError; the message template and the group-before-load-balancer ordering are recorded behavior only.

## Steps to Reproduce
Replay the bound wire corpus against the emulator (replay_wire_corpus) and inspect:
1. Fixture autoscaling-group-configuration-and-topology-negative, step task#9 — AttachLoadBalancers with an AutoScalingGroupName that was never created, plus one or more LoadBalancerNames. Compare $.ErrorResponse.Error.Message: it must be AWS's group-not-found sentence for this operation ('AutoScalingGroup name not found - null'), not 'Provided Load Balancers may not be valid. Please ensure they exist and try again.'. HTTP status stays 400 and Code stays 'ValidationError'.
2. Same check in fixture autoscaling-group-dependent-operations-on-nonexistent-group, step task#11 (other member of the same cluster).
3. To confirm the ordering specifically: issue AttachLoadBalancers naming a nonexistent group AND load balancers that do exist — the refusal must still be the group-not-found message, proving the group is resolved before LoadBalancerNames is examined.
