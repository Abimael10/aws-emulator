Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: datazone.subscription-request-accept-creates-approved-subscription datazone.revoke-subscription-transitions-to-revoked
Issue-Shape-Id: com.amazonaws.datazone#DataZone
Issue-Key: datazone.subscription-request-accept-creates-approved-subscription|run#9|AssertionError|assert request["status"] == "PENDING", f"expected PENDING, got {request['status']!r}"
Issue-Key: datazone|AcceptSubscriptionRequest|ValidationException
Issue-Commit: 2f916589b2ea3e738a5dcf2ef5358049c7617ada

# datazone: CreateSubscriptionRequest resolves a consumer's request to ACCEPTED at create time instead of leaving it PENDING, so AcceptSubscriptionRequest/RevokeSubscription are unreachable

> _Note: this issue was identified against the code base at commit `2f916589b2ea3e738a5dcf2ef5358049c7617ada`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSubscriptionRequest

## Description
1 net-new root cause in datazone's subscription-request state machine.

Cause 1 of 1: CreateSubscriptionRequest decides the created request itself, on the wrong condition, so a request that AWS leaves awaiting a decision comes back already decided and every later decision call is refused.

AWS: Smithy — CreateSubscriptionRequest returns required `status` of enum SubscriptionRequestStatus (PENDING | ACCEPTED | REJECTED); AcceptSubscriptionRequest (PUT /v2/domains/{domainIdentifier}/subscription-requests/{identifier}/accept) takes the decision and returns the request with status ACCEPTED; RevokeSubscription (PUT /v2/domains/{domainIdentifier}/subscriptions/{identifier}/revoke) returns required `status` of SubscriptionStatus. The model does not state when a created request is already decided, so for that part this is recorded behavior only: in both recordings a subscription request created by a consumer-only IAM identity — a principal that is NOT an owner of the project that published the listing — comes back with status PENDING, and the subsequent AcceptSubscriptionRequest succeeds with status ACCEPTED and materialises an APPROVED Subscription that RevokeSubscription then moves to REVOKED. DataZone auto-resolves a request only when the requester is already an owner of the PRODUCING project (the checks' own docstrings state this is why they request from a consumer-only identity); it does not auto-resolve based on whether the subscribed listing has a subscription target behind it.

Emulator: CreateSubscriptionRequest answers HTTP 200 with status 'ACCEPTED' for that consumer-only request (check subscription-request-accept-creates-approved-subscription: "AssertionError: expected PENDING, got 'ACCEPTED'"). The condition it uses is not the requester's relationship to the producing project at all: services/service-com-amazonaws-datazone-datazone/src/state/subscription_requests.rs:51-68 (`created_subscription_request_status`) returns PENDING only when a subscribed listing revision resolves to a fulfilling subscription target and ACCEPTED otherwise, and create_subscription_request.rs:366 stores that verdict. Because a published asset listing in these flows has no subscription target behind it, every such request is born ACCEPTED. The follow-on call is then refused: AcceptSubscriptionRequest's guard (`SubscriptionRequest::awaits_a_decision`, subscription_requests.rs:91; accept_subscription_request.rs:236-243) answers HTTP 400 ValidationException with the fixed text "The approved subscription is already revoked or unsubscribed." (constant SUBSCRIPTION_ALREADY_DECIDED_MESSAGE, subscription_requests.rs:74-75) — which is what check revoke-subscription-transitions-to-revoked dies on, before it can ever reach RevokeSubscription.

Why: a client that implements the documented DataZone approval flow cannot run it against this emulator. The create response reports a decision no reviewer took (reviewerId/decisionComment absent, yet status terminal), so an approval UI or workflow sees the ask as already granted; the approving call it then makes is refused 400 ValidationException instead of returning the request as ACCEPTED, no Subscription is materialised for the request (ListSubscriptions by subscriptionRequestIdentifier stays empty), and RevokeSubscription — and therefore the whole APPROVED -> REVOKED transition — is unreachable. The refusal message makes it worse: it talks about an approved subscription being revoked or unsubscribed, naming neither the subscription request nor the status it actually carries, so a client cannot tell why the accept failed.

Fix: the create-time status rule in DataZone's subscription-request state (`created_subscription_request_status` in the service's state/subscription_requests module, consumed by the CreateSubscriptionRequest handler). A request whose principal is not an owner of the producing project must be stored and reported PENDING — the presence or absence of a subscription target for the listing is not what decides it — so that the AcceptSubscriptionRequest handler's awaits-a-decision guard admits the decision and the subscription it creates can be read APPROVED and then revoked.

Traces: checks subscription-request-accept-creates-approved-subscription (mismatch on the create's status) and revoke-subscription-transitions-to-revoked (errors on the accept the ACCEPTED-at-create request refuses).

## Evidence
Check datazone.subscription-request-accept-creates-approved-subscription [mismatch]:
  request = CreateSubscriptionRequest(domainIdentifier=<domain>, subscribedPrincipals=[{project:{identifier:<consumer project>}}], subscribedListings=[{identifier:<asset listing>}], requestReason=...), issued by a consumer-only IAM identity.
  Expected (AWS, recorded): HTTP 200, status == "PENDING"; then AcceptSubscriptionRequest -> 200 status "ACCEPTED"; ListSubscriptions(subscriptionRequestIdentifier=<request id>) -> exactly one item; GetSubscription -> status "APPROVED".
  Actual (emulator): HTTP 200 with status "ACCEPTED".
  Assertion: AssertionError: expected PENDING, got 'ACCEPTED'

Check datazone.revoke-subscription-transitions-to-revoked [error]:
  Same create, then AcceptSubscriptionRequest(domainIdentifier=<domain>, identifier=<request id>).
  Expected (AWS, recorded): HTTP 200, request status "ACCEPTED", a Subscription created APPROVED, then RevokeSubscription -> 200 status "REVOKED" and GetSubscription -> "REVOKED".
  Actual (emulator): HTTP 400 botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the AcceptSubscriptionRequest operation: The approved subscription is already revoked or unsubscribed.
  That message is entirely fixed text in the emulator (no part is taken from the request or the store): it names neither the subscription request identifier nor the status the request carries, and it describes a revoked/unsubscribed subscription rather than an already-decided request.

Code path (citations only):
  services/service-com-amazonaws-datazone-datazone/src/state/subscription_requests.rs:51-68 — created_subscription_request_status gates PENDING on fulfilling_subscription_target
  services/service-com-amazonaws-datazone-datazone/src/ops/create_subscription_request.rs:366 — that verdict is stored as the request's status
  services/service-com-amazonaws-datazone-datazone/src/state/subscription_requests.rs:74-75, 91 — SUBSCRIPTION_ALREADY_DECIDED_MESSAGE and awaits_a_decision
  services/service-com-amazonaws-datazone-datazone/src/ops/accept_subscription_request.rs:236-243 — the 400 ValidationException refusal

## Steps to Reproduce
1. CreateDomain and wait for AVAILABLE; create two projects in it (a producer and a consumer).
2. Publish an asset listing owned by the producer project (create form type + asset, then a PUBLISH listing change set). Do not create a subscription target for it.
3. Using an IAM identity that is a member of the CONSUMER project only (not an owner of the producer project), call CreateSubscriptionRequest(domainIdentifier=<domain>, subscribedPrincipals=[{project:{identifier:<consumer project id>}}], subscribedListings=[{identifier:<listing id>}], requestReason="...").
4. Observe the response `status`: the emulator returns "ACCEPTED"; AWS returns "PENDING".
5. Call AcceptSubscriptionRequest(domainIdentifier=<domain>, identifier=<request id>). The emulator answers HTTP 400 ValidationException "The approved subscription is already revoked or unsubscribed."; AWS answers 200 with status "ACCEPTED".
6. Poll ListSubscriptions(domainIdentifier=<domain>, subscriptionRequestIdentifier=<request id>): no subscription is ever created in the emulator, so GetSubscription/RevokeSubscription (APPROVED -> REVOKED) cannot be reached.
