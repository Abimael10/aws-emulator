Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found
Issue-Key: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found|_assert_serves#26|PublishedUrlRejected|url, headers = ctx.endpoint_route(domain)
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudformation__cloudfront__s3: a CloudFormation-provisioned distribution publishes DomainName as a <id>.cloudfront.net host the emulator does not serve, so the root-object edge round trip never reaches a verdict

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause.

Cause 1 of 1: the DomainName an AWS::CloudFront::Distribution publishes (and that CloudFront's own read-back reports) is a <id>.cloudfront.net host that is not an address the emulator answers on, so no viewer request can ever be made through the distribution's edge and every assertion downstream of the first edge GET is unreachable.

AWS: Smithy (cloudfront, 2020-05-31) Distribution.DomainName is documented as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." CloudFormation's AWS::CloudFront::Distribution exposes the same value as the Fn::GetAtt DomainName attribute. On AWS that host is a real, resolvable address that serves viewer traffic: once the distribution is Deployed, an HTTPS GET of "/" against it returns the bucket object named by DistributionConfig.DefaultRootObject, fetched from the S3 origin under the distribution's origin access control, while an unsigned GET straight at the bucket is refused 403 AccessDenied / "Access Denied". The form of the name (the generated <id> label and the cloudfront.net suffix) is AWS's; the requirement an emulator must meet is that the announced address is one the emulator itself serves viewer traffic on, because that announced address is the only handle a client has on the edge.

Emulator: the stack reaches CREATE_COMPLETE and its DomainName output is published in AWS's shape, e.g. a <id>.cloudfront.net host, but that host is not one of the emulator's own hosts and nothing listens for it. The replay is cut off at the first viewer request with simparity.published.PublishedUrlRejected: "endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". (The host, port and generated distribution id differ per run and are not part of the requirement.) Because the announcement is unusable, none of the rest of the scenario runs: the first DefaultRootObject fetch, the 403 AccessDenied / "Access Denied" refusal of the unsigned direct S3 read, the in-place Comment + DefaultRootObject update that must keep the same physical distribution id and then serve the second root object, and the post-delete GetDistribution refusal 404 NoSuchDistribution / "The specified distribution does not exist." are all never exercised.

Why: a client that deploys this stack and then drives traffic through the distribution — exactly what the Fn::GetAtt DomainName output exists for — is told an address it cannot reach. Every CDN behaviour a caller depends on (default root object, origin access control on a private bucket, cache behaviour, update-in-place semantics) is unobservable, and a caller cannot distinguish "edge not configured" from "edge not addressable": it only sees connection failures at a name the emulator itself handed it.

Fix: in CloudFront's distribution state/serializer, the DomainName minted by CreateDistribution (and reported by GetDistribution/GetDistributionConfig and relayed by the AWS::CloudFront::Distribution resource handler into Ref/GetAtt and stack Outputs) must be an address the emulator serves, and a viewer listener for that distribution must answer GETs by fetching from the configured origin and applying DefaultRootObject and the default cache behaviour. The CloudFormation side is only a relay: it must publish whatever addressable name CloudFront mints, not rewrite it.

Related open issues describe the same class of defect for other service groups and do NOT cover this check: 20261125T091314Z-cloudfront-2-net-new-causes-a-distribution-s.md, 20261125T052443Z-cloudfront-s3-the-distribution-s-published-domainname-resolves.md, 20261125T052443Z-apigateway-cloudfront-s3-the-published-distribution-domainname-is.md, and 20261118T131652Z-cloudformation-cloudfront-s3-an-aws-s3-bucket-s.md (which is about AWS::S3::Bucket RegionalDomainName, a different attribute). 20261113T232314Z-cloudformation-cloudfront-s3-a-cloudformation-provisioned-cloudfront-distribution.md names the same scenario but as "never serves its S3 origin's DefaultRootObject"; the replay now stops earlier, at the announced address itself.

Traces: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found (verdict: mismatch).

## Evidence
Check: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found [mismatch]

Expected (AWS): the stack output DomainName (Fn::GetAtt Distribution.DomainName) is an address viewer traffic can be sent to. An HTTPS GET of "/" there returns the body of the object named by DefaultRootObject (first.html, then second.html after the in-place update); an unsigned GET of the same object straight at the bucket's RegionalDomainName is refused with HTTP 403 and S3 error Code "AccessDenied", Message "Access Denied"; after the stack delete, cloudfront GetDistribution(Id=<id>) is refused 404 with Code "NoSuchDistribution", Message "The specified distribution does not exist." The in-place update keeps the distribution's physical id and leaves every stack output byte-identical.

Actual (emulator): CREATE_COMPLETE is reached and DomainName is published as a <id>.cloudfront.net host (observed: https://dgzni4pnd7h2s3.cloudfront.net — the id and host are per-run values, not a requirement). The first viewer request aborts:

  File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
  File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(...)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:32941/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Nothing after that announcement is exercised: the DefaultRootObject fetch, the 403 AccessDenied direct-read refusal, the in-place update's same-physical-id and second-root-object assertions, and the post-delete NoSuchDistribution refusal are all unreached.

## Steps to Reproduce
1. CreateStack with a template containing an AWS::S3::Bucket (SSE-S3, full PublicAccessBlockConfiguration), an AWS::CloudFront::OriginAccessControl (OriginAccessControlOriginType s3, SigningBehavior always, SigningProtocol sigv4), an AWS::CloudFront::Distribution whose DistributionConfig sets Enabled true, DefaultRootObject first.html, PriceClass PriceClass_100, one Origin with DomainName = Fn::GetAtt [Bucket, RegionalDomainName] and OriginAccessControlId = Fn::GetAtt [Oac, Id], and a DefaultCacheBehavior with the managed Managed-CachingDisabled CachePolicyId and AllowedMethods [GET, HEAD], plus an AWS::S3::BucketPolicy granting cloudfront.amazonaws.com s3:GetObject conditioned on AWS:SourceArn of this distribution. Export Outputs for Ref Distribution, Fn::GetAtt Distribution.Id, Fn::GetAtt Distribution.DomainName, Ref Bucket and Fn::GetAtt Bucket.RegionalDomainName.
2. Wait for CREATE_COMPLETE; PutObject first.html and second.html into the bucket.
3. Read the DomainName stack output and issue an HTTPS GET of "/" against it. Observe that the host announced is a <id>.cloudfront.net name the emulator does not serve, so the request cannot be addressed to the emulator at all (PublishedUrlRejected); on AWS this returns the body of first.html.
4. (Unreached today) UpdateStack with a new Comment and DefaultRootObject second.html, confirm the physical distribution id and the stack outputs are unchanged, and GET "/" again expecting second.html.
5. (Unreached today) Empty the bucket, DeleteStack, then GetDistribution(Id=<id>) and expect 404 NoSuchDistribution / "The specified distribution does not exist."
