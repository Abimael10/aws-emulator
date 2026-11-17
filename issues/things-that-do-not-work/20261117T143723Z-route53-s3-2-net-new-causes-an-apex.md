Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53__s3.apex-alias-hosted-zone-id-must-match-the-aliased-s3-website-endpoint route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header
Issue-Key: route53__s3.apex-alias-hosted-zone-id-must-match-the-aliased-s3-website-endpoint|_expect_error#20|AssertionError|raise AssertionError(f"expected the change to be refused, but it succeeded: {result!r}")
Issue-Key: route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header|_nameserver_ips#5|AssertionError|assert entry["addresses"], (
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# route53__s3: 2 net-new causes — an apex alias whose HostedZoneId does not match the aliased S3 website endpoint is accepted, and a hosted zone's delegated nameservers resolve nowhere

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ChangeResourceRecordSets

## Description
Two distinct root causes in the Route 53 control plane and in the nameservers it delegates a zone to.

Cause 1 of 2: ChangeResourceRecordSets accepts an apex A-record alias whose AliasTarget.HostedZoneId belongs to a different region's S3 website endpoint than the AliasTarget.DNSName it names.
AWS: Smithy (route53) models ChangeResourceRecordSets with AliasTarget{HostedZoneId, DNSName, EvaluateTargetHealth} and the error InvalidChangeBatch. AWS validates that the alias target's DNSName lies within the zone named by HostedZoneId and refuses the batch with InvalidChangeBatch; the message is the bracketed sentence '[Tried to create an alias that targets <dns-name>., type A in zone <hosted-zone-id>, but the alias target name does not lie within the target zone]'. Fixed text: '[Tried to create an alias that targets ', '., type A in zone ', ', but the alias target name does not lie within the target zone]'; the DNS name and the hosted zone id are values taken from the request, not literals. The same change with the matching S3-website hosted zone id must then succeed and read back with that HostedZoneId and DNSName.
Emulator: the mismatched change is accepted — 'AssertionError: expected the change to be refused, but it succeeded' with HTTP 200 and a ChangeInfo in PENDING. No alias-target zone validation happens at all.
Why: a client mis-pastes a region's S3-website hosted zone id (the single most common Route 53 alias mistake) and Route 53 accepts it; the apex then resolves to nothing and the only system that would have caught it at submission time stayed silent.
Fix: ChangeResourceRecordSets' AliasTarget validation (DNSName must lie within the zone named by HostedZoneId) and the InvalidChangeBatch message it raises.
Traces: route53__s3.apex-alias-hosted-zone-id-must-match-the-aliased-s3-website-endpoint.

Cause 2 of 2: the nameservers a created hosted zone is delegated to are AWS-shaped names that resolve to no address, so nothing can query the zone the emulator serves.
AWS: Smithy (route53) models CreateHostedZone -> DelegationSet.NameServers; those names resolve to addresses that answer DNS for the zone, which is the only way a record the zone holds can be observed. The host names themselves are environment-specific and are not a requirement — what is a requirement is that the names the service hands back are reachable where the caller is.
Emulator: resolving the delegated nameserver fails — 'AssertionError: the delegated nameserver ns-751.awsdns-27.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}'. The emulator announces public AWS nameserver names rather than an address it serves DNS on, so no record it holds can be looked up.
Why: host-header-based selection of an S3 website bucket through a Route 53 record cannot be exercised; more generally, every DNS-driven integration (alias to a website endpoint, weighted/latency records, CNAME validation) is unobservable because the zone's own delegation points away from the emulator.
Fix: the CreateHostedZone/GetHostedZone DelegationSet.NameServers the emulator announces, and the DNS listener that must answer for the zone on the address it announces.
Traces: route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header.

## Evidence
Cause 1 — Expected: ('InvalidChangeBatch', '[Tried to create an alias that targets <s3-website-host>., type A in zone <other-region-s3-website-zone-id>, but the alias target name does not lie within the target zone]'), and the zone left with only its default NS/SOA records. Actual: HTTP 200 with ChangeInfo {'Status': 'PENDING'} — 'AssertionError: expected the change to be refused, but it succeeded'.
Cause 2 — Expected: each name in CreateHostedZone's DelegationSet.NameServers resolves to an address that answers DNS for the zone. Actual: 'the delegated nameserver ns-751.awsdns-27.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}'. (The particular ns-… label is a per-run value; the defect is announcing names the emulator serves no DNS on.)

## Steps to Reproduce
Cause 1: CreateHostedZone for a test zone; CreateBucket named for that zone and PutBucketWebsite on it; ChangeResourceRecordSets UPSERT an apex A record with AliasTarget{DNSName: the current region's S3 website endpoint host, HostedZoneId: ANOTHER region's S3-website hosted zone id}. Read the error code and message, then repeat with the matching hosted zone id.
Cause 2: CreateHostedZone and read DelegationSet.NameServers; resolve each name from the caller's vantage point and query it for a record in the zone.
