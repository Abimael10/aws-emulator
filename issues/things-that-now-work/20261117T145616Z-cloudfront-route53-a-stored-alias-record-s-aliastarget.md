Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__route53.alias-record-set-requires-a-cloudfront-net-target-name
Issue-Key: cloudfront__route53.alias-record-set-requires-a-cloudfront-net-target-name|run#13|AssertionError|assert listed[0]["AliasTarget"]["DNSName"] == f"{dist_domain}.", (
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront__route53: a stored alias record's AliasTarget.DNSName is read back with DNS backslash-octal escaping instead of echoing the submitted target name

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResourceRecordSets

## Description
Cause 1 of 1: an alias record set does not round-trip its AliasTarget.DNSName — the value read back is re-encoded in DNS presentation format (characters outside the hostname set rendered as `\\<octal>`), so it no longer equals the name that was submitted.

AWS: Smithy: `AliasTarget.DNSName` (required, target DNSName, length 0..1024) is, per its documentation, "the domain name that CloudFront assigned when you created your distribution" — the caller passes the target's own domain name and Route 53 stores it, normalizing only case and the trailing dot. Recorded behavior: ListResourceRecordSets reports the alias target as the submitted name with a trailing dot added, character-for-character.

Emulator: ChangeResourceRecordSets accepts the alias (with the CloudFront alias hosted zone id Z2FDTNDATAQYW2) and the record is listed under the right name, but `AliasTarget.DNSName` comes back as `'127.0.0.1\\07234865.'` where the submitted target — the DomainName the emulator's own CreateDistribution returned for the distribution — contained a `:` at that position. `\\072` is the DNS presentation escape for octal 072 = ':'. Hypothesis: the record store serializes the stored name through a DNS wire/presentation encoder that escapes every octet outside the permitted hostname character set, and the escaped form is what the XML projection returns. The particular host and port in that value are per-environment and are not the requirement; the requirement is that the member echoes whatever DNSName was submitted (plus the trailing dot).

Why: a client cannot compare, match or re-submit the alias target it reads back — an UPSERT/DELETE built from the listed record names a different target than the one stored, and code that checks "does this alias point at my distribution's DomainName?" always answers no. It also makes the alias-target validation path unverifiable: the check could not even reach the negative case (an alias on the CloudFront zone id whose target is not a cloudfront.net name must be refused InvalidChangeBatch with `[Tried to create an alias that targets <target>, type A in zone Z2FDTNDATAQYW2, but the alias target name does not lie within the target zone]`), so that guard remains unexercised here.

Fix: the AliasTarget.DNSName projection in the route53 ListResourceRecordSets serializer (and the normalization applied when ChangeResourceRecordSets stores it) — store and return the submitted name, lowercased with a trailing dot, without presentation-format escaping.

Traces: cloudfront__route53.alias-record-set-requires-a-cloudfront-net-target-name

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check cloudfront__route53.alias-record-set-requires-a-cloudfront-net-target-name (check line 60):

  assert listed[0]["AliasTarget"]["DNSName"] == f"{dist_domain}."
  AssertionError: expected the stored alias target to echo the distribution's domain name with a trailing dot, got '127.0.0.1\\07234865.'

Flow: cloudfront CreateDistribution -> dist_domain = Distribution.DomainName; route53 CreateHostedZone; ChangeResourceRecordSets CREATE of an A alias at cdn.<zone> with AliasTarget {HostedZoneId: 'Z2FDTNDATAQYW2', DNSName: dist_domain, EvaluateTargetHealth: false}; ListResourceRecordSets(StartRecordName=<that name>, StartRecordType='A', MaxItems='1').
Expected: AliasTarget.DNSName == dist_domain + '.'
Actual: the same characters with ':' replaced by its DNS octal escape '\\072' ('127.0.0.1\\07234865.' for a submitted '127.0.0.1:34865').
The record's Name was listed correctly, so only the alias target member is affected.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. cloudfront CreateDistribution (disabled, one custom origin) and note Distribution.DomainName.
3. route53 CreateHostedZone(<tok>.test.); ChangeResourceRecordSets CREATE an A record at cdn.<tok>.test. with AliasTarget={'HostedZoneId':'Z2FDTNDATAQYW2','DNSName':<that DomainName>,'EvaluateTargetHealth':False}.
4. ListResourceRecordSets(HostedZoneId=<zone>, StartRecordName='cdn.<tok>.test.', StartRecordType='A', MaxItems='1') and compare AliasTarget.DNSName with the submitted DomainName + '.': the returned value is escaped in DNS presentation format and does not match.
5. Replay cloudfront__route53.alias-record-set-requires-a-cloudfront-net-target-name.
