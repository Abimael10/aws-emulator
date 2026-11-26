Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone acm__cloudformation__route53.deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname
Issue-Key: acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected|run#22|AssertionError|assert mismatched_cert_arn, (
Issue-Key: acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone|_wait_for_stack_resource_physical_id#13|Timeout|time.sleep(interval_s)
Issue-Key: acm__cloudformation__route53.deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname|_until#16|Timeout|time.sleep(interval_s)
Issue-Commit: b4093aae6d988631adde420a6428194a1a0d4714

# acm__cloudformation__route53: 2 net-new causes — a CREATE_FAILED AWS::CertificateManager::Certificate row carries no PhysicalResourceId, and the DomainValidationOptions HostedZoneId leg never writes ACM's validation CNAME into the named hosted zone

> _Note: this issue was identified against the code base at commit `b4093aae6d988631adde420a6428194a1a0d4714`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResources

## Description
Two net-new root causes in the CloudFormation resource provider for AWS::CertificateManager::Certificate, both exercised through DomainValidationOptions[].HostedZoneId. Related but NOT covered by open issue 20261117T114529Z-acm-cloudformation-route53-the-physicalresourceid-cloudformation-reports-for.md (that one is about a PhysicalResourceId that is present but is not an ACM ARN; the suite does not mark these checks as owned by it, and the observed symptoms are now different — on the CREATE_FAILED row the id is absent entirely, and the two happy-path deploys no longer fail on DescribeCertificate at all but never converge).

Cause 1 of 2: a CREATE_FAILED Certificate resource row loses the identifier of the certificate it already minted.
AWS: recorded behavior only (CloudFormation does not model its resource providers in Smithy). ACM mints the certificate first and only then handles DomainValidationOptions, so when the named hosted zone does not cover DomainName the Route 53 RRSet write is refused and the resource goes CREATE_FAILED while the certificate exists; CloudFormation keeps that certificate's ARN as the resource's PhysicalResourceId on the CREATE_FAILED row (with DisableRollback=true the row stays readable), and that id is a usable ACM handle — Smithy (acm) types RequestCertificateResponse.CertificateArn and DescribeCertificateRequest.CertificateArn as Arn, @length 20..2048, @pattern ^arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*$. After the stack is deleted, DescribeCertificate on it is refused ResourceNotFoundException with message "Could not find certificate <certificateArn>." — fixed text "Could not find certificate " plus the ARN from the request, then a period.
Emulator: the stack and the resource both reach CREATE_FAILED as expected, but DescribeStackResources(StackName=<stack>, LogicalResourceId="Certificate")[0] has no PhysicalResourceId at all (the member is absent/None), so the minted certificate is unreachable and the whole downstream contract (the Route 53 RRSet-rejection ResourceStatusReason naming ACM's own challenge name and the rejecting zone, HandlerErrorCode GeneralServiceException, wrapped Route53 status 400, and the certificate's disappearance after stack delete) cannot be observed.
Why: a client that deploys a certificate through CloudFormation and hits a validation-zone misconfiguration is left with an orphaned certificate it has no handle for — it cannot read the failure, cannot describe or delete the certificate, and cannot assert that stack deletion cleaned it up.
Fix: the AWS::CertificateManager::Certificate resource provider's create path — it must set the resource's PhysicalResourceId (and Ref) to the ARN RequestCertificate returned as soon as the certificate is minted, before DomainValidationOptions handling runs, and must preserve it on the failure row rather than clearing it when the handler errors.
Traces: acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected.

Cause 2 of 2: a Certificate whose DomainValidationOptions name an owned HostedZoneId never gets its DNS validation CNAME written into that zone, so the deploy never converges.
AWS: recorded behavior only. When a DomainValidationOptions entry pairs a DomainName with a HostedZoneId the caller owns, CloudFormation writes ACM's DNS challenge into that hosted zone itself: for each domain, a CNAME record whose Name and single ResourceRecords[].Value are exactly the DomainValidationOptions[].ResourceRecord Name/Value DescribeCertificate publishes for that domain (Type CNAME), and each zone receives only its own domain's challenge. The certificate stays Status PENDING_VALIDATION while the challenge is outstanding, with one DomainValidationOptions entry per domain (ValidationMethod DNS).
Emulator: neither deploy reaches that state within the suite's budget — both checks are cut off at 60s with no assertion failure reported, i.e. one of the polls (the Certificate resource reporting a physical id, DescribeCertificate publishing a ResourceRecord per domain, or ListResourceRecordSets on the named zone showing the challenge CNAME) never becomes true. No validation CNAME is observable in the named zone, and the single-zone and two-zone (DomainName + SubjectAlternativeNames) shapes both stall.
Why: the documented CloudFormation path for automatic DNS validation is inert — a stack that should self-validate and settle on an ISSUED certificate hangs in create forever, and every dependent resource (listener certificate, custom domain, distribution) can never be deployed from a template.
Fix: the AWS::CertificateManager::Certificate resource provider — after RequestCertificate it must poll ACM for each domain's ResourceRecord and ChangeResourceRecordSets an UPSERT CNAME of that exact Name/Value into the HostedZoneId named for that domain (one write per DomainValidationOptions entry, into that entry's zone only), and must report the resource's progress/physical id so the create completes instead of stalling.
Traces: acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone, acm__cloudformation__route53.deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname.

## Evidence
Cause 1 — acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected:
Expected: DescribeStackResources(StackName=<stack>, LogicalResourceId="Certificate")[0] has ResourceStatus 'CREATE_FAILED' AND a PhysicalResourceId that is the ACM certificate ARN (matching ^arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*$), usable against acm DescribeCertificate; its ResourceStatusReason is the wrapped Route 53 refusal 'Resource handler returned message: "[RRSet with DNS name <acm-challenge-name> is not permitted in zone <other-zone-name>.] (Service: Route53, Status Code: 400, Request ID: <id>) (SDK Attempt Count: n)" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)' — fixed text as quoted, with the RRSet name taken from the certificate's own DomainValidationOptions[0].ResourceRecord.Name, the zone name from the named (non-covering) hosted zone, and the request id/token/attempt count per-run values; after stack delete, DescribeCertificate on that ARN is refused ResourceNotFoundException "Could not find certificate <arn>."
Actual: ResourceStatus is 'CREATE_FAILED' (that assertion passes) but the row carries no PhysicalResourceId —
  AssertionError: a CREATE_FAILED Certificate resource must still carry the PhysicalResourceId ACM minted before validation failed; got None

Causes 2 — acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone and .deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname:
Expected: the Certificate resource reports a physical id without CREATE_FAILED; DescribeCertificate publishes DomainValidationOptions with ValidationMethod 'DNS' and a ResourceRecord of Type 'CNAME' for every domain (Status 'PENDING_VALIDATION', SubjectAlternativeNames covering DomainName and each SAN); ListResourceRecordSets on each named hosted zone holds a CNAME at that domain's ResourceRecord.Name whose ResourceRecords values are exactly [that ResourceRecord.Value], and no zone holds the other domain's challenge.
Actual: both checks are terminated — 'Error: Check timed out after 60.0s' — with none of the above observable; the emulator never writes a validation CNAME into the named zone.

## Steps to Reproduce
Cause 1:
1. route53 CreateHostedZone for a generated domain <d> (covering zone) and for an unrelated generated domain <o> (non-covering zone).
2. cloudformation CreateStack with DisableRollback=true and a template whose only resource is Type AWS::CertificateManager::Certificate, Properties DomainName=<d>, ValidationMethod=DNS, DomainValidationOptions=[{DomainName: <d>, HostedZoneId: <zone id of o>}].
3. Poll DescribeStacks until a terminal status — it is CREATE_FAILED, as AWS does.
4. cloudformation DescribeStackResources(StackName=<stack>, LogicalResourceId="Certificate") — the row's ResourceStatus is CREATE_FAILED but PhysicalResourceId is absent, so the certificate ACM minted before the Route 53 write failed has no handle and neither the wrapped Route53 ResourceStatusReason nor the post-delete ResourceNotFoundException can be checked.

Cause 2:
1. route53 CreateHostedZone for a generated domain <d> (and, for the two-zone variant, a second zone for a generated SAN domain <s>).
2. cloudformation CreateStack with a template whose only resource is Type AWS::CertificateManager::Certificate, Properties DomainName=<d> (plus SubjectAlternativeNames [<s>]), ValidationMethod=DNS, DomainValidationOptions one entry per domain naming that domain's bare HostedZoneId.
3. Poll DescribeStackResources(LogicalResourceId="Certificate") for a physical id, then acm DescribeCertificate for each domain's DomainValidationOptions[].ResourceRecord.
4. route53 ListResourceRecordSets on each named hosted zone — the challenge CNAME ACM published is never written into the zone, and the deploy never converges (the check is cut off at 60s).
