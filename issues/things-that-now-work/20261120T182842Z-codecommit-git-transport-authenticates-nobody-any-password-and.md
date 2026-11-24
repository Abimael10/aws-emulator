Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit.git-credentials-with-wrong-password-refused codecommit.git-transport-after-credential-deleted-refused codecommit.git-credential-helper-clone-push-roundtrip codecommit.git-transport-with-forged-sigv4-refused
Issue-Shape-Id: com.amazonaws.codecommit#CodeCommit_20150413
Issue-Key: codecommit.git-credentials-with-wrong-password-refused|run#30|AssertionError|assert bad.returncode != 0, (
Issue-Key: codecommit.git-transport-after-credential-deleted-refused|_poll_git#31|Timeout|time.sleep(interval_s)
Issue-Key: codecommit.git-credential-helper-clone-push-roundtrip|run#18|AssertionError|assert clone.returncode == 0, (
Issue-Key: codecommit.git-transport-with-forged-sigv4-refused|run#24|AssertionError|assert real.returncode == 0, (
Issue-Commit: c9b7fc7795b284972b7c862846d545eef7a83f7a

# CodeCommit git transport authenticates nobody: any password and any revoked/unknown credential clone, and the announced cloneUrlHttp is http:// so the credential-helper path cannot authenticate at all (3 causes)

> _Note: this issue was identified against the code base at commit `c9b7fc7795b284972b7c862846d545eef7a83f7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GitPull/GitPush (git smart-HTTP transport) and GetRepository (repositoryMetadata.cloneUrlHttp)

## Description
Three net-new root causes in the CodeCommit git smart-HTTP transport and the address it is announced on. All three are in the codecommit service crate. Related but NOT covering: open issue 20261025T040000Z-codecommit-announced-clone-url-is-served-with-an-untrusted-certificate.md (it carries three other check ids and is about the announced URL being unreachable at all; the URL is now reachable in the clear, and what remains is described in Cause 3 of 3).

Cause 1 of 3 — the password half of a CodeCommit git credential is never verified, so a wrong password clones.
AWS: an HTTPS git credential is the (ServiceUserName, ServicePassword) pair IAM mints with CreateServiceSpecificCredential(ServiceName="codecommit.amazonaws.com"); the ServicePassword is returned once and is the secret. CodeCommit's git transport verifies it on every request: the documented setup flow (docs.aws.amazon.com/codecommit/latest/userguide/setting-up-gc.html) is user-name-plus-password HTTP Basic, and a request presenting a valid user name with a wrong password is NOT served. The requirement is a non-2xx refusal — the same 401 with the `Basic realm="CodeCommit"` challenge this transport already sends for a request that presents no credential, so an unmodified git client exits non-zero (`fatal: Authentication failed for '<clone url>'`); the exact refusal status/body is recorded behavior only, since the recording shows only that git exits non-zero. The user name is not a secret (ListServiceSpecificCredentials returns it), so the password is the whole proof.
Emulator: the clone with a valid ServiceUserName and `wrong-<ServicePassword>` SUCCEEDS — exit 0, empty stderr, full ref advertisement and packfile served. The crate states this as deliberate: services/service-com-amazonaws-codecommit-codecommit-20150413/src/service/git_authorization.rs:51-56 ("The password is NOT verified, and cannot be") and src/service/git_http.rs:77-83.
Why: a client relying on AWS gets no authentication at all on the git data plane — knowing only a non-secret user name grants read and write to every repository the holder's policies allow, and any test asserting that a bad password is refused passes vacuously.
Fix: the presented-credential resolution in src/service/git_authorization.rs (`presented_principal` / `long_term_principal`) must verify the presented Basic password against a verifier the IAM credential store keeps for the ServicePassword it minted, and src/service/git_http.rs must answer the challenge refusal when it does not match.
Traces: codecommit.git-credentials-with-wrong-password-refused.

Cause 2 of 3 — a revoked (deleted) or entirely unknown git credential is resolved to the account's own unbounded principal instead of being refused.
AWS: DeleteServiceSpecificCredential revokes git access; once the credential is deleted the same user name/password is refused on the git transport (eventually consistent, then permanently refused). A user name the account holds no credential record for likewise identifies nobody and is refused — it never resolves to the account's root/ambient principal.
Emulator: src/service/git_authorization.rs:318-345 `long_term_principal` falls through, for ANY presented id that matches no STS session, no access key and no ACTIVE service-specific credential, to `ambient_user_arn()` with `unbounded: true` — a principal that passes every `permits()` check. So after DeleteServiceSpecificCredential the identical clone keeps succeeding: the check polls for a refusal for up to 150 s and never sees one (the check ends "Check timed out after 60.0s" with the post-deletion clone still exiting 0).
Why: revocation is inert — a client that deletes a credential to cut off access still has it; and any invented user name is upgraded to an unbounded account principal, so IAM-scoped git access checks cannot be exercised.
Fix: `long_term_principal` in src/service/git_authorization.rs must return no principal for a credential no store holds (so git_http.rs refuses it) instead of synthesising the ambient unbounded principal; the ambient fallback belongs only to a credential the account genuinely published.
Traces: codecommit.git-transport-after-credential-deleted-refused.

Cause 3 of 3 — the announced cloneUrlHttp uses the `http` scheme, so the documented `aws codecommit credential-helper` flow supplies no credentials and SigV4 git access is unreachable.
AWS: Smithy: RepositoryMetadata.cloneUrlHttp — "The URL to use for cloning the repository over HTTPS"; its value is an `https://` origin (`https://git-codecommit.<region>.amazonaws.com/v1/repos/<name>`). The documented temporary-access flow (docs.aws.amazon.com/codecommit/latest/userguide/temporary-access.html) is `git -c credential.helper='!aws codecommit credential-helper $@' -c credential.UseHttpPath=true clone <cloneUrlHttp>`, where the helper answers git's credential query with the caller's access key id as the user name and a SigV4 signature over that HTTPS URL as the password, and the service verifies that signature — a clone with real credentials succeeds and one signed with wrong keys is refused.
Emulator: cloneUrlHttp is announced as `http://<host>:<port>/v1/repos/<name>` (src/state/git_endpoint.rs:66, `GIT_SCHEME = "http"`, announced by `ensure_git_endpoint`). Driven through the credential helper, git never obtains a user name and exits 128 with `fatal: could not read Username for 'http://<host>:<port>/v1/repos/<repo>': terminal prompts disabled` — fixed text `fatal: could not read Username for '<url>': terminal prompts disabled`, with the URL taken from the announced cloneUrlHttp; host and port vary per run and are not the requirement. Hypothesis: the `aws codecommit credential-helper` emits nothing for a credential query whose `protocol` is `http` (it signs only the HTTPS clone URL), so the transport's 401 challenge can never be satisfied. The same failure hits the forged-signature check's positive control, and because of it the negative half (wrong keys) is refused for the wrong reason — the transport also verifies no signature at all (src/service/git_authorization.rs:51-56), so a forged SigV4 would be accepted if the helper ever produced one.
Why: every client that follows AWS's documented credential-helper instructions cannot clone or push against this emulator, and a check that a forged SigV4 is refused passes without the emulator ever deciding it.
Fix: the announced address in src/state/git_endpoint.rs (`GIT_SCHEME`, `ensure_git_endpoint`) must be published under `https` on an origin a stock git client accepts, and the SigV4-derived password presented on that path must be verified in src/service/git_authorization.rs.
Traces: codecommit.git-credential-helper-clone-push-roundtrip, codecommit.git-transport-with-forged-sigv4-refused.

## Evidence
Cause 1 — codecommit.git-credentials-with-wrong-password-refused [mismatch]: `AssertionError: a git clone with a valid user name but a wrong password SUCCEEDED; it must be refused. stderr=''`. The positive control (correct password) cloned first, so the acceptance is attributable to the password being ignored, not to an unusable credential. Expected: non-2xx refusal (401 + `WWW-Authenticate: Basic realm="CodeCommit"`), git exits non-zero. Actual: HTTP 200 ref advertisement + packfile, git exits 0.

Cause 2 — codecommit.git-transport-after-credential-deleted-refused [mismatch]: `Check timed out after 60.0s`. The pre-deletion clone succeeded (positive control); after `iam.delete_service_specific_credential(...)` the check polls the identical authenticated clone for a non-zero exit and never gets one, exhausting the window. Expected: the revoked credential is refused. Actual: it is resolved to `ambient_user_arn()` with `unbounded: true` (git_authorization.rs:338-344) and the clone keeps succeeding.

Cause 3 — codecommit.git-credential-helper-clone-push-roundtrip [mismatch]: `AssertionError: credential-helper clone exited 128; stderr="fatal: could not read Username for 'http://127.0.0.1:45413/v1/repos/sim2real-behav-cchelper-<id>': terminal prompts disabled\n"`. codecommit.git-transport-with-forged-sigv4-refused [mismatch]: `AssertionError: the clone with real credentials exited 128; stderr="fatal: could not read Username for 'http://127.0.0.1:34027/v1/repos/sim2real-behav-ccforge-<id>': terminal prompts disabled\n"`. In both, the announced cloneUrlHttp carries the `http` scheme (GIT_SCHEME = "http"); AWS's cloneUrlHttp is an `https://` URL. Host, port and repository suffix are per-run values, not requirements.

## Steps to Reproduce
Cause 1:
1. `meta = cc.create_repository(repositoryName=R)['repositoryMetadata']`; `cc.put_file(...)` to seed `main`.
2. Create an IAM user, attach a policy allowing `codecommit:GitPull`/`GitPush` on `meta['Arn']`, and `iam.create_service_specific_credential(UserName=U, ServiceName='codecommit.amazonaws.com')`.
3. `git clone http(s)://<ServiceUserName>:<ServicePassword>@<authority>/v1/repos/R` -> succeeds (control).
4. `git clone http(s)://<ServiceUserName>:wrong-<ServicePassword>@<authority>/v1/repos/R` -> expect a non-zero exit after a 401; observe exit 0 with the repository cloned.

Cause 2:
5. With the same credential, clone once (succeeds), then `iam.delete_service_specific_credential(UserName=U, ServiceSpecificCredentialId=...)`.
6. Re-run the identical authenticated clone -> expect a refusal; observe it still succeeding indefinitely. Same for a user name no credential record exists for.

Cause 3:
7. Read `meta['cloneUrlHttp']` -> observe an `http://...` URL where AWS returns `https://git-codecommit.<region>.amazonaws.com/v1/repos/R`.
8. `git -c credential.helper='!aws codecommit credential-helper $@' -c credential.UseHttpPath=true clone <meta['cloneUrlHttp']>` with `GIT_TERMINAL_PROMPT=0` -> expect the clone to complete; observe exit 128 with `fatal: could not read Username for '<url>': terminal prompts disabled`.
9. Re-replay the four check ids in trace_ids.
