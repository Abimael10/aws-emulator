Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-knowledge-base-documents-delete-workflow
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-knowledge-base-documents-delete-workflow
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|ListKnowledgeBaseDocuments|2xx->2xx|$
Issue-Key: bedrock-agent|ListKnowledgeBaseDocuments|2xx->2xx|$.documentDetails[*]
Issue-Key: bedrock-agent|ListKnowledgeBaseDocuments|2xx->2xx|$.documentDetails[*].status
Issue-Commit: f8e20ae46548b9b285010d18fd783588395da7bd

# bedrock-agent ListKnowledgeBaseDocuments drops a completed deletion's document from the listing instead of reporting it with status NOT_FOUND

> _Note: this issue was identified against the code base at commit `f8e20ae46548b9b285010d18fd783588395da7bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListKnowledgeBaseDocuments

## Description
Cause 1 of 1: after DeleteKnowledgeBaseDocuments has run to completion for a CUSTOM document, ListKnowledgeBaseDocuments must still enumerate that document's identifier, carrying the terminal status NOT_FOUND. The emulator removes the record from the listing the moment the delete chain reaches its terminal status, so the page comes back with an empty documentDetails and a client polling the listing for the deletion's terminal status never observes it.

AWS: Smithy: ListKnowledgeBaseDocuments is POST /knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}/documents, @readonly, HTTP 200, output member documentDetails (target KnowledgeBaseDocumentDetails) is @required and is the @paginated items member; DeleteKnowledgeBaseDocuments is POST .../documents/deleteDocuments, @idempotent, HTTP 202, documentIdentifiers @length 1..10. KnowledgeBaseDocumentDetail requires knowledgeBaseId, dataSourceId, identifier and status, with status targeting the DocumentStatus enum whose values include DELETING, DELETE_IN_PROGRESS and NOT_FOUND ("NOT_FOUND - the document wasn't found"), plus the optional statusReason and updatedAt. The model does not say whether a completed deletion leaves the data source's listing, so for that point this is recorded behavior only: the recording shows the delete accepted, then the listing for the same knowledgeBaseId/dataSourceId polled until it reports exactly one documentDetails entry for the deleted identifier ({identifier.custom.id = <the deleted custom document id>, identifier.dataSourceType = CUSTOM, knowledgeBaseId, dataSourceId, status NOT_FOUND, updatedAt a date-time}). In other words the terminal outcome of the delete is reported THROUGH the listing, under the identifier the request named; the ids and the custom document id in the recording are per-run request values, and updatedAt is a per-run timestamp - the requirement is that the deleted identifier is still an entry of documentDetails and that its status reaches NOT_FOUND.

Emulator: the same poll never produces an entry. ListKnowledgeBaseDocuments answers HTTP 200 with documentDetails: [] for the whole 180s poll window (61 attempts), so $.documentDetails[0] is missing and $.documentDetails[0].status is never NOT_FOUND. The status chain itself does advance - the document's record is kept and its terminal status is reachable through the targeted GetKnowledgeBaseDocuments read - but the listing projection filters the record out as soon as it is no longer "held", so the listing can never report the terminal status.

Why: a client cannot confirm a deletion through the listing operation the recording uses for it. Any reconciler, CI assertion or purge workflow that deletes a document and then polls ListKnowledgeBaseDocuments for the document's terminal NOT_FOUND status blocks until its own timeout against the emulator while completing in seconds against AWS, and code that reads a data source's delete outcomes out of the listing page sees no outcome at all. Status codes and error codes are not at issue: every call succeeds; the divergence is the content of the 200 listing page.

Fix: the listing projection in this operation's state - the page builder ListKnowledgeBaseDocuments' handler calls must include a document record whose deletion has completed and report it under its terminal DocumentStatus, instead of excluding it with the "still held" predicate, while keeping the bound on retained deleted records so the page does not grow without limit.

Note on an existing open issue: 20261118... aside, the open behavioral issue 20261122T213226Z-bedrock-agent-listknowledgebasedocuments-keeps-enumerating-a-document-whose.md asserts the opposite requirement (that a completed deletion must LEAVE the listing) and the current code implements that reading; this wire fixture records AWS keeping the identifier in the listing with status NOT_FOUND, so neither issue covers this divergence and the two must be reconciled by whoever fixes them - the recorded wire behavior is the one this issue is grounded in.

Traces: bedrock-agent-knowledge-base-documents-delete-workflow#task#8. Supporting citation in the checkout: services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/state.rs lines 3694-3728 (page_of_documents filters on record.is_held(), and its doc comment states a document whose deletion has run to its end "is in no page"), and lines 3441-3450 (the record is retained with its terminal status but only for the targeted read).

## Evidence
Fixture bedrock-agent-knowledge-base-documents-delete-workflow, step task#8 (ListKnowledgeBaseDocuments), expected HTTP 200 / actual HTTP 200, body diverges:

Expected (recorded AWS): {"documentDetails": [{"knowledgeBaseId": "<kb id>", "dataSourceId": "<ds id>", "identifier": {"dataSourceType": "CUSTOM", "custom": {"id": "<deleted custom document id>"}}, "status": "NOT_FOUND", "updatedAt": "<date-time>"}]} - reached by polling the listing.

Actual (emulator): {"documentDetails": []} on every attempt. Replay detail: "$: poll capped at 180s"; "$.documentDetails[0]: missing"; "$.documentDetails[0].status: poll: not in ['NOT_FOUND'] after 61 attempt(s) (expected 'NOT_FOUND', got None)".

Template note: the knowledge-base id, data-source id and custom document id in the recorded entry are values taken from the request, and updatedAt is a per-run timestamp; the requirement is the presence of the deleted identifier's entry and its status value NOT_FOUND.

## Steps to Reproduce
1. Create a VECTOR knowledge base and a CUSTOM data source on it; wait for both to report ACTIVE/AVAILABLE.
2. IngestKnowledgeBaseDocuments with an in-line CUSTOM document whose customDocumentIdentifier id is D; poll until it reports INDEXED.
3. DeleteKnowledgeBaseDocuments(knowledgeBaseId, dataSourceId, documentIdentifiers=[{dataSourceType: CUSTOM, custom: {id: D}}]) - accepted with HTTP 202.
4. Poll ListKnowledgeBaseDocuments(knowledgeBaseId, dataSourceId) until the entry for D reports status NOT_FOUND. AWS reports that entry; the emulator answers 200 with documentDetails: [] indefinitely, so the poll never converges (fixture gives up after 180s / 61 attempts).
