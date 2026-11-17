Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found network-security-manager.list-templates-invalid-status-filter-rejected
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found|run#5|AssertionError|assert code == "ResourceNotFoundException", (
Issue-Key: network-security-manager.list-templates-invalid-status-filter-rejected|run#4|AssertionError|assert message == (
Issue-Commit: 5b447d105eda88a360bfeb63d30f3c9a9053d9de

# network-security-manager read/filter surface diverges: 2 causes — GetDeployment answers 501 instead of ResourceNotFoundException, enum-value-set ValidationException message ordered wrongly

> _Note: this issue was identified against the code base at commit `5b447d105eda88a360bfeb63d30f3c9a9053d9de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
Two distinct, net-new defects in the Network Security Manager (PiccoloCustomerAPIService, restJson1) read/filter surface, both observed replaying the simparity-behavioral-aws-v1 suite against the emulator. Neither is covered by an open issue: '20261109T113448Z-network-security-manager-createrule-stub.md' owns only CreateRule, and '20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md' owns only the CreatePolicy/CreateRule/CreateScope entity-creation causes (its Cause 2 is the @pattern anchor rendering, a different renderer clause than Cause 2 below). The replay reports no stub owner and no owning issue for either check here.

=== Cause 1 of 2: GetDeployment is not served — HTTP 501 NotImplemented instead of the modeled ResourceNotFoundException for an unknown deployment ===
AWS: Smithy model com.amazonaws.networksecuritymanager#GetDeployment — GET /deployments/{deploymentIdentifier}, success @http code 200, input member deploymentIdentifier (@httpLabel, @required, DeploymentIdentifier @length 1..1010). Its modeled errors are AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429) and ValidationException (400); 501/NotImplemented is not among them. ResourceNotFoundException is documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again." and carries a @required `message` plus optional `resourceId`/`resourceType`. For a well-formed deployment ARN that names no existing deployment, the recorded AWS response is HTTP 404 with Error.Code "ResourceNotFoundException" and Error.Message "The requested resource was not found." — that message is entirely fixed text for this error class (no request- or state-derived parts); only the resourceId/resourceType members, when populated, carry request-derived values. The same read path on success must return 200 with GetDeploymentOutput's required members (deploymentId, deploymentArn, deploymentName, status, associatedPolicyList, associatedScopeList, version).
Emulator: `services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_deployment.rs` is an unmodified scaffold — its handler body is `Err(crate::scaffolded::Scaffolded::not_implemented("GetDeployment"))`, so every call answers HTTP 501 with Error.Code "NotImplemented". The replayed check got code 'NotImplemented' where ResourceNotFoundException was required.
Why: a client cannot read a deployment at all, and its not-found path never runs: an SDK sees a protocol-level 501 that no NSM error shape models, so `except nsm.exceptions.ResourceNotFoundException` does not catch it and retry/404-handling logic built on AWS's contract is dead code. Existence probes ("does this deployment exist?") cannot be expressed.
Fix: implement the GetDeployment handler and its state lookup (resolve deploymentIdentifier — ARN or id — against the deployment entity store; return the 200 output on a hit, and the modeled ResourceNotFoundException with AWS's fixed message on a miss), then delete that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.
Traces: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found.

=== Cause 2 of 2: the enum-constraint ValidationException message lists the allowed values in the model's declaration order instead of AWS's rendering order ===
AWS: Smithy model shape EntityStatusFilter — enum, declared values ACTIVE | DRAFT | DISABLED; ListTemplatesInput.status targets it as @httpQuery("status"). ListTemplates models ValidationException (@error client, @httpError 400, @required message). For a status value outside the enum, the recorded AWS response is HTTP 400 with Error.Code "ValidationException" and the standard constraint report as Error.Message: "1 validation error detected: Value '<rejected value>' at '<memberName>' failed to satisfy constraint: Member must satisfy enum value set: [<allowed values>]". Fixed text: "N validation error(s) detected: Value '...' at '...' failed to satisfy constraint: Member must satisfy enum value set: [...]"; variable parts are the error count, the rejected value and the member name (both taken from the request) and the allowed-value list (taken from the model). Recorded behavior only for the ORDER of that list: AWS renders it "[DRAFT, ACTIVE, DISABLED]", which is not the model's declaration order, so the renderer cannot simply emit declaration order. Hypothesis: AWS emits the enum in its service-internal/alphabetical-by-definition order rather than the published model order.
Emulator: status 400 and code ValidationException are correct, but the message is "1 validation error detected: Value 'BOGUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [ACTIVE, DRAFT, DISABLED]" — the bracketed list is in the model's declaration order, so the message text differs from AWS's for every @enum-constrained member in this service.
Why: the error message is part of the contract. A client, test or operator-facing surface that matches or displays AWS's constraint text sees a different string, and because the defect is in the shared enum-constraint message renderer (reached by the centralized deserialize→validate walk, before any handler runs) it mis-words every out-of-enum refusal in the service, not just ListTemplates.status.
Fix: the enum-constraint clause of the shared constraint-violation message renderer used by the central input-validation stage (the same renderer whose @pattern clause is faulted by Cause 2 of 20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md) must emit the allowed-value list in AWS's order rather than the model's declaration order.
Traces: network-security-manager.list-templates-invalid-status-filter-rejected.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout. Per-service: network-security-manager 0/9. Both failures below are verdict [mismatch], service heading `### network-security-manager`, and neither is marked ALREADY FILED nor OWNED by an open issue.

Cause 1 — check network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found:
  check body: fake_arn = f"arn:aws:network-security-manager:{ctx.region}:{ctx.account}:deployment:{fake_id}"; nsm.get_deployment(deploymentIdentifier=fake_arn)
  assertion: `assert code == "ResourceNotFoundException"`
  AssertionError: GetDeployment for an unknown identifier gave 'NotImplemented', expected ResourceNotFoundException
  (the check further requires Error.Message == "The requested resource was not found.")
  handler: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_deployment.rs — line 4 carries `//! SCAFFOLDED, NOT IMPLEMENTED:`; line 198 is `Err(crate::scaffolded::Scaffolded::not_implemented("GetDeployment"))`.
  model: GetDeployment GET /deployments/{deploymentIdentifier} code 200; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400 — no 501.

Cause 2 — check network-security-manager.list-templates-invalid-status-filter-rejected:
  call: nsm.list_templates(status="BOGUS")
  expected Error.Code ValidationException (matched) and Error.Message
    "1 validation error detected: Value 'BOGUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [DRAFT, ACTIVE, DISABLED]"
  actual Error.Message
    "1 validation error detected: Value 'BOGUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [ACTIVE, DRAFT, DISABLED]"
  AssertionError: unexpected message "...[ACTIVE, DRAFT, DISABLED]"
  difference is exactly the ordering of the bracketed allowed-value list; everything else in the message matches.
  model: EntityStatusFilter enum values [ACTIVE, DRAFT, DISABLED]; ListTemplatesInput.status @httpQuery("status"); ListTemplates errors include ValidationException @httpError 400.

## Steps to Reproduce
Boot the network-security-manager emulator from this checkout and replay the two checks, or exercise them directly:

Cause 1: GET /deployments/arn%3Aaws%3Anetwork-security-manager%3A<region>%3A<account>%3Adeployment%3A<an id never created> (botocore: nsm.get_deployment(deploymentIdentifier=that ARN)). Expect HTTP 404, Error.Code ResourceNotFoundException, Error.Message "The requested resource was not found."; observe HTTP 501 with Error.Code NotImplemented. Replay check network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found; after the fix its follow-up ListDeployments paginator read must also succeed and must not surface the never-created id.

Cause 2: GET /templates?status=BOGUS (botocore: nsm.list_templates(status="BOGUS")). Expect HTTP 400 ValidationException with message "1 validation error detected: Value 'BOGUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [DRAFT, ACTIVE, DISABLED]"; observe the same message with the list ordered [ACTIVE, DRAFT, DISABLED]. Replay check network-security-manager.list-templates-invalid-status-filter-rejected; its control call nsm.list_templates(status="ACTIVE") must then return a `templates` list.
