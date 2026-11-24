Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit.git-credential-helper-clone-push-roundtrip codecommit.git-transport-with-forged-sigv4-refused
Issue-Shape-Id: com.amazonaws.codecommit#CodeCommit_20150413
Issue-Key: codecommit.git-credential-helper-clone-push-roundtrip|run#18|AssertionError|assert clone.returncode == 0, (
Issue-Key: codecommit.git-transport-with-forged-sigv4-refused|run#24|AssertionError|assert real.returncode == 0, (
Issue-Commit: 97d900f89f587f485ba8a31fa0a1f26bc3bdf335

# CodeCommit credential-helper git transport: the announced cloneUrlHttp is served with a self-signed certificate, so the SigV4 credential-helper clone/push path is unreachable

> _Note: this issue was identified against the code base at commit `97d900f89f587f485ba8a31fa0a1f26bc3bdf335`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRepository

## Description
Cause 1 of 1 — the git smart-HTTP endpoint announced in GetRepository's repositoryMetadata.cloneUrlHttp terminates TLS with a certificate no standard git client trusts, so the `aws codecommit credential-helper` (SigV4 temporary-access) transport path can never be exercised: both the happy-path roundtrip and the positive control of the forged-signature rejection check die at the TLS handshake before any git protocol exchange. Failing checks: codecommit.git-credential-helper-clone-push-roundtrip, codecommit.git-transport-with-forged-sigv4-refused.

AWS: Smithy: GetRepository output shape RepositoryMetadata member cloneUrlHttp (String) — the model documents it as "The URL to use for cloning the repository over HTTPS"; the model says nothing further about the transport, so for the transport itself this is recorded/documented behavior only (https://docs.aws.amazon.com/codecommit/latest/userguide/temporary-access.html). Real CodeCommit serves git smart-HTTP on that address over TLS with a publicly trusted certificate, and authenticates the request with SigV4 produced by `aws codecommit credential-helper get`. Consequently: (a) `git -c credential.helper='!aws codecommit credential-helper $@' -c credential.UseHttpPath=true clone <cloneUrlHttp>` exits 0, a subsequent `git push origin main` exits 0, and GetBranch(repositoryName, branchName='main') then reports the commitId that git pushed; (b) the same clone performed with a non-authoritative access key/secret pair produces a SigV4 signature that does not verify and is refused by the transport (non-zero git exit), while the caller's real credentials succeed.

Emulator: both clones fail at TLS, never reaching authentication or the git protocol. git exits 128 with `fatal: unable to access 'https://<host>:<port>/v1/repos/<repositoryName>/': SSL certificate OpenSSL verify result: self-signed certificate (18)`. The fixed text of that failure is git's own; the host, port and repository name are per-run values from the announced cloneUrlHttp and are not part of the requirement. In codecommit.git-transport-with-forged-sigv4-refused the negative half is indistinguishable from the positive half for the wrong reason — the forged-key clone is 'refused' by the certificate, not by signature verification — and the positive control then fails, so the SigV4 verification behaviour the check exists to pin is never observed.

Why: a client that reads cloneUrlHttp from GetRepository and hands it to an unmodified git client, as the CodeCommit credential-helper workflow documents, cannot clone, fetch or push at all; it sees a certificate-trust failure rather than either success or an authentication refusal. Any emulator-side SigV4 verification on the git transport is also unverifiable, so a forged-credential clone cannot be shown to be rejected for the right reason.

Fix: the listener/TLS seam that fronts the CodeCommit git smart-HTTP transport and the handler that composes the announced clone URL — the transport must be reachable by an unmodified git client at the address GetRepository announces (either a certificate the client's trust store accepts, or a plaintext loopback endpoint that the announced URL names), and once reachable the credential-helper SigV4 request must be verified so a wrong-key signature is refused and a correct one is accepted.

Related: open issue 20261025T040000Z-codecommit-announced-clone-url-is-served-with-an-untrusted-certificate.md records the same certificate rejection, but its Issue-Trace-Ids/Issue-Key entries carry only codecommit.delete-file-on-tag-named-branch-blocked, codecommit.git-push-non-fast-forward-refused-by-the-server and codecommit.git-push-over-https-is-readable-through-the-api, so it does not carry the two credential-helper/SigV4 check keys filed here; open issue 20261122T211920Z-codecommit-git-transport-no-clone-fetch-or-push.md likewise owns a different set of nine git checks. Neither covers these two.

Traces: codecommit.git-credential-helper-clone-push-roundtrip (assertion at check line 64: `credential-helper clone exited 128; stderr="fatal: unable to access '.../v1/repos/<repo>/': SSL certificate OpenSSL verify result: self-signed certificate (18)"`); codecommit.git-transport-with-forged-sigv4-refused (assertion at check line 83: `the clone with real credentials exited 128; stderr="... self-signed certificate (18)"`).

## Evidence
Expected (real AWS):
- codecommit.git-credential-helper-clone-push-roundtrip: `git -c credential.helper='!aws codecommit credential-helper $@' -c credential.UseHttpPath=true clone <cloneUrlHttp>` exits 0; a commit pushed with the same helper exits 0; GetBranch(branchName='main').branch.commitId equals the local `git rev-parse HEAD`.
- codecommit.git-transport-with-forged-sigv4-refused: the clone signed with a wrong access key/secret exits non-zero (refused by SigV4 verification); the clone signed with the caller's real credentials exits 0.

Actual (emulator): both clones exit 128 at the TLS handshake with git's
`fatal: unable to access 'https://<host>:<port>/v1/repos/<repositoryName>/': SSL certificate OpenSSL verify result: self-signed certificate (18)`
(fixed text from git/OpenSSL; host, port and repository name are per-run values). No HTTP request, no SigV4 verification and no git protocol exchange occur. The forged-credential half therefore cannot be attributed to signature rejection, and the positive control fails outright.

## Steps to Reproduce
1. `meta = cc.create_repository(repositoryName=R)['repositoryMetadata']`.
2. `cc.put_file(repositoryName=R, branchName='main', filePath='README.md', fileContent=b'seed\n', commitMessage='seed')`.
3. With a hermetic git (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`) run `git -c credential.helper='!aws codecommit credential-helper $@' -c credential.UseHttpPath=true clone --quiet <meta['cloneUrlHttp']> ./clone` -> expect exit 0; observe exit 128 with `SSL certificate OpenSSL verify result: self-signed certificate (18)`.
4. Commit a new file in ./clone and `git push origin main` with the same helper -> expect exit 0 and GetBranch('main').branch.commitId == local HEAD.
5. Repeat step 3 with AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE / AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY and no session token -> expect a non-zero exit caused by SigV4 verification; then repeat with the caller's real credentials -> expect exit 0, observe the same certificate failure.
6. Re-replay codecommit.git-credential-helper-clone-push-roundtrip and codecommit.git-transport-with-forged-sigv4-refused.
