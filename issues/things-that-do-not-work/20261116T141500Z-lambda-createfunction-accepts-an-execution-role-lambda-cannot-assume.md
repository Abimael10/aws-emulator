Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-function-lifecycle-crud
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-function-lifecycle-crud
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateFunction|4xx->2xx|$.Type
Issue-Related: 20261116T024510Z-lambda-3-net-new-causes-in-request-validation.md
Issue-Related: 20261114T124155Z-lambda-4-net-new-causes-function-url-timestamp.md
Issue-Related: 20261109T163411Z-cloudformation-iam-lambda-s3-lambda-createfunction-accepts-a-role-arn-that-names.md

# lambda: CreateFunction accepts an execution role Lambda cannot assume (split out of the function-URL-timestamp issue)

> _This record was SPLIT OUT of
> `20261114T124155Z-lambda-4-net-new-causes-function-url-timestamp.md` when the other three
> causes of that issue were fixed. It carries that issue's Cause 1 unchanged, plus what the
> attempted fix measured. The same cluster is also named as Cause 1 of
> `20261116T024510Z-lambda-3-net-new-causes-in-request-validation.md`; whichever change lands
> first should close both records._

## Operation
CreateFunction

## Description
CreateFunction accepts an execution role Lambda cannot assume (fixture
lambda-function-lifecycle-crud, step task#7).

AWS: CreateFunction resolves the `Role` before the function is persisted and refuses a role
Lambda cannot assume — a role that does not exist as much as one whose trust policy does not
let `lambda.amazonaws.com` assume it — with HTTP 400, header
`x-amzn-errortype: InvalidParameterValueException` and body
`{"Type":"User","message":"The role defined for the function cannot be assumed by Lambda."}`.
That message is entirely fixed text for this error class: it carries no request- or
state-derived value, and `User` is the fixed `Type` of a caller-caused refusal. Smithy:
`CreateFunction.Role` is `@required`, target `RoleArn`, pattern
`^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`; the operation models
`InvalidParameterValueException` (errorKind client, httpError 400, members `Type` and
`message`) and answers 201 on success.

Emulator: answers HTTP 201 and creates the function, so `$.Type`, `$.message` and the
`x-amzn-errortype` header are all absent — no refusal is emitted at all.

Why: a client that misconfigures (or misspells) an execution role is told the function was
created, when AWS would never have accepted it; the misconfiguration surfaces only at the
first invoke, and every negative-path test that asserts the 400 passes vacuously.

## Where the behaviour lives
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state.rs`,
`execution_role_is_unassumable`, which both write paths that store an execution role
(`ops/create_function.rs`, `ops/update_function_configuration.rs`) ask before they persist
anything. It takes `service_common::iam_role::held_role_is_unassumable`, which refuses only a
role this account's IAM HOLDS whose trust policy withholds the service principal, and SERVES a
role IAM reports absent.

## What an attempted fix measured (2026-11-16)
Taking `service_common::iam_role::role_is_unassumable` instead — the same reading with IAM's
`Absent` answer refusing — is the whole behavioural change, and it is enough: with it, the
replay of `lambda-function-lifecycle-crud` PASSES end to end, task#7 included (400
`InvalidParameterValueException` with the message above). That also confirms the recorded role
is one this account's IAM holds no record of, rather than a held role with a withholding trust
policy.

What makes it a larger piece of work than one line is the rule it reverses, which this crate
states deliberately and several suites assert:

* `cargo test -p service-com-amazonaws-lambda-awsgirapiservice --test all` fails **28** tests
  across ~14 suites whose premise is "a role this account's IAM holds no record of is no
  evidence about the role, so the write is served": `invoke_identity_authorization`,
  `invoke_authorization`, `invoke_authorization_ceiling`, `invoke_caller_authorization`,
  `invoke_resource_policy_denial`, `invoke_log_delivery`, `invocation_log_delivery`,
  `invocation_report_memory_used`, `invocation_run_capacity`,
  `invocation_reserved_concurrency_burst`, `handler_logging_facility`,
  `node_handler_error_wording`, `node_unhandled_error_log`, and the "a role nothing is known
  about refuses nothing" rows of `dead_letter_config_execution_role_permissions` and
  `vpc_config_execution_role_permissions`. Each of them creates a function with a well-formed
  role ARN of this account while IAM is mounted beside Lambda.
* `core/router/tests/all/lambda_execution_role_trust.rs` asserts the served answer directly
  (`unheld_rows`, "a role this account's IAM does not hold" → accepted), and the maintainer
  note in `issues/things-that-now-work/human-lambda-createcapacityprovider-refuses-an-operator-role-absen.md`
  says "Do not change `held_role_is_unassumable` for its other callers (a function's execution
  role)".
* The same reversal reaches every other crate whose tests mount IAM beside Lambda and create a
  function with a role IAM holds nothing under (`core/router/tests/all/` has dozens of such
  suites), so the change needs a full-workspace `cargo test` to land safely.

A remedy that keeps each of those suites' premise intact is available per suite: a row that
means "nothing is known about this role" can name a role of ANOTHER account
(`arn:aws:iam::123456789012:role/...`), which this emulator's IAM is not the one to ask about
and which both readings serve, while a row that means "a role this account holds" creates it
through IAM's own `CreateRole` (the shared `given_role` helper in
`tests/all/event_source_mapping_role_access.rs` already does this).

## Evidence
Replay (corpus simparity-wire-aws-v1), fixture lambda-function-lifecycle-crud step task#7,
operation CreateFunction, verdict mismatch,
divergence_key `lambda|CreateFunction|status=400->201|keys=$.Type,$.message,$headers.x-amzn-errortype,status`:
  expected: status 400; header `x-amzn-errortype: InvalidParameterValueException`; body
            `$.Type = 'User'`, `$.message = 'The role defined for the function cannot be assumed by Lambda.'`
  actual:   status 201; `$.Type` missing, `$.message` missing, no `x-amzn-errortype` header
            (the function is created).

## Steps to Reproduce
1. Clone the project repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus and read fixture
   lambda-function-lifecycle-crud at step task#7: `POST /2015-03-31/functions` naming an
   execution role this account's IAM holds no record of. Expect HTTP 400 with the header and
   body above; observe HTTP 201 and a created function.
3. The same precondition from a client: `CreateRole` a role trusting only
   `ec2.amazonaws.com`, then `CreateFunction` naming it — that one IS refused today — and then
   `CreateFunction` naming a role never created at all, which is not.
