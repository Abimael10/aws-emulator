Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content
Issue-Key: route-53|ChangeResourceRecordSets|InvalidChangeBatch
Issue-Commit: 42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35

# acm__cloudfront__iam__lambda__route53__s3: a distribution's published DomainName is not a DNS host name, so the documented Route 53 alias to a CloudFront distribution is refused InvalidChangeBatch

> _Note: this issue was identified against the code base at commit `42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause, grounded in the behavioral check acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content, which errors out at its first Route 53 call and therefore never reaches any of its four graded stages (authoritative A/AAAA answers for the alias, the MissingKey 403 for an unsigned viewer request, the signed-URL 200 from the private S3 origin, and ACM InUseBy naming the distribution).

Cause 1 of 1: CreateDistribution publishes Distribution.DomainName as an IP literal with a URL PATH appended, which is not a domain name, so a Route 53 alias record that targets the distribution can never be created.
AWS: CreateDistribution's 201 response carries Distribution.DomainName = the distribution's CloudFront domain name, a DNS host name (Smithy: cloudfront Distribution.DomainName, required - "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net"). Route 53 then accepts an A (and AAAA) alias record whose AliasTarget is {HostedZoneId: Z2FDTNDATAQYW2 (CloudFront's fixed alias hosted-zone id), DNSName: that domain name} (Smithy: route53 AliasTarget.DNSName - for a "CloudFront distribution" you "Specify the domain name that CloudFront assigned when you created your distribution"), and the ChangeResourceRecordSets returns ChangeInfo with a PENDING/INSYNC status. AWS's own zone-containment check on an alias target only refuses a DNSName that does not lie inside the target zone; a <id>.cloudfront.net name does lie inside Z2FDTNDATAQYW2, so a correctly minted DomainName is never refused.
Emulator: CreateDistribution returns a DomainName of the form "<ip-literal>/_aws/cloudfront/<distribution-id>" - an address plus a URL path, with no DNS label structure at all (the recorded run shows 127.0.0.1/_aws/cloudfront/e89fkddmx3r403; the host, port and id are per-run values, the shape is the defect). Feeding that value back as the AliasTarget.DNSName the model prescribes, route53 refuses the change with HTTP 400 and error code InvalidChangeBatch, message "[Tried to create an alias that targets 127.0.0.1/_aws/cloudfront/e89fkddmx3r403., type A in zone Z2FDTNDATAQYW2, but the alias target name does not lie within the target zone]". In that message the fixed text is AWS's own alias-containment wording ("Tried to create an alias that targets " / ", type " / " in zone " / ", but the alias target name does not lie within the target zone"); the variable parts are the submitted DNSName, the record Type and the AliasTarget.HostedZoneId - all taken from the request. The refusal wording itself is right; it fires because the name CloudFront handed the caller is not a host name, so the containment test can never pass.
Why: DomainName is the single value a client uses to address or alias a distribution - as a Route 53 alias target, as a CNAME value, as a TLS SNI/Host name, or as an assertion on the *.cloudfront.net form. A value carrying a path segment is rejected by every one of those consumers, so the standard "create distribution with an alternate domain name, then point Route 53 at it" workflow (CNAMEs.html, routing-to-cloudfront-distribution.html) is unreachable in the emulator: the alias CREATE is refused before any edge behaviour can be exercised, and the whole ACM-certificate / signed-URL / OAC data path behind it is untestable.
Fix: the distribution domain-name derivation in the cloudfront service's state plus CreateDistribution's (and CreateDistributionWithTags'/GetDistribution's) response construction - mint a single-label-plus-cloudfront.net DNS name for the distribution and keep the emulator's own address out of the member; the route53 alias-target containment check must then accept that name under hosted zone Z2FDTNDATAQYW2. Note the complementary requirement: the emulator must also ANSWER on the name it publishes, since the check connects to it with SNI and Host set to the alternate domain name.
Related but not covering: the open issue 20261114T142904Z-cloudfront-reopens-generated-identifiers-and-published-domain-names.md records the same DomainName-form fault from the WIRE corpus (there the emitted value was a bare "127.0.0.1:<port>"); it carries no behavioral check id and does not record this downstream Route 53 InvalidChangeBatch consequence, so it does not own this failure. No other open cloudfront/route53 issue carries this check.
Traces: acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1): 263/283 passed; this check is reported as [error] under the heading '### acm__cloudfront__iam__lambda__route53__s3' and is the only failing check in the report not marked ALREADY FILED or OWNED by.

Failing stack (tail), raised from the check's _change() helper on its first ChangeResourceRecordSets for the A alias:
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.InvalidChangeBatch: An error occurred (InvalidChangeBatch) when calling the ChangeResourceRecordSets operation: [Tried to create an alias that targets 127.0.0.1/_aws/cloudfront/e89fkddmx3r403., type A in zone Z2FDTNDATAQYW2, but the alias target name does not lie within the target zone]

Expected: the CREATE of {Name: <label>.<baseline apex>, Type: A, AliasTarget: {HostedZoneId: Z2FDTNDATAQYW2, DNSName: <the distribution's DomainName>, EvaluateTargetHealth: false}} is accepted (200, ChangeInfo.Id present, reaching INSYNC), and the same for Type AAAA.
Actual: 400 InvalidChangeBatch, because the DNSName the emulator's CreateDistribution published for the distribution - '127.0.0.1/_aws/cloudfront/e89fkddmx3r403' - is an address plus a URL path rather than a DNS host name, and so cannot lie within any hosted zone. Smithy requires that member to be a domain name ('The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net').

Because the alias CREATE aborts the check, four later assertions were never reached: (1) every delegated name server answering the alias as A and AAAA records under the aliased name with aa=1/NOERROR, (2) an unsigned GET on the alternate name answered 403 with <Code>MissingKey</code> over a certificate whose SAN/CN is exactly that name, (3) a canned-policy signed URL returning the private S3 object's bytes through the OAC, and (4) DescribeCertificate InUseBy == [the distribution ARN].

## Steps to Reproduce
1. Clone the repo, build and boot the emulator, and replay the behavioral suite (simparity-behavioral-aws-v1).
2. Or reproduce directly: pick the public hosted zone tagged sim2real-infra-dns-baseline (apex A, zone id Z) and request+validate an ACM certificate (us-east-1) for a fresh label under A.
3. cloudfront.CreateDistribution with Aliases={Quantity:1, Items:[<label>.A]}, ViewerCertificate={ACMCertificateArn:<cert>, SSLSupportMethod:'sni-only', MinimumProtocolVersion:'TLSv1.2_2021'}, one S3 origin with an OriginAccessControlId, and DefaultCacheBehavior.TrustedKeyGroups enabled.
4. Read Distribution.DomainName from the 201 response: it is '<ip>/_aws/cloudfront/<distribution id>' - it contains a '/' and has no DNS labels, so it is not a domain name.
5. route53.ChangeResourceRecordSets on zone Z with Changes=[{Action:'CREATE', ResourceRecordSet:{Name:'<label>.A', Type:'A', AliasTarget:{HostedZoneId:'Z2FDTNDATAQYW2', DNSName:<that DomainName>, EvaluateTargetHealth:false}}}]. AWS accepts this; the emulator answers 400 InvalidChangeBatch '[Tried to create an alias that targets <DNSName>., type A in zone Z2FDTNDATAQYW2, but the alias target name does not lie within the target zone]'. Repeat with Type 'AAAA' for the same refusal.
6. Confirm the fix by re-replaying acm__cloudfront__iam__lambda__route53__s3.cloudfront-alternate-domain-serves-its-acm-certificate-and-signed-content: DomainName must be a '<id>.cloudfront.net'-form name, both alias CREATEs must reach INSYNC, and the check must then proceed past the alias stage.
