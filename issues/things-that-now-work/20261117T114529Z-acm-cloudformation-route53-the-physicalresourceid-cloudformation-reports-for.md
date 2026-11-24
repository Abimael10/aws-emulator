Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone acm__cloudformation__route53.deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected
Issue-Key: acm__cloudformation__route53|DescribeCertificate|ValidationException
Issue-Commit: 14940b74adde738ffb545252b71aede81788f071

# acm__cloudformation__route53: the PhysicalResourceId CloudFormation reports for AWS::CertificateManager::Certificate is not an ACM certificate ARN, so DescribeCertificate on it is refused ValidationException

> _Note: this issue was identified against the code base at commit `14940b74adde738ffb545252b71aede81788f071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResources

## Description
Cause 1 of 1: the stack-resource row CloudFormation publishes for an AWS::CertificateManager::Certificate carries a PhysicalResourceId that is not the ACM certificate ARN the resource minted, so the identifier cannot be used against ACM.
AWS: recorded behavior only for the identifier's form, and Smithy pins the shape it must have — a CloudFormation Certificate resource's PhysicalResourceId (and its Ref) is the ACM certificate ARN returned by RequestCertificate; Smithy (acm) types RequestCertificateResponse.CertificateArn and DescribeCertificateRequest.CertificateArn as Arn with length 20..2048 and pattern ^arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*$. Consequently DescribeCertificate(CertificateArn=<PhysicalResourceId>) succeeds for a Certificate resource in any state, including a CREATE_FAILED one that minted the certificate before DNS validation failed.
Emulator: the value reported in StackResource.PhysicalResourceId does not satisfy ACM's certificateArn pattern; passing it straight to DescribeCertificate is refused with HTTP 400, Error.Code "ValidationException" and Message "1 validation error detected: Value of the input at 'certificateArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*" — fixed envelope text with the member name 'certificateArn' and the pattern taken from the model.
Why: every client that deploys a certificate through CloudFormation and then talks to ACM about it (read the DNS challenge, wait for ISSUED, attach it to a listener/distribution, delete it) is broken: the only handle CloudFormation gives it is not an ACM ARN. It also makes the whole DNS-validation-through-CloudFormation surface unobservable — the checks below cannot even get as far as asserting that the validation CNAME is written into the named hosted zone, or that the certificate disappears after stack delete.
Fix: the CloudFormation resource provider for AWS::CertificateManager::Certificate — its create path must set the resource's PhysicalResourceId (and Ref) to the ARN ACM's RequestCertificate returned, and keep it on the CREATE_FAILED row when DomainValidationOptions handling fails.
Traces: acm__cloudformation__route53.deploy-cfn-certificate-hostedzoneid-writes-validation-cname-into-the-named-zone, acm__cloudformation__route53.deploy-cfn-certificate-two-hosted-zones-each-get-their-own-validation-cname, acm__cloudformation__route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected.

## Evidence
Expected: cfn DescribeStackResources(StackName=<stack>, LogicalResourceId="Certificate")[0].PhysicalResourceId is an ACM certificate ARN, and acm DescribeCertificate(CertificateArn=<that id>) answers 200 with the certificate's DomainValidationOptions[0].ResourceRecord (the DNS challenge).
Actual, all three checks: botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the DescribeCertificate operation: 1 validation error detected: Value of the input at 'certificateArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*
The emulator therefore publishes a physical id that is not an acm ARN at all (the assertion that the id is non-empty passes).

## Steps to Reproduce
1. route53 CreateHostedZone for a domain <d>.
2. cloudformation CreateStack with a template whose only resource is Type AWS::CertificateManager::Certificate, Properties DomainName=<d>, ValidationMethod=DNS, DomainValidationOptions=[{DomainName: <d>, HostedZoneId: <zone id>}].
3. Poll cfn DescribeStackResources(LogicalResourceId="Certificate") until a PhysicalResourceId appears.
4. Call acm DescribeCertificate(CertificateArn=<that PhysicalResourceId>) — it is refused 400 ValidationException against the certificateArn pattern instead of returning the certificate. The same happens for the two-hosted-zone variant and for the negative variant whose HostedZoneId names a zone that does not cover the domain (where the id must survive on the CREATE_FAILED row).
