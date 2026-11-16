Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: amplify.create-backend-environment-lifecycle
Issue-Shape-Id: com.amazonaws.amplify#Amplify
Issue-Key: amplify.create-backend-environment-lifecycle|run#14|AssertionError|assert err_bare_gone == _env_missing(env_bare), err_bare_gone
Issue-Commit: f42da0d4917d7016500e6277673593e18f63e86f

# Amplify backend-environment 404 message diverges: "Backend environment <name> not found." instead of "BackendEnvironment <name> does not exist."

> _Note: this issue was identified against the code base at commit `f42da0d4917d7016500e6277673593e18f63e86f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBackendEnvironment

## Description
One net-new root cause in the Amplify backend-environment operations.

Cause 1 of 1: the NotFoundException message for a backend environment that does not exist uses this crate's generic entity template instead of the wording Amplify uses for this entity. Failing check ids: amplify.create-backend-environment-lifecycle.

AWS: GetBackendEnvironment / DeleteBackendEnvironment answer HTTP 404 with error code NotFoundException when the named environmentName does not exist under the app. The Smithy model spells out only the exception itself (smithy_query(query="operation", operation="GetBackendEnvironment","DeleteBackendEnvironment"): error NotFoundException, httpError 404, doc "An entity was not found during an operation."); it does not spell the message text, so the message is recorded behavior only. The recorded behavior is the message `BackendEnvironment <environmentName> does not exist.` — fixed text `BackendEnvironment `, then the environmentName taken verbatim from the request, then the fixed text ` does not exist.` (one sentence, closed by a period). Note the entity word is the single CamelCase token `BackendEnvironment`, and the predicate is `does not exist.`, not `not found.`. The status (404) and code (NotFoundException) the emulator returns are already correct.

Emulator: the same request answers 404 NotFoundException with the message `Backend environment <environmentName> not found.` — e.g. for a just-deleted environment the replay observed ('NotFoundException', 'Backend environment oprjaogd not found.') where AWS answers 'NotFoundException', 'BackendEnvironment oprjaogd does not exist.'. The divergence is in the text only: the entity word is spelled as two words (`Backend environment`) and the predicate is `not found.`. It reproduces on every address of a missing backend environment: GetBackendEnvironment after a successful DeleteBackendEnvironment, a second DeleteBackendEnvironment of the same name, GetBackendEnvironment of a validly-named environment that was never created, and DeleteBackendEnvironment of one that was never created — the check asserts all four and fails on the first.

Why: a client that branches on the refusal text — the common pattern of treating "does not exist" as idempotent-delete/absent and anything else as a real failure, or a test/assertion written against the AWS string — takes the wrong branch against the emulator even though the code matches. Because every one of the four absence paths shares the wording, an application's absence handling for Gen 1 backend environments is uniformly wrong, and the whole backend-environment lifecycle check stops at its first assertion so nothing after it (opaque stackName handling, environmentName 2–10-lowercase-letter validation, the missing-app NotFoundException) reaches a verdict.

Fix: the backend-environment entity wording used to build this 404's message belongs in the Amplify crate's state-level message construction, where every 404's text is centralized, not in either handler — but it must not be fixed by changing the shared not_found_message construction, which other entities (App, Branch, Job, Webhook, Domain) do answer with `<Entity> <id> not found.` and which this same check relies on for the missing-app case (`App dnonexistent00000000 not found.`). Backend environments need their own message construction for this error class, used by both the GetBackendEnvironment and DeleteBackendEnvironment handlers' not-found arms so the two cannot drift.

Traces: amplify.create-backend-environment-lifecycle. The construction sites are services/service-com-amazonaws-amplify-amplify/src/state.rs:2517 (not_found_message) and :2560 (BACKEND_ENVIRONMENT_ENTITY = "Backend environment"), reached from services/service-com-amazonaws-amplify-amplify/src/ops/get_backend_environment.rs:170-176 and services/service-com-amazonaws-amplify-amplify/src/ops/delete_backend_environment.rs:174-181.

## Evidence
Check amplify.create-backend-environment-lifecycle, line 46 (assert err_bare_gone == _env_missing(env_bare)):

Expected (AWS, per the recorded check's _env_missing helper):
  ("NotFoundException", "BackendEnvironment <environmentName> does not exist.")
Actual (emulator):
  AssertionError: ('NotFoundException', 'Backend environment oprjaogd not found.')

HTTP status 404 and error code NotFoundException agree; only the message text diverges. Fixed text required: "BackendEnvironment " + " does not exist."; the only variable part is environmentName, echoed from the request ("oprjaogd" above is one run's generated name, not part of the template).

Emulator source producing it: state.rs not_found_message(entity, identifier) -> format!("{entity} {identifier} not found.") with BACKEND_ENVIRONMENT_ENTITY = "Backend environment".

Same wording is returned by the other three absence paths the check asserts (delete-after-delete, get-never-created, delete-never-created).

## Steps to Reproduce
1. amplify.create_app(name=<unique>) -> appId.
2. amplify.create_backend_environment(appId=appId, environmentName="oprjaogd")  # 2-10 lowercase letters.
3. amplify.delete_backend_environment(appId=appId, environmentName="oprjaogd")  # succeeds.
4. amplify.get_backend_environment(appId=appId, environmentName="oprjaogd")
   -> emulator: 404 NotFoundException "Backend environment oprjaogd not found."
   -> AWS:      404 NotFoundException "BackendEnvironment oprjaogd does not exist."
5. Repeat step 3 (delete again), and call get/delete for an environmentName never created (e.g. "gabcdefg"): each shows the same diverging message.
Replay: amplify.create-backend-environment-lifecycle.
