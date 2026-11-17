Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content
Issue-Key: acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content|_poll_until#37|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# A Route 53 public hosted zone's delegated name servers never answer a DNS query over TCP

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetHostedZone

## Description
Cause 1 of 1: the name servers a hosted zone delegates to never answer an authoritative DNS query over TCP, so nothing downstream of name resolution can be exercised.

AWS: route53 CreateHostedZone/GetHostedZone return `DelegationSet.NameServers`, the four authoritative servers for the zone. Those servers answer queries for names in the zone authoritatively, over UDP and — as RFC 1035 requires of every authoritative server, and as Route 53 implements — over TCP port 53 as well. A query for a record created by ChangeResourceRecordSets must come back with that record and the AA bit set; a name not in the zone must come back NXDOMAIN. (Smithy: route53 GetHostedZone output member `DelegationSet` -> `DelegationSet.NameServers`; the DNS wire behavior itself is not in the model, so: recorded behavior only.)

Emulator: every name server reported in the zone's delegation set is polled over TCP and none of them ever answers. The check's bounded poll exhausts the run budget and raises `TimeoutError: every name server of the zone answering over TCP: the run budget ended first; not graded`. The control-plane zone and record-set calls that precede it succeeded, so the gap is the DNS service behind the delegation set, not the zone's creation.

Why: no client can resolve a name the emulator claims to be authoritative for. Every scenario layered on top of a hosted zone — a CloudFront alternate domain name validated and served under its ACM certificate, an ALB or API Gateway custom domain, private-zone split-horizon resolution — cannot even start, and a caller that follows the delegation set it was handed gets a hang instead of an answer or NXDOMAIN.

Fix: the DNS responder that serves the zones held by the route53 state — specifically its TCP listener on port 53 and the authoritative answer path for names in a hosted zone. The addresses advertised in `DelegationSet.NameServers` must be answered on by that responder.

Traces: acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content

## Evidence
Expected: each name server listed in the zone's `DelegationSet.NameServers` answers a TCP/53 query for a record in that zone authoritatively.
Actual: none of them answers. The bounded poll exhausts the run budget and raises, verbatim:
  TimeoutError: every name server of the zone answering over TCP: the run budget ended first; not graded
raised from `_poll_until(...)`. The route53 control-plane calls (CreateHostedZone, ChangeResourceRecordSets) and the ACM/CloudFront setup that precede it succeeded, so the delegation set is published but nothing answers on it.

## Steps to Reproduce
1. route53 CreateHostedZone for a public zone; read `DelegationSet.NameServers` back with GetHostedZone.
2. ChangeResourceRecordSets to add a record in that zone.
3. Send a DNS query for that record to each advertised name server over TCP port 53.
4. Observe: no name server ever answers; the poll runs out the budget. Expected: an authoritative answer carrying the record (and NXDOMAIN for a name outside the zone).
