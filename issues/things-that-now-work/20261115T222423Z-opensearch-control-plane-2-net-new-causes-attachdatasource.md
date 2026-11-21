Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.attach_data_source_duplicate_rejected opensearch.dependency-failure-on-update-index opensearch.overly-permissive-update-index-disabled-feature-blocked
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.attach_data_source_duplicate_rejected|run#7|AssertionError|assert code == "ResourceNotFoundException", f"expected ResourceNotFoundException, got {code}"
Issue-Key: opensearch.dependency-failure-on-update-index|run#9|AssertionError|assert (refusal_code, refusal_message) == ("ValidationException", (
Issue-Key: opensearch.overly-permissive-update-index-disabled-feature-blocked|run#11|AssertionError|assert update_message == gated, f"UpdateIndex ValidationException message {update_message!r}"
Issue-Commit: 0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764

# opensearch control plane: 2 net-new causes — AttachDataSource reports a missing application as ValidationException instead of ResourceNotFoundException, and the UpdateIndex/GetIndex software-version gate uses the wrong ValidationException message

> _Note: this issue was identified against the code base at commit `0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachDataSource

## Description
Two net-new root causes on the OpenSearch Service control plane. Open issue 20261114T222138Z-opensearch-4-net-new-causes-createindex-accepts-a.md covers neighbouring application/data-source refusals but carries neither check id below.

Cause 1 of 2: AttachDataSource against an application id that does not exist is refused with the wrong error shape.
AWS: Smithy — AttachDataSource (POST /2021-01-01/opensearch/application/{id}/attachDataSource) models both ResourceNotFoundException (errorKind client, httpError 409) and ValidationException (400); a well-formed `id` that matches the member's pattern ^[a-z0-9]{3,30}$ but names no application is a missing resource, not an invalid request, so AWS answers ResourceNotFoundException with message "Application <id> not found for account <account-id>" — fixed text "Application " / " not found for account ", with the id taken from the request path and the account id of the caller.
Emulator: answers ValidationException. The code (and therefore the HTTP status, 400 instead of 409) and the message both diverge; the request was otherwise valid and the positive control (attaching to a real application, settling at ATTACHED) is what the same handler must keep doing.
Why: clients branch on the error code — ResourceNotFoundException means "create the application first" and is non-retryable-but-recoverable, while ValidationException means "the request itself is wrong". Provisioning code that catches ResourceNotFoundException to create-then-attach never fires, and the message it logs does not name the account the lookup was made in.
Fix: the AttachDataSource handler's application lookup in the opensearch service crate — an unknown application id must raise the modeled ResourceNotFoundException with AWS's message, leaving ValidationException for input that violates the modeled constraints.
Traces: opensearch.attach_data_source_duplicate_rejected.

Cause 2 of 2: the UpdateIndex / GetIndex software-version feature gate refuses with ValidationException but with emulator-authored messages.
AWS: Smithy — UpdateIndex (PUT /2021-01-01/opensearch/domain/{DomainName}/index/{IndexName}) and GetIndex (GET, same path) both model ValidationException (400) and DependencyFailureException (424). On a domain whose engine does not offer index management (an OpenSearch_2.11 domain and an Elasticsearch_7.10 domain were both recorded), each call is refused with ValidationException whose message is exactly "Automatic Semantic Enrichment feature is not supported on this software version." — the message is entirely fixed text, with no value interpolated from the request or state; on a domain on the latest version the same call instead fails with DependencyFailureException.
Emulator: the code is ValidationException, but the message is one of two different strings depending on the engine: 'Updates to index settings is not supported.' (OpenSearch_2.11 domain) and 'The domain is not on the latest software version. Upgrade the domain to start using this feature. ' (Elasticsearch_7.10 domain, note the trailing space). Neither is AWS's wording, and the message varies where AWS's is constant.
Why: the message is the only thing distinguishing this gate from the other ValidationExceptions these operations can raise, so a client cannot tell "this engine version does not offer index management" from a rejected IndexSchema; log assertions and user-facing error text written against AWS do not match, and a trailing space in an error string breaks exact-match handling.
Fix: the version-gate validation shared by the UpdateIndex and GetIndex handlers in the opensearch service crate — one message, AWS's exact text, for both operations and every gated engine version.
Traces: opensearch.dependency-failure-on-update-index, opensearch.overly-permissive-update-index-disabled-feature-blocked.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), `### opensearch` section.

[mismatch] opensearch.attach_data_source_duplicate_rejected
  AssertionError: expected ResourceNotFoundException, got ValidationException
  The check then asserts the message "Application <bogus-id> not found for account <account-id>"; the positive control (attach to a real application, poll to ATTACHED) was never reached.

[mismatch] opensearch.dependency-failure-on-update-index
  AssertionError: expected ValidationException with AWS's exact software-version-gate message, got ('ValidationException', 'Updates to index settings is not supported.')
  Expected ('ValidationException', 'Automatic Semantic Enrichment feature is not supported on this software version.') on an OpenSearch_2.11 domain.

[mismatch] opensearch.overly-permissive-update-index-disabled-feature-blocked
  AssertionError: UpdateIndex ValidationException message 'The domain is not on the latest software version. Upgrade the domain to start using this feature. '
  Expected the same 'Automatic Semantic Enrichment feature is not supported on this software version.' on an ACTIVE Elasticsearch_7.10 domain; GetIndex must be refused identically.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout.
2. Cause 1: CreateApplication (to have a real application for the positive control) and CreateDomain for a minimum single-node domain. Call AttachDataSource with id=<a pattern-valid id that names no application, e.g. 20 lowercase alphanumerics> and dataSourceArn=<the domain ARN>. AWS refuses with ResourceNotFoundException (409) and message "Application <id> not found for account <account-id>"; the emulator refuses with ValidationException. Then AttachDataSource to the real application id and poll the attachment to ATTACHED.
3. Cause 2, OpenSearch_2.11: CreateDomain with EngineVersion OpenSearch_2.11 (t3.small.search x1, gp3 10 GiB) and call UpdateIndex(DomainName, IndexName, IndexSchema={"settings": {"index": {"number_of_shards": 1}}}). Read Error.Code and Error.Message.
4. Cause 2, Elasticsearch_7.10: CreateDomain with EngineVersion Elasticsearch_7.10, wait until it is genuinely ACTIVE (not merely visible), then call UpdateIndex and GetIndex for the same index name and read Error.Code and Error.Message for both. AWS answers ValidationException / "Automatic Semantic Enrichment feature is not supported on this software version." in every case; the emulator answers two other strings.
5. Replay opensearch.attach_data_source_duplicate_rejected, opensearch.dependency-failure-on-update-index and opensearch.overly-permissive-update-index-disabled-feature-blocked.
