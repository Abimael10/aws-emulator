Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53resolver.firewall-block-response-arms-serve-three-distinct-answers route53resolver.firewall-rule-group-association-priority-decides-the-live-answer route53resolver.firewall-rule-qtype-scopes-the-block-to-that-query-type route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it route53resolver.system-rule-overrides-the-forward-rule-for-the-subdomain
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver.firewall-block-response-arms-serve-three-distinct-answers|_poll_dns#8|Timeout|time.sleep(5)
Issue-Key: route53resolver.firewall-rule-group-association-priority-decides-the-live-answer|_poll_dns#7|Timeout|time.sleep(5)
Issue-Key: route53resolver.firewall-rule-qtype-scopes-the-block-to-that-query-type|run#7|AssertionError|assert before[0]["rcode"] == "NOERROR", (
Issue-Key: route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it|_poll_dns#8|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: route53resolver.system-rule-overrides-the-forward-rule-for-the-subdomain|run#7|AssertionError|assert before[0]["answers"], (
Issue-Commit: 07a81b42592fdd972bbc8a5b9a2141006a4e2532

# route53resolver in-VPC DNS data plane serves no usable answer: public names come back SERVFAIL with an empty answer section and private-hosted-zone records never become visible, blocking all 5 Resolver/DNS-Firewall data-plane checks

> _Note: this issue was identified against the code base at commit `07a81b42592fdd972bbc8a5b9a2141006a4e2532`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFirewallRule

## Description
One net-new root cause, reached by 5 checks listed under `### route53resolver`.

Cause 1 of 1: the VPC-provided Route 53 Resolver (the address an in-VPC workload resolves through — the VPC's .2 address / 169.254.169.253) returns no usable answer for a query issued from inside the VPC. A public name is answered SERVFAIL with an empty answer section, and a record in a Route 53 private hosted zone associated with the querying VPC never becomes visible at all, so no Resolver-rule or DNS-Firewall effect can be observed. Failing checks: route53resolver.firewall-rule-qtype-scopes-the-block-to-that-query-type, route53resolver.system-rule-overrides-the-forward-rule-for-the-subdomain, route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it, route53resolver.firewall-block-response-arms-serve-three-distinct-answers, route53resolver.firewall-rule-group-association-priority-decides-the-live-answer.

AWS: recorded behavior only for the data plane — no Smithy shape describes a DNS answer. What the recordings show: in a VPC with DNS support/hostnames enabled, a query for a public name (example.com, qtype A) issued from inside the VPC through the VPC resolver returns rcode NOERROR with at least one answer record; a private hosted zone created with PrivateZone=true and associated with that VPC, holding a record whose ChangeResourceRecordSets change reached INSYNC, resolves that name from inside the VPC with NOERROR and the configured address within a couple of minutes. Only on top of those answers are the modelled control-plane semantics observable: Smithy (services/service-com-amazonaws-route53resolver-route53resolver) FirewallRule.Action enum ALLOW|BLOCK|ALERT and BlockResponse enum NODATA|NXDOMAIN|OVERRIDE (each arm must show up as a distinct answer), FirewallRule.Qtype (a rule scopes its action to that query type only, other qtypes keep resolving), FirewallRuleGroupAssociation.Priority (the lowest-priority association decides the live answer), and ResolverRule.RuleType, whose own documentation states the precedence: FORWARD for a domain forwards queries for that domain and its subdomains to TargetIps, the most specific matching rule wins, and SYSTEM on a subdomain makes Resolver process queries for that subdomain instead of forwarding them.

Emulator: the in-VPC querier now runs and the resolver replies, but with nothing usable. Graded directly, before any Resolver rule or firewall object exists: a query for example.com/A from inside the VPC comes back {'query': 'example.com', 'qtype': 'A', 'id_match': True, 'rcode': 'SERVFAIL', 'answers': []} — SERVFAIL with an empty answer section where AWS answers NOERROR with an address (checks firewall-rule-qtype-scopes-the-block-to-that-query-type and system-rule-overrides-the-forward-rule-for-the-subdomain both assert on exactly this first poll). forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it burns its whole polling window on its FIRST _poll_dns and raises TimeoutError 'the private hosted zone <zone>.internal never became visible to the VPC resolver' — the record of a zone associated with the VPC is never served. firewall-block-response-arms-serve-three-distinct-answers and firewall-rule-group-association-priority-decides-the-live-answer never get past the same in-VPC polling and consume the whole 60 s check budget ('Check timed out after 60.0s'). (Zone names, addresses and ids in these messages are per-run values; the invariant is that the VPC resolver never produces the answer AWS produces.)

Why: a client's only vantage point inside a VPC resolves nothing. Service discovery through a private hosted zone and any ordinary outbound name lookup from a VPC-attached function or instance fail with SERVFAIL, and every Route 53 Resolver behavior that exists only in the answer is unobservable: which BLOCK response arm (NODATA / NXDOMAIN / OVERRIDE record) is served, which rule-group association Priority decides the live answer, whether a rule's Qtype scopes the block to that query type only, whether a FORWARD rule diverts only the matched domain (and whether disassociating it restores resolution), and whether a SYSTEM rule overrides a FORWARD rule for a subdomain.

Fix: the VPC DNS resolver data plane the emulator serves to in-VPC workloads — the component that answers queries addressed to the VPC's .2 / 169.254.169.253 resolver. It must resolve public names to a real NOERROR answer instead of SERVFAIL, must serve the records of every Route 53 private hosted zone associated with the querying VPC, and must compose the answer from the resolver rules and DNS Firewall rule groups associated with that VPC (Action/BlockResponse/Qtype, association Priority, FORWARD vs SYSTEM precedence and most-specific-domain matching) so the control-plane objects the emulator already stores take effect in the answer.

Related open issues do NOT carry these checks' current replay keys: 20261105T123334Z-route53resolver-every-in-vpc-dns-data-plane-check.md lists these same 5 check ids but is bound to an earlier, now-gone failure (the querier Lambda dying with Runtime.ImportModuleError on os.register_at_fork before any query was sent); the querier now runs and the resolver answers, so this is a distinct cause one step further down the path. 20261119T232925Z-ec2-lambda-logs-route53resolver-the-vpc-route-53-resolver-answers-nothing-from.md describes the same silent data plane but is keyed to the 9 ec2__lambda__logs__route53resolver checks and filed under that service, not to these 5.

Traces: the 5 check ids in trace_ids, as examples.

## Evidence
Expected (recorded AWS behavior), queried from inside a VPC with DNS support enabled through the VPC resolver:
  - example.com / A -> rcode NOERROR with a non-empty answers list.
  - <label>.<private-zone>.internal / A, the zone associated with that VPC and the record change INSYNC -> NOERROR with the configured address, within ~2 minutes.
  - with a BLOCK firewall rule: the arm selected by BlockResponse (NODATA, NXDOMAIN, or the OVERRIDE record) in the answer; with Qtype set, only that query type is affected; the lowest-Priority association decides; a FORWARD rule's matched domain and its subdomains stop resolving normally while a SYSTEM rule on a subdomain restores it.

Actual (emulator):
  - route53resolver.firewall-rule-qtype-scopes-the-block-to-that-query-type (mismatch):
    AssertionError: an A query for example.com did not succeed before any rule existed, so blocking one query type below would prove nothing: {'query': 'example.com', 'qtype': 'A', 'id_match': True, 'rcode': 'SERVFAIL', 'answers': []}
  - route53resolver.system-rule-overrides-the-forward-rule-for-the-subdomain (mismatch):
    AssertionError: example.com served no answer from inside the VPC before any rule existed, so diverting it below would prove nothing: {'query': 'example.com', 'qtype': 'A', 'id_match': True, 'rcode': 'SERVFAIL', 'answers': []}
  - route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it (error), raised from _poll_dns on the FIRST poll of the check:
    TimeoutError: the private hosted zone <zone>.internal never became visible to the VPC resolver: the run budget ended first; not graded
  - route53resolver.firewall-block-response-arms-serve-three-distinct-answers (mismatch): Check timed out after 60.0s while polling the in-VPC resolver.
  - route53resolver.firewall-rule-group-association-priority-decides-the-live-answer (mismatch): Check timed out after 60.0s while polling the in-VPC resolver.

In the two assertions the diverging values are rcode 'SERVFAIL' (AWS: 'NOERROR') and answers [] (AWS: at least one record). The zone name, VPC CIDR, addresses and ids in these messages are per-run values and are not part of the requirement.

## Steps to Reproduce
1. CreateVpc (e.g. 10.60.0.0/16) with DNS support and DNS hostnames enabled, one subnet, the VPC's default security group.
2. Create a python3.x Lambda function with VpcConfig in that subnet whose handler issues DNS queries through the VPC resolver (169.254.169.253) and returns {rcode, answers} per query; wait for State=Active.
3. Invoke it with {"queries": [{"name": "example.com", "type": "A"}]}. Expected rcode NOERROR with a non-empty answers list; observed rcode SERVFAIL with answers == [].
4. route53 CreateHostedZone(PrivateZone=true, VPC=<that vpc>) for <zone>.internal, ChangeResourceRecordSets CREATE <label>.<zone>.internal A <in-VPC address>, wait for the change to reach INSYNC.
5. Invoke the function for <label>.<zone>.internal / A and poll. Expected NOERROR with the configured address within ~2 minutes; observed no answer for the whole polling window.
6. With that resolution working on AWS, create a FirewallRuleGroup + FirewallDomainList + CreateFirewallRule(Action=BLOCK, BlockResponse=NODATA|NXDOMAIN|OVERRIDE, Qtype=...) and AssociateFirewallRuleGroup(Priority=...), and a CreateResolverRule(RuleType=FORWARD, TargetIps=[in-VPC address with nothing listening]) plus a SYSTEM rule on a subdomain, re-querying after each change. Expected the documented arm/priority/precedence to show up in the answer; observed no answer at all to differentiate.
