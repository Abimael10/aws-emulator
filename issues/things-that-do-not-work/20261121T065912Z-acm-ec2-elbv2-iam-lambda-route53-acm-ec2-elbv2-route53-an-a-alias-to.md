Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header
Issue-Key: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header|_poll_until#29|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 18a314cdb2529802996db00768c0e97bdee3f604

# acm/ec2/elbv2/route53: an A alias to an application load balancer is never answered by the zone's delegated name servers, so the ALB HTTPS data plane is unreachable on the custom name

> _Note: this issue was identified against the code base at commit `18a314cdb2529802996db00768c0e97bdee3f604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ChangeResourceRecordSets

## Description
One net-new root cause, grounded in the single failing behavioral check of this service group that the suite does not mark ALREADY FILED or OWNED.

=== Cause 1 of 1: an alias A record whose AliasTarget is an application load balancer is never answered, by any name server the hosted zone delegates to, with addresses the load balancer's own DNS name resolves to. ===

AWS: Smithy (com.amazonaws.route53#AWSDnsV20130401): ChangeResourceRecordSets -> ResourceRecordSet.AliasTarget is a structure with required HostedZoneId (ResourceId, length 0..32), required DNSName (DNSName, length 0..1024) and required EvaluateTargetHealth (default false); its documentation is "Information about the Amazon Web Services resource, such as a CloudFront distribution or an Amazon S3 bucket, that you want to route traffic to", and for an ELB load balancer HostedZoneId is the load balancer's CanonicalHostedZoneId and DNSName its DNSName. The DNS wire behavior is not modeled, so: recorded behavior only — after the change reaches INSYNC (GetChange), every host published in the zone's DelegationSet.NameServers answers a query for the aliased name with rcode NOERROR and the AA (authoritative answer) bit set, and the answer is one or more A records **under the aliased name itself** (never a CNAME to the load balancer's name), whose addresses are a subset of the addresses the load balancer's own DNS name resolves to. Route 53 synthesizes the alias target's addresses at query time; the aliased name is an apex-capable A record.

Emulator: the control plane all succeeds — RequestCertificate/DescribeCertificate issue the certificate, CreateLoadBalancer + CreateListener(HTTPS, fixed-response 403) + CreateRule(http-header condition, fixed-response 200) are accepted, ChangeResourceRecordSets CREATE of the alias is accepted and reaches INSYNC, and DescribeLoadBalancers reports State.Code == "active" — and then the alias is never answered. The check polls the zone's own delegated name servers from a vantage point in the environment for an A answer under the aliased name sharing an address with the load balancer's DNS name, and that condition is never satisfied for the whole life of the run: `TimeoutError: the alias answers sharing an address with the load balancer's own name: the run budget ended first; not graded` (check_acm__ec2__elbv2__iam__lambda__route53_..._gates_a_fixed_200_on_a_secret_header.py line 534 -> _poll_until line 71). Because the DNS step never completes, steps (2)-(5) of the check — the listener's fixed 403 on a request without the secret header, the served certificate's SAN/CN being exactly the custom name, the 403 for a wrong header value, the rule's fixed 200 for the right one, and ACM's InUseBy naming the load balancer — are never exercised; this issue is scoped to the alias-answer divergence only, and those five remain unverified rather than reported here. Hypothesis (mechanism, not asserted by this recording): the same unreachable authoritative plane already reported for plain record sets, where the names published in DelegationSet.NameServers do not resolve from the environment and/or never answer — see open issues 20261117T145616Z-route53-7-net-new-causes-the-authoritative-dns (cause 1) and 20261117T134237Z-acm-cloudfront-iam-lambda-route53-s3-a-route-53-public-hosted-zone-s-delegated. Neither of those covers this check (the suite marks it neither OWNED nor ALREADY FILED): they are about non-alias records and a CloudFront alias respectively, and neither exercises alias-target synthesis for an elbv2 application load balancer, which additionally requires the emulator to join the AliasTarget's CanonicalHostedZoneId/DNSName to the load balancer's current addresses.

Why: this is the standard way a custom HTTPS endpoint is published on AWS — an apex/subdomain A alias to an ALB. A client that does what AWS documents (create the alias, wait for INSYNC, then connect to the name with SNI/Host set to it) can never resolve the name against the emulator, so the ALB listener, its rules, and the ACM certificate served over SNI are all unreachable by name. Everything layered on an alias-to-ALB record (blue/green by alias, header-gated routing, certificate validation over the custom name) is unobservable, and the INSYNC status the emulator reports is not truthful: it claims the change is served everywhere when no delegated server serves it.

Fix: the route53 DNS responder for alias record sets together with the alias-target resolution it uses — a ResourceRecordSet with an AliasTarget pointing at an elbv2 load balancer must be answered, on the hosts published in that zone's DelegationSet.NameServers, as A records under the aliased name with the target load balancer's current addresses, NOERROR + AA; and those published hosts must be resolvable and answering in the environment the emulator is reached on.

Traces: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, service group acm__ec2__elbv2__iam__lambda__route53: 0/1 checks passed (1 failed, verdict `error`).

Failing check: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header

  File "check_acm__ec2__elbv2__iam__lambda__route53_alb_https_listener_on_an_issued_certificate_gates_a_fixed_200_on_a_secret_header.py", line 534, in run
    answers, resolved = _poll_until(
  File "...", line 71, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: the alias answers sharing an address with the load balancer's own name: the run budget ended first; not graded

What succeeded before the poll (so the divergence is not a control-plane failure): the ACM certificate was issued, the security group authorized 443, CreateLoadBalancer returned LoadBalancerArn/DNSName/CanonicalHostedZoneId, CreateListener(HTTPS, Certificates=[cert], DefaultActions=fixed-response 403 'sim2real fixed refusal') and CreateRule(Priority=10, Conditions=[http-header X-Sim2real-Origin-Token], Actions=fixed-response 200) were accepted, ChangeResourceRecordSets CREATE of {Name: <name>, Type: A, AliasTarget: {HostedZoneId: <lb CanonicalHostedZoneId>, DNSName: <lb DNSName>, EvaluateTargetHealth: false}} was accepted and reached INSYNC, and DescribeLoadBalancers reported State.Code == 'active'.

Expected (AWS): for every host in the zone's DelegationSet.NameServers, a query for <name>/A returns rcode=0 with the AA bit set, and the answer records are A records whose owner name is <name> (lowercased) and whose addresses intersect the set of addresses <lb DNSName> resolves to.
Actual (emulator): no delegated name server ever returns such an answer; the poll never satisfies its predicate and runs until the run budget is exhausted. The specific record name, load balancer DNS name, hosted-zone id and addresses are per-run values; the requirement is the shape of the answer, not those values.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. route53: pick/create a public hosted zone and read DelegationSet.NameServers from GetHostedZone.
3. acm: RequestCertificate for a name in that zone and wait until it is ISSUED.
4. ec2/elbv2: create a security group allowing TCP 443, then CreateLoadBalancer(Type='application', Scheme='internet-facing', Subnets=[2 subnets], SecurityGroups=[sg]); note LoadBalancerArn, DNSName and CanonicalHostedZoneId. CreateListener(Protocol='HTTPS', Port=443, Certificates=[{CertificateArn: cert}], DefaultActions=[{Type:'fixed-response', FixedResponseConfig:{StatusCode:'403', ContentType:'text/plain', MessageBody:'sim2real fixed refusal'}}]), then CreateRule(Priority=10, Conditions=[{Field:'http-header', HttpHeaderConfig:{HttpHeaderName:'X-Sim2real-Origin-Token', Values:[<secret>]}}], Actions=[{Type:'fixed-response', FixedResponseConfig:{StatusCode:'200', ...}}]).
5. route53: ChangeResourceRecordSets CREATE {Name: <name>, Type:'A', AliasTarget:{HostedZoneId: <CanonicalHostedZoneId>, DNSName: <lb DNSName>, EvaluateTargetHealth: False}} and poll GetChange until INSYNC. Wait for DescribeLoadBalancers State.Code == 'active'.
6. From a client in the environment, resolve <lb DNSName> to a set of addresses, then send each host listed in DelegationSet.NameServers a query for <name>/A. Observe that no server returns an authoritative (AA) NOERROR answer carrying A records under <name> with any of those addresses — the query either cannot be sent (the advertised host does not resolve) or returns no such answer, indefinitely.
7. Replay acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header to confirm; once the alias is answered, the check continues into the listener's fixed 403/200, the served certificate's SAN/CN, and ACM InUseBy, which this run never reached.
