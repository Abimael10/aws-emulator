Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it|_poll_dns#23|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: af6e2ca0d6958cf434154027297611bddfae7bb3

# route53resolver: an associated FORWARD ResolverRule never diverts queries for its DomainName in the VPC DNS data plane — the private-zone answer keeps being served

> _Note: this issue was identified against the code base at commit `af6e2ca0d6958cf434154027297611bddfae7bb3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateResolverRule

## Description
One net-new root cause, reached by 1 check listed under `### route53resolver`.

Cause 1 of 1: a Resolver rule with RuleType=FORWARD, a DomainName and TargetIps, associated to a VPC via AssociateResolverRule (association reaching Status=COMPLETE), has no effect on the answers the emulator's in-VPC DNS resolver serves for that domain. The querying workload keeps receiving the VPC-local/private-hosted-zone answer for the rule's DomainName as though no rule were associated. Failing check: route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it.

NOTE — this is one step PAST the already-open issue 20261120T050704Z-route53resolver-in-vpc-dns-data-plane-serves-no.md, which also lists this check id. That issue is bound to the FIRST _poll_dns of the check failing with 'the private hosted zone <zone>.internal never became visible to the VPC resolver'. In this replay the private hosted zone IS now visible from inside the VPC (the first poll and both of its assertions pass) and the check gets all the way to the SECOND poll, which fails with a different message from a different call site: 'the FORWARD rule never diverted <subject> away from the resolver's own answer'. So private-zone visibility is no longer the blocker; the un-emulated behavior is now the resolver-rule association's effect on the answer, which the suite reports as unowned.

AWS: Smithy (services/service-com-amazonaws-route53resolver-route53resolver) ResolverRule.RuleType documents the semantics directly: "When you want to forward DNS queries for specified domain name to resolvers on your network, specify FORWARD or DELEGATE. If a query matches multiple Resolver rules (example.com and www.example.com), outbound DNS queries are routed using the Resolver rule that contains the most specific domain name." AssociateResolverRule(ResolverRuleId, VPCId) is what makes a rule take effect for a VPC; its association reports Status=COMPLETE. The DNS answer itself is recorded behavior only — no Smithy shape models a DNS response. What the recording shows: once a FORWARD rule for subject.<zone>.internal pointing at TargetIps=[{Ip: <address in the subnet with nothing listening>, Port: 53}] is associated and COMPLETE, a query from inside that VPC for that exact name stops returning the private-hosted-zone answer (the query is forwarded to the dead target, so it presents as no answer records — a client timeout or a SERVFAIL both qualify; the rcode is not pinned), while an unrelated control name in the SAME private zone keeps resolving NOERROR with its configured address, showing the rule is scoped to the domain it names and its subdomains. DisassociateResolverRule restores the original answer: the matched name resolves NOERROR again with the address its zone holds.

Emulator: the FORWARD rule is created and kept faithfully at the control plane (TargetIps round-trips, the association reaches COMPLETE, ListResolverRuleAssociations would list it), but the data plane ignores it. For the whole 300 s polling window after the association completed, the in-VPC query for the rule's DomainName still comes back with answers — the resolver's own private-zone answer — never the diverted/no-answer result. The check therefore exhausts its budget and errors: `TimeoutError: the FORWARD rule never diverted subject.<zone>.internal away from the resolver's own answer: the run budget ended first; not graded` (raised from _poll_dns, check line 350, reached from run line 576). In that message the fixed text is "the FORWARD rule never diverted … away from the resolver's own answer"; the name `subject.sim2real-r53r-<hex>.internal` is a per-run generated value from the request and is not part of the requirement.

Why: a client cannot test or rely on hybrid-DNS routing. Conditional forwarding — the core purpose of Route 53 Resolver rules — is invisible: code that associates a FORWARD rule to send a corporate domain to on-premises resolvers sees queries for that domain still answered locally, so a misrouted or an unreachable forwarding target is indistinguishable from a working one. The same gap hides rule precedence (most-specific DomainName wins, SYSTEM overriding FORWARD for a subdomain) and makes DisassociateResolverRule's restore semantics untestable, since nothing ever changed to restore.

Fix: the VPC DNS resolver data plane — the component that answers queries addressed to the VPC's .2 / 169.254.169.253 resolver — must consult the ResolverRuleAssociations of the querying VPC before answering from its own zones: select the matching rule by most-specific DomainName suffix match, and for RuleType=FORWARD send the query to the rule's TargetIps instead of answering locally (an unreachable target yielding no answer records), leaving names not matched by any rule to resolve exactly as before. It must also stop applying a rule once its association is removed. The control-plane state the emulator already stores for CreateResolverRule/AssociateResolverRule is correct; only its consumption in the answer path is missing.

Traces: route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it, as an example.

## Evidence
Expected (recorded AWS behavior), all queries issued from inside a VPC with DNS support enabled through the VPC resolver, with a private hosted zone associated to that VPC holding A records for subject.<zone>.internal and control.<zone>.internal:
  1. Before any rule: both names -> rcode NOERROR with their configured addresses. (This step now PASSES on the emulator.)
  2. After CreateResolverRule(RuleType=FORWARD, DomainName=subject.<zone>.internal, ResolverEndpointId=<outbound endpoint>, TargetIps=[{Ip: <subnet>.250, Port: 53}]) + AssociateResolverRule(VPCId=<vpc>) reaching Status=COMPLETE:
     - subject.<zone>.internal -> no answer records (forwarded to a target with nothing listening; client timeout or SERVFAIL both acceptable).
     - control.<zone>.internal -> still NOERROR with its address (the rule is scoped to the domain it names).
  3. After DisassociateResolverRule: subject.<zone>.internal -> NOERROR again, answers[0].rdata == the address its zone holds.

Actual (emulator), route53resolver.forward-rule-diverts-only-the-matched-domain-and-disassociation-restores-it [error]:
  Step 1 passes — the private zone is visible from inside the VPC and both names answer.
  Step 2 never happens: for the full 300 s window the in-VPC query for subject.<zone>.internal keeps returning the resolver's own answer records, so the poll predicate (no answers for the subject) is never satisfied and the check raises, from _poll_dns (check line 350) reached via run (check line 576):
    TimeoutError: the FORWARD rule never diverted subject.sim2real-r53r-d00606e549ba.internal away from the resolver's own answer: the run budget ended first; not graded
  Step 3 is never reached.

The diverging behavior is the answer content: AWS serves NO answer records for the rule's DomainName once the FORWARD association is COMPLETE; the emulator serves the unchanged local answer. The zone label, the hex suffix in the generated zone name, the VPC CIDR, the .250 target address and all resource ids are per-run values and are not part of the requirement.

This replay key (_poll_dns raising on the SECOND poll with the 'never diverted' message) is not carried by any open issue; 20261120T050704Z-route53resolver-in-vpc-dns-data-plane-serves-no.md is keyed to this check failing on its FIRST poll with 'the private hosted zone … never became visible', which no longer reproduces.

## Steps to Reproduce
1. ec2 CreateVpc (e.g. 10.61.0.0/16) with DNS support and DNS hostnames enabled, two subnets in different AZs, and the VPC's default security group.
2. route53 CreateHostedZone(PrivateZone=true, VPC=<that vpc>) for <zone>.internal; ChangeResourceRecordSets CREATE A records for subject.<zone>.internal and control.<zone>.internal pointing at in-subnet addresses; wait for the change to reach INSYNC.
3. Stand up an in-VPC querier (e.g. a VPC-attached Lambda) that resolves a name through the VPC resolver (169.254.169.253) and returns {rcode, answers}. Query both names: expect NOERROR with the configured addresses. (This succeeds today.)
4. route53resolver CreateResolverEndpoint(Direction=OUTBOUND) in those subnets with that security group; wait for it to become operational.
5. CreateResolverRule(RuleType="FORWARD", DomainName="subject.<zone>.internal", ResolverEndpointId=<endpoint>, TargetIps=[{"Ip": "<subnet-prefix>.250", "Port": 53}]) — an address in the subnet with nothing listening on 53.
6. AssociateResolverRule(ResolverRuleId=<rule>, VPCId=<vpc>) and poll GetResolverRuleAssociation until Status=COMPLETE.
7. Re-query from inside the VPC, polling for up to 300 s. EXPECTED: subject.<zone>.internal returns no answer records (forwarded to the dead target) while control.<zone>.internal still returns NOERROR with its address. OBSERVED: subject.<zone>.internal keeps returning the private-zone answer for the entire window and the poll times out with 'the FORWARD rule never diverted <subject> away from the resolver's own answer'.
8. DisassociateResolverRule and re-query: EXPECTED subject.<zone>.internal back to NOERROR with the zone's address (unreachable today, since the answer never changed in step 7).
