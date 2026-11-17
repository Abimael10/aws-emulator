Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__route53__servicediscovery.deploy-private-dns-namespace-hosted-zone-delete-rejected
Issue-Key: route-53|DeleteHostedZone|HostedZoneNotEmpty
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudformation__route53__servicediscovery: DeleteHostedZone on a zone owned by a Cloud Map private DNS namespace is refused as HostedZoneNotEmpty instead of AccessDenied naming the namespace

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteHostedZone

## Description
Cause 1 of 1: a hosted zone created by an AWS Cloud Map private DNS namespace is not marked as Cloud-Map-managed, so Route 53 refuses a direct delete for the wrong reason.

AWS: Smithy: DeleteHostedZone binds HostedZoneNotEmpty (client, 400), InvalidDomainName (400), InvalidInput (400), NoSuchHostedZone (404) and PriorRequestNotComplete (400); the Cloud Map ownership rule is not in the model, so: recorded behavior only — a hosted zone whose lifecycle belongs to a servicediscovery private DNS namespace may only be deleted through Cloud Map, and a direct route53 DeleteHostedZone is refused with Code `AccessDenied` and Message of the fixed form `The resource hostedzone/<hosted zone id> can only be managed through AWS Cloud Map (<namespace ARN>)`. The fixed text is "The resource ", " can only be managed through AWS Cloud Map (", ")"; the hosted zone id and the namespace ARN are values from the request and the emulator's state (the ids in any single recording are per-run values, not part of the requirement). This refusal takes precedence over the zone's contents: it is the ownership, not the record count, that blocks the delete.

Emulator: the call is refused, but with Code `HostedZoneNotEmpty` and Message `The specified hosted zone contains resource record sets other than the default SOA and NS records and so cannot be deleted. Delete all non-default records and try again.` — i.e. the zone is treated as an ordinary zone that merely holds extra records, with no record of the Cloud Map namespace that owns it.

Why: the two refusals tell a client opposite things. AccessDenied means "delete the namespace instead"; HostedZoneNotEmpty means "empty the zone and retry" — advice that, followed, deletes the service-discovery records Cloud Map manages and still leaves the delete refused. Automation that branches on the code (or surfaces the namespace ARN from the message to point the operator at the right API) takes the wrong branch, and the Cloud Map ownership link is invisible.

Fix: the ownership gate in the DeleteHostedZone handler, plus the state that records which servicediscovery namespace owns a hosted zone when a private DNS namespace creates it — the gate must run before the non-empty check and raise AccessDenied carrying the owning namespace's ARN.

Traces: cloudformation__route53__servicediscovery.deploy-private-dns-namespace-hosted-zone-delete-rejected

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check cloudformation__route53__servicediscovery.deploy-private-dns-namespace-hosted-zone-delete-rejected (check line 164, expect_error on r53.delete_hosted_zone(Id=zone_id)):

  AssertionError: expected ('AccessDenied', 'The resource hostedzone/Z3BBTVNKI2YQA0G can only be managed through AWS Cloud Map (arn:aws:servicediscovery:us-west-2:000000000000:namespace/ns-b05qrc28vsqq4sew)'), got ('HostedZoneNotEmpty', 'The specified hosted zone contains resource record sets other than the default SOA and NS records and so cannot be deleted. Delete all non-default records and try again.')

Expected (AWS): 400 AccessDenied, Message = 'The resource hostedzone/<zone id> can only be managed through AWS Cloud Map (<namespace arn>)'.
Actual (emulator): 400 HostedZoneNotEmpty with the generic non-empty-zone sentence; the zone id and namespace ARN above are that run's values.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Deploy a stack (or call servicediscovery directly) creating an AWS::ServiceDiscovery::PrivateDnsNamespace in a VPC, and note the namespace ARN and the hosted zone id it creates.
3. Call route53 DeleteHostedZone(Id=<that hosted zone id>) directly.
4. Observe Error.Code == 'HostedZoneNotEmpty' with the generic non-empty message, instead of 'AccessDenied' with 'The resource hostedzone/<zone id> can only be managed through AWS Cloud Map (<namespace arn>)'.
5. Replay cloudformation__route53__servicediscovery.deploy-private-dns-namespace-hosted-zone-delete-rejected.
