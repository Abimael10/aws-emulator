Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.elb-target-health-change-event-not-delivered-without-eventbridge-rule
Issue-Key: autoscaling__ec2__elbv2.elb-target-health-change-event-not-delivered-without-eventbridge-rule|run#25|AssertionError|raise AssertionError(
Issue-Commit: edd8d27229a8bc99e308a4b32d393f13d6f32cb7

# CloudWatch/EventBridge: an alarm state transition reaches no matching rule's SQS target in the check that watches an ALB's target health

> _Re-filed from `issues/things-that-now-work/20261107T173527Z-autoscaling-ec2-elbv2-auto-scaling-elbv2-asg-instances-are-never-registered.md`, which carried this as its CAUSE 2 of 2. Cause 1 of that report (Auto Scaling target registration, `DetachInstances`, and the ELB health verdict's replacement) is fixed; this cause is not, so it is carried here with its own replay key and trace id rather than left on a record filed as working._

## Operation
SetAlarmState

## Description
Cause 1 of 1: the behavioral check `autoscaling__ec2__elbv2.elb-target-health-change-event-not-delivered-without-eventbridge-rule` never receives the alarm state-change envelope on the SQS queue its EventBridge rule targets.

AWS: every alarm state transition (including one forced with `SetAlarmState`) is published to
the account's default event bus as an event with source `aws.cloudwatch`, detail-type
`CloudWatch Alarm State Change`, and a detail carrying `alarmName`, `state.value` and
`previousState.value`; a rule whose pattern matches those fields delivers the event to its
target (here an SQS queue).

Emulator: the check creates an internal ALB and a target group, puts a metric alarm on
`AWS/ApplicationELB` `UnHealthyHostCount` under the `TargetGroup` + `LoadBalancer` dimension
pair, forces `ALARM` with `SetAlarmState` (`DescribeAlarms` confirms it), confirms nothing is
delivered while no rule exists, creates a matching rule with an SQS target, and then
re-drives the `OK` transition for 120 s while polling the queue. No message arrives. Under
the replay harness's 60 s per-check budget the verdict is `Check timed out after 60.0s`
inside `_receive_matching_message`, i.e. inside `sqs.receive_message` — every call the check
makes is served, and nothing diverges on the wire.

## What has already been ruled out

The publication path IS implemented and is pinned end-to-end, including over the WHOLE
assembled emulator in the same shape the check uses (alarm over an ALB's
`UnHealthyHostCount`, a queue whose policy grants `events.amazonaws.com`, a rule matching the
envelope, the `OK` transition re-driven in a loop):

* `core/router/tests/all/elb_alarm_transition_reaches_its_rule_queue.rs` — both tests pass.
* `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/alarm_state_change_events.rs`,
  `.../alarm_state_change_reaches_its_rule_target.rs`, `.../alarm_state_change_event_chain.rs`.

Two concrete emulator-side theories were implemented and then reverted, because each left
the replay verdict byte-identical (`unchanged: true`), so neither is the cause:

1. a routed target group holding no registration publishing `HealthyHostCount` /
   `UnHealthyHostCount` ZEROS, which would grade the check's alarm `OK` and make a re-asked
   `SetAlarmState OK` no transition at all (`SetAlarmState` publishes only when the state
   it is asked for differs from the one the alarm is in);
2. the same for ANY zero count — Elastic Load Balancing's documented "reporting criteria:
   there is a nonzero value" — which removes every way such an alarm can sit in `OK`.

Ruled out by reading the code: the queue-policy decision
(`eventbridge::state::target_policy_admits` already decides `aws:SourceArn` /
`aws:SourceAccount`); the queue-URL form (`sqs::state::queue_name_from_url` resolves a queue
by its `/<account>/<name>` path, so the URL EventBridge builds reaches it); long polling
(`sqs::ops::receive_message` returns as soon as a message is available); and the
authenticating-router axis (the same in-process publication path is pinned under signature
verification by `core/router/tests/all/batch_job_state_change_on_an_authenticating_router.rs`,
and several `things-that-now-work` reports record EventBridge-rule-to-SQS delivery working
for other publishers in this same corpus).

## What the next fixer should check

* Whether the check's target group is ROUTED by a listener and holds registrations whose
  health the load balancer has DECIDED, and therefore what state the alarm's own evaluation
  leaves it in at the instant of each `SetAlarmState OK`: a transition only publishes when
  the asked-for state differs from the stored one.
* Whether the `_receive_matching_message` helper's own budget, rather than any emulator
  behaviour, is what the 60 s per-check budget is spent on — and if so, what in the
  emulator's answers makes the earlier phases slow.

## Evidence
Expected: an SQS message whose body has detail-type `CloudWatch Alarm State Change`,
`detail.state.value` `OK` and `detail.previousState.value` `ALARM`. Actual: no matching
message in 120 s of retried transitions —
`AssertionError: expected message for alarm sim2real-tg-alarm-<id> never received on <queue-url> despite retrying the OK transition for 120s`
(the alarm name, the queue URL and the ids are per-run values; the requirement is the event's
source, detail-type and detail fields).

## Steps to Reproduce
1. Replay the behavioral suite (run_behavioral_suite) and look under `### autoscaling__ec2__elbv2`.
2. Create an internal ALB and a target group, put a metric alarm on `AWS/ApplicationELB`
   `UnHealthyHostCount` with the `TargetGroup` + `LoadBalancer` dimensions, `SetAlarmState`
   `ALARM`, confirm the queue receives nothing while no rule exists, create an EventBridge
   rule with pattern
   `{source:[aws.cloudwatch], detail-type:[CloudWatch Alarm State Change], detail:{alarmName:[<name>]}}`
   and an SQS target, then re-drive `SetAlarmState OK` and poll the queue — no message
   arrives.
