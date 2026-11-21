Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer
Issue-Key: ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect|_claim#2|AssertionError|assert message.startswith(taken) and message.endswith('"'), (
Issue-Key: ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer|find#2|TimeoutError|raise TimeoutError(f"a matching resolver query log record in {lg!r}: the run budget "
Issue-Commit: 77bfd47bd7799ecf9c5ff0eea8e18d7399a37670

# ec2__lambda__logs__route53resolver: 2 net-new causes — the RSLVR-02203 priority-conflict message omits AWS's trailing Trace Id clause, and Resolver query logging writes no record to its destination log group

> _Note: this issue was identified against the code base at commit `77bfd47bd7799ecf9c5ff0eea8e18d7399a37670`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFirewallRule

## Description
Two net-new root causes in the Route 53 Resolver surface. Both checks now get PAST in-VPC DNS resolution, so neither is the cause carried by open issue 20261119T232925Z-ec2-lambda-logs-route53resolver-the-vpc-route-53-resolver-answers-nothing-from.md (that issue's replay keys for these two checks are `_poll_dns#2|TimeoutError`; both checks now fail later, at `_claim`, with different keys). That issue does not cover either cause below.

---
Cause 1 of 2: CreateFirewallRule's refusal of a second rule at a priority already taken in the same rule group carries only the first half of AWS's message — the trailing ` Trace Id: "..."` clause is missing entirely.

AWS: Smithy (route53resolver): `CreateFirewallRule` declares `ValidationException` (client error, httpError 400) among its errors. Recorded message, as AWS words this error class:
  `[RSLVR-02203] The DNS Firewall rule group already has a rule associated with the same priority: "<firewall-rule-group-id>". Provide another priority. Trace Id: "<trace-id>"`
Fixed text (a requirement, verbatim): `[RSLVR-02203] The DNS Firewall rule group already has a rule associated with the same priority: "`, then `". Provide another priority. Trace Id: "`, then the closing `"`. Variable parts, taken from the request/state: the firewall rule group id (the `FirewallRuleGroupId` the request named) and the per-request trace id. The trace id's VALUE is per-run and is not a requirement; the presence of the ` Trace Id: "<value>"` clause is — Route 53 Resolver terminates its error messages with it.

Emulator: the message stops after `Provide another priority.` — the ` Trace Id: "..."` clause is absent, so the message neither ends in `"` nor carries a trace id at all.

Why: a client (or an operator reading a failure) that matches or logs Route 53 Resolver's documented message shape sees a truncated message and has no trace id to quote to support. Any consumer keyed on the full message text fails to recognise the error.

Fix: the route53resolver CreateFirewallRule validation path's error message construction (the priority-conflict branch) — it must append the ` Trace Id: "<id>"` clause the service always emits.

Traces: ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect.

---
Cause 2 of 2: a Resolver query-log configuration associated with the VPC writes no query-log record at all to its destination CloudWatch Logs log group, so a query the in-VPC client provably made is never recorded.

AWS: recorded behavior only (no Smithy shape describes a query-log record's content). The recording shows: after `CreateResolverQueryLogConfig` with a CloudWatch Logs log group as DestinationArn and `AssociateResolverQueryLogConfig` for the VPC, every DNS query issued from inside that VPC produces a log event in that log group carrying at least the queried name (`query_name`), and a firewall rule whose Action is ALERT logs the match while still returning the answer.

Emulator: no matching record ever appears. The check polls the destination log group for a record whose `query_name` is the name it just queried, and exhausts its whole window and the run budget without finding one — i.e. the log group receives no query-log events.

Why: Resolver query logging is the only record of what a VPC workload resolved. With the destination never written, every audit, troubleshooting and DNS-Firewall ALERT workflow built on resolver query logs sees an empty log group, and an ALERT rule is indistinguishable from no rule at all.

Fix: the route53resolver query-log data plane (the component that, for a VPC with an associated ResolverQueryLogConfig, emits one record per resolved query — including firewall-action annotation — to the config's DestinationArn).

Traces: ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer.

## Evidence
Cause 1 — `ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect` [mismatch]:

    AssertionError: losing racer 'd1' was refused with
      '[RSLVR-02203] The DNS Firewall rule group already has a rule associated with the same priority: "rslvr-frg-<id>". Provide another priority.',
    expected
      '[RSLVR-02203] The DNS Firewall rule group already has a rule associated with the same priority: "rslvr-frg-<id>". Provide another priority. Trace Id: "1-...'

Expected: the message ends with ` Trace Id: "<trace-id>"` (closing double quote included).
Actual: the message ends at `Provide another priority.`.
(The `rslvr-frg-...` id is the FirewallRuleGroupId from the request; the trace id is per-run. Only the presence of the clause is graded as fixed text.)

Cause 2 — `ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer` [error]:

    TimeoutError: a matching resolver query log record in '/sim2real/r53r/<id>': the run budget ended first; not graded

raised from the check's `find(lambda r: r.get("query_name","").rstrip(".").lower() == target)` poll over the destination log group.
Expected: a record naming the queried name appears in the destination log group shortly after the query.
Actual: the log group stays empty for the whole polling window.

## Steps to Reproduce
Cause 1 (RSLVR-02203 message missing the Trace Id clause):
1. route53resolver CreateFirewallRuleGroup, and CreateFirewallDomainList twice (two distinct domain lists).
2. CreateFirewallRule(FirewallRuleGroupId=<group>, FirewallDomainListId=<list A>, Priority=100, Action='BLOCK', Name='r1'). It succeeds.
3. CreateFirewallRule(FirewallRuleGroupId=<group>, FirewallDomainListId=<list B>, Priority=100, Action='BLOCK', Name='r2'). Expected: ValidationException, HTTP 400, message '[RSLVR-02203] The DNS Firewall rule group already has a rule associated with the same priority: "<group id>". Provide another priority. Trace Id: "<trace id>"'. Observed: the same message truncated after 'Provide another priority.' — no ' Trace Id: "..."' clause.

Cause 2 (Resolver query logging writes no record):
1. CreateVpc with EnableDnsHostnames=true, one subnet, its default security group.
2. logs CreateLogGroup <dest>; route53resolver CreateResolverQueryLogConfig(DestinationArn=<that log group ARN>) and AssociateResolverQueryLogConfig(ResolverQueryLogConfigId=<id>, ResourceId=<vpc>); wait for the association to become ACTIVE.
3. Create a firewall rule group with an ALERT rule for a domain, and AssociateFirewallRuleGroup to the VPC.
4. From inside the VPC (a VpcConfig-attached Lambda querying the VPC resolver), resolve that domain. Expected: the answer is returned AND a record whose query_name is that domain appears in <dest> within the polling window. Observed: the answer path proceeds but <dest> never receives any query-log record.
