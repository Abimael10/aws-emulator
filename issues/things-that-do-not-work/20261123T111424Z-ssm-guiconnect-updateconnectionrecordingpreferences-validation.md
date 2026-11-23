Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: cd8f73d475d74adb0ca8a470eb19f8c740cf481a

# The credential-scope Region is used unfiltered as the IAM resource ARN, so one request is judged for one Region and enrollment-checked for another — and a Region-scoped explicit Deny can be dodged

_Filed: 2026-11-23T11:14:24Z_
_Operation: UpdateConnectionRecordingPreferences | Class: validation_

> _Note: this issue was identified against the code base at commit `cd8f73d475d74adb0ca8a470eb19f8c740cf481a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate reads "the Region this request was signed for" TWICE for one and the same `UpdateConnectionRecordingPreferences` call, through two different readings that disagree:

* the IAM decision uses this crate's own `state::authorization::request_region`, which takes the credential scope's region field verbatim and interpolates it into the resource ARN the decision is taken against;
* the just-in-time node access gate uses `ssm_store::request_region`, which REFUSES region text carrying anything outside `[A-Za-z0-9-]` and substitutes the default, for the stated reason that "the value lands inside an ARN and in a store key".

The region field of a SigV4 credential scope is caller-controlled text that the parser only requires to be non-empty and free of `/` and `,` (vendor/server/src/lib.rs:1985-1993), and `emulator_arn` interpolates whatever it is given (vendor/server/src/lib.rs:170-172). So a request signed with `Credential=AKIDEXAMPLE/20260101/*/ssm-guiconnect/aws4_request` is:

* routed to this service (the route key comes from the scope's SERVICE field, service.rs:123-126),
* authorized against the resource `arn:aws:ec2:*:000000000000:instance/*`, where the `*` is a LITERAL character of the request's resource, because `wildcard_match` treats `*`/`?` as wildcards only on the POLICY side,
* but enrollment-checked for `us-east-1`, because `*` fails SSM's `region_is_well_formed` and falls back to the default.

Two consequences, both from the quoted code:

1. The two gates of one request answer for two different Regions, which contradicts this crate's own stated rule that the decision is taken "against the account's EC2 instance space in the REGION the request was signed for".
2. An explicit `Deny` scoped to one Region's instances is dodged: the pattern `arn:aws:ec2:us-east-1:000000000000:instance/*` does not match the literal value `arn:aws:ec2:*:000000000000:instance/*`, while an `Allow` on `"*"` still does — so the caller performs the action the Region-scoped Deny was written to refuse, and the enrollment gate still passes it as `us-east-1`.

The inconsistency is inside this crate: `just_in_time_node_access.rs:46` and `authorization.rs:163` are two call sites in the SAME handler (update_connection_recording_preferences.rs:189-191 and :211-214) asking the same question of the same request and getting two different answers. No comment or test in this crate states a reason for the lenient reading; nothing in `authorization.rs` mentions the region's shape at all.

The same unfiltered reading feeds `GetConnectionRecordingPreferences` and `DeleteConnectionRecordingPreferences` too (they call the same `authorization::refusal`), but only the Update handler holds both readings side by side, which is why it is named here.

## Evidence

SIDE 1 — this crate's own reading, unfiltered, going straight into an ARN:
services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state/authorization.rs:126-134
    `/// The region this request was signed for, defaulting to [`DEFAULT_REGION`].`
    `///`
    `/// The only region a request carries is the one in its SigV4 credential scope, so that is`
    `/// what the region-scoped resource ARN below is built from.`
    `#[must_use]`
    `pub fn request_region(parts: &::http::request::Parts) -> String {`
    `    ::emulator_server::sigv4_credential_scope(&parts.headers)`
    `        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)`
    `}`
services/.../src/state/authorization.rs:141-143
    `pub fn recorded_instances_resource(region: &str) -> String {`
    `    ::emulator_server::emulator_arn(INSTANCE_SERVICE, region, ANY_INSTANCE)`
    `}`
used by the decision at services/.../src/state/authorization.rs:163
    `let resource = recorded_instances_resource(&request_region(ctx.parts()));`

SIDE 2 — the reading the SAME handler uses for the enrollment gate, which refuses that text and defaults instead, with the rule stated:
services/.../src/state/just_in_time_node_access.rs:46
    `let region = ssm_store::request_region(parts);`
services/service-com-amazonaws-ssm-amazonssm/src/state.rs:3862-3870
    `/// The scope is caller-controlled text and the value lands inside an ARN and in a`
    `/// store key, so anything outside `[A-Za-z0-9-]` is refused here and the default used`
    `/// instead: a generated ARN can never carry text an ARN may not hold.`
    `pub fn request_region(parts: &::http::request::Parts) -> String {`
    `    ::emulator_server::sigv4_credential_scope(&parts.headers)`
    `        .map(|scope| scope.region)`
    `        .filter(|region| region_is_well_formed(region))`
    `        .unwrap_or_else(|| DEFAULT_REGION.to_string())`
    `}`

BOTH call sites are in ONE handler:
services/.../src/ops/update_connection_recording_preferences.rs:189-191
    `if let Some(message) =`
    `    crate::state::authorization::refusal(&*self.engine, ctx, Self::NAME)`
services/.../src/ops/update_connection_recording_preferences.rs:211-214
    `if !crate::state::just_in_time_node_access::account_is_enrolled(`
    `    &*self.engine,`
    `    ctx.parts(),`
    `) {`

THE RULE THE DIVERGENCE BREAKS, pinned by this crate's own unit test — services/.../src/state/authorization.rs:398-402
    `/// RULE: the decision is taken against the account's EC2 instance space in the REGION`
    `/// the request was signed for, so a statement scoped to one Region's instances covers`
    `/// that Region and no other.`
    `#[tokio::test]`
    `async fn a_statement_is_read_for_the_instance_space_of_the_requests_region() {`

WHY A LITERAL `*` IN THE REQUEST RESOURCE DODGES A REGION-SCOPED DENY — services/common/src/iam_authorization.rs:1003-1016
    `/// Whether `pattern` matches `value`, with IAM's `*` (any run of characters, including`
    `/// none) and `?` (exactly one character) wildcards.`
    `/// Every `*` and `?` of `pattern` is a wildcard here, ...`
    `pub fn wildcard_match(pattern: &str, value: &str, case_sensitive: bool) -> bool {`
(only the POLICY side holds wildcards; the request `value` is matched literally)

THAT THE TEXT IS CALLER-CONTROLLED AND UNCHECKED — vendor/server/src/lib.rs:1985-1993
    `if access_key_id.is_empty() || date.is_empty() || region.is_empty() || service.is_empty() {`
    `    return None;`
    `}`
    `Some(SigV4Scope { ... region: region.to_string(), ... })`
and vendor/server/src/lib.rs:170-172
    `pub fn emulator_arn(service: &str, region: &str, resource: &str) -> String {`
    `    format!("arn:aws:{service}:{region}:{EMULATOR_ACCOUNT_ID}:{resource}")`
    `}`

TEST COVERAGE, and the hole in it: every request helper hardcodes a well-formed Region — tests/all/connection_recording_preferences.rs:100-103 `Credential={akid}/20260101/us-east-1/ssm-guiconnect/aws4_request, `, tests/all/jit_node_access_gate_and_error_messages.rs:72 — and the one Region-spanning case uses only well-formed names, tests/all/identity_authorization.rs:846-850
    `let rows: [(&str, &str, bool); 3] = [`
    `    ("the Region the grant names", REGION, true),`
    `    ("another Region", "eu-west-3", false),`
    `    ("a third Region", "ap-south-1", false),`
so filtering the region the way the sibling reading does leaves `a_region_scoped_grant_serves_only_that_regions_calls` (:836), `a_statement_is_read_for_the_instance_space_of_the_requests_region` (:402) and `the_enrollment_is_decided_for_the_region_the_request_was_signed_for` (jit:589) unchanged. No test pins a malformed scope region, and no comment states a reason for the asymmetry.

## Steps to Reproduce

1. Stand the emulator up with this service, Systems Manager, IAM and STS registered, and enroll the account in just-in-time node access for us-east-1 (an access-request approval policy, as tests/all/identity_authorization.rs:355 `enroll` does).
2. Create an IAM user whose identity policy both allows the action broadly and denies it for one Region's instances: {"Statement":[{"Effect":"Allow","Action":"ssm-guiconnect:*","Resource":"*"},{"Effect":"Deny","Action":"ssm-guiconnect:*","Resource":"arn:aws:ec2:us-east-1:000000000000:instance/*"}]}.
3. Call POST /UpdateConnectionRecordingPreferences signed `Credential=<akid>/20260101/us-east-1/ssm-guiconnect/aws4_request` with a valid body. Observed: 403 AccessDeniedException — the Deny covers `arn:aws:ec2:us-east-1:000000000000:instance/*` (src/state/authorization.rs:163).
4. Repeat the identical call with only the credential scope's region field changed to `*` (`Credential=<akid>/20260101/*/ssm-guiconnect/aws4_request`). The request is still claimed by this service (src/service.rs:123-126 keys on the scope's SERVICE field). Observed: 200 — the decision was taken against `arn:aws:ec2:*:000000000000:instance/*`, which the Deny's pattern does not match because the request resource's `*` is a literal (services/common/src/iam_authorization.rs:1003-1016), while the `Allow` on `"*"` still matches.
5. Observe that the SAME request's enrollment gate answered for `us-east-1`, not for `*`: src/state/just_in_time_node_access.rs:46 goes through `ssm_store::request_region`, which rejects `*` as not well-formed and substitutes the default (services/service-com-amazonaws-ssm-amazonssm/src/state.rs:3866-3869) — so one request was judged for two different Regions.
6. Call GetConnectionRecordingPreferences signed for us-east-1 and see the record the denied-for-us-east-1 call wrote.
