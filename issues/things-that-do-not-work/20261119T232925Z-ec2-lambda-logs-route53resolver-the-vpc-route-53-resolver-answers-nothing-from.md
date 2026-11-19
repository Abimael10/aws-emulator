Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__lambda__logs__route53resolver.dns-firewall-block-rule-actually-blocks-resolution-inside-the-vpc ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect ec2__lambda__logs__route53resolver.deleting-a-firewall-domain-list-in-use-rejected-and-it-keeps-blocking ec2__lambda__logs__route53resolver.disabling-autodefined-reverse-lookup-stops-the-reverse-answer ec2__lambda__logs__route53resolver.dns-firewall-block-override-returns-the-configured-override-record ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer ec2__lambda__logs__route53resolver.firewall-block-takes-precedence-over-the-private-hosted-zone ec2__lambda__logs__route53resolver.firewall-rule-priority-decides-which-action-the-resolver-applies ec2__lambda__logs__route53resolver.query-log-config-records-the-query-the-instance-actually-made
Issue-Key: ec2__lambda__logs__route53resolver.dns-firewall-block-rule-actually-blocks-resolution-inside-the-vpc|_claim#2|AssertionError|assert before[0]["answers"], (
Issue-Key: ec2__lambda__logs__route53resolver.concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.deleting-a-firewall-domain-list-in-use-rejected-and-it-keeps-blocking|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.disabling-autodefined-reverse-lookup-stops-the-reverse-answer|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.dns-firewall-block-override-returns-the-configured-override-record|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.firewall-alert-action-logs-the-match-without-blocking-the-answer|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.firewall-block-takes-precedence-over-the-private-hosted-zone|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.firewall-rule-priority-decides-which-action-the-resolver-applies|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: ec2__lambda__logs__route53resolver.query-log-config-records-the-query-the-instance-actually-made|_poll_dns#2|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# The VPC Route 53 Resolver answers nothing from inside the VPC: a public name comes back SERVFAIL with no answers and private-hosted-zone records never become visible, so all 9 DNS-firewall data-plane checks are blocked

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResolverQueryLogConfigAssociations

## Description
One net-new root cause, reached by 9 checks.

Cause 1 of 1: the VPC-provided Route 53 Resolver (the .2 address every instance/ENI in a VPC resolves through) returns no usable answer for queries issued from inside the VPC — neither for a public name nor for a record in a private hosted zone associated with that VPC.

AWS: recorded behavior only for the data plane — no Smithy model describes DNS answers. What the recordings show is: a VPC created with EnableDnsHostnames=true resolves a public name (example.com, A) through the VPC resolver with rcode NOERROR and at least one answer record; a Route 53 private hosted zone created with PrivateZone=true and associated with that VPC, holding an A record created by ChangeResourceRecordSets that reached INSYNC, resolves <label>.<zone> from inside the VPC with NOERROR and the configured address within a couple of minutes; and the autodefined reverse lookup answers PTR for an in-VPC address until UpdateResolverConfig sets AutodefinedReverseFlag=DISABLE, after which the PTR answer stops. Only once those answers exist can the DNS Firewall claims (BLOCK, BLOCK with an override record, ALERT without blocking, rule priority, firewall-vs-private-zone precedence, in-use domain-list delete, concurrent same-priority rules, query logging) be observed at all.

Emulator: the in-VPC querier is invoked successfully and the resolver answers, but with nothing usable. Graded directly: a query for example.com/A from inside the VPC, before any firewall rule exists, comes back {'query': 'example.com', 'qtype': 'A', 'id_match': True, 'rcode': 'SERVFAIL', 'answers': []} — the resolver replies SERVFAIL with an empty answer section where AWS replies NOERROR with an address. The other eight checks poll the resolver's own answer for a private-hosted-zone record (or for the PTR of an in-VPC address to stop) and never see it: each one burns its whole polling window and then the run budget on the very first _poll_dns, i.e. the record a zone associated with the VPC must serve never becomes visible, and disabling AutodefinedReverse never changes the PTR answer.

Why: the Lambda in the VPC is the only vantage point inside the VPC, so no client can observe DNS resolution in a VPC at all. Every Route 53 Resolver and DNS Firewall behavior that is only observable in the answer — a BLOCK actually blocking, an override record being returned, ALERT logging without blocking, rule-priority precedence, firewall precedence over a private hosted zone, autodefined reverse lookup, a query-log entry recording the query an instance really made — is unverifiable, and ordinary workloads (service discovery through a private hosted zone, any outbound name lookup from a VPC-attached function or instance) fail with SERVFAIL.

Fix: the VPC DNS resolver data plane the emulator serves to in-VPC workloads (the component that answers queries addressed to the VPC's .2 resolver): it must resolve public names to a real answer instead of SERVFAIL, must serve the records of every Route 53 private hosted zone associated with the querying VPC, and must honour the resolver config (AutodefinedReverseFlag) and the associated DNS Firewall rule groups when composing the answer.

Traces: the 9 checks in trace_ids. The graded one is ec2__lambda__logs__route53resolver.dns-firewall-block-rule-actually-blocks-resolution-inside-the-vpc (the SERVFAIL above); the other eight end on the first poll of the resolver's answer for a private-zone record or a PTR, which is the same silence. Open issue 20261105T083215Z-ec2-lambda-logs-route53resolver-the-lambda-python-runtime-has-a-crippled-stdlib.md lists the same 9 check ids but is bound to a different failure — the querier Lambda dying with Runtime.ImportModuleError on os.register_at_fork before any query was sent. That no longer happens: the querier now runs and the resolver answers, so this is a distinct, net-new cause one step further down the path and no open issue carries these checks' current replay keys.

## Evidence
Expected (recorded AWS behavior), from inside a VPC with EnableDnsHostnames=true, queried through the VPC resolver by a VpcConfig-attached Lambda:
  - example.com/A → rcode NOERROR with a non-empty answers list.
  - <label>.<private-zone>/A, the zone associated with that VPC and the record change INSYNC → NOERROR with the configured address, within a couple of minutes.
  - the PTR for an in-VPC address answers until UpdateResolverConfig(AutodefinedReverseFlag=DISABLE), then stops answering.

Actual (emulator):
  - ec2__lambda__logs__route53resolver.dns-firewall-block-rule-actually-blocks-resolution-inside-the-vpc (graded mismatch):
    AssertionError: example.com did not resolve from inside the VPC before any rule existed, so blocking it below would prove nothing: {'query': 'example.com', 'qtype': 'A', 'id_match': True, 'rcode': 'SERVFAIL', 'answers': []}
  - deleting-a-firewall-domain-list-in-use-rejected-and-it-keeps-blocking, dns-firewall-block-override-returns-the-configured-override-record, firewall-alert-action-logs-the-match-without-blocking-the-answer, firewall-block-takes-precedence-over-the-private-hosted-zone, firewall-rule-priority-decides-which-action-the-resolver-applies:
    TimeoutError: the private hosted zone <zone>.internal never became visible to the VPC resolver — raised from _poll_dns on the FIRST poll of the check, before any firewall object is created.
  - concurrent-firewall-rules-at-one-priority-admit-exactly-one-and-it-takes-effect:
    TimeoutError: the five private-zone names never all became visible to the VPC resolver.
  - query-log-config-records-the-query-the-instance-actually-made:
    TimeoutError: the wildcard record in <zone>.internal never became visible to the VPC resolver.
  - disabling-autodefined-reverse-lookup-stops-the-reverse-answer:
    TimeoutError: disabling AutodefinedReverse never stopped the PTR answer for <addr>.in-addr.arpa.
  (Zone names, addresses and ids above are per-run values; the invariant is that the VPC resolver never produces the answer AWS produces.)

## Steps to Reproduce
1. CreateVpc (10.99.0.0/16) with ModifyVpcAttribute EnableDnsHostnames=true, one subnet, the VPC's default security group.
2. Create a python3.12 Lambda function with VpcConfig in that subnet whose handler performs DNS queries through the VPC resolver and returns {rcode, answers} per query; wait for State=Active.
3. Invoke it with {"queries": [{"name": "example.com", "type": "A"}]}. Expected rcode NOERROR with a non-empty answers list; observed rcode SERVFAIL with answers == [].
4. CreateHostedZone(PrivateZone=true, VPC=<that vpc>) for <zone>.internal, ChangeResourceRecordSets CREATE target.<zone>.internal A 10.99.88.20, wait for the change to reach INSYNC.
5. Invoke the function with {"queries": [{"name": "target.<zone>.internal", "type": "A"}]} and poll. Expected NOERROR with 10.99.88.20 within ~2 minutes; observed no answer for the whole polling window.
6. route53resolver UpdateResolverConfig(ResourceId=<vpc>, AutodefinedReverseFlag=DISABLE), then query the PTR of an in-VPC address and poll. Expected the PTR answer to stop; observed no change.
