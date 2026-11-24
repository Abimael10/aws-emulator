Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2.test-render-email-template-does-not-html-escape-substituted-values sesv2.test-render-email-template-assembles-a-multipart-alternative-mime-message sesv2.test-render-email-template-mime-encodes-non-ascii-rendering-data sesv2.email-template-contract-limits-rejected sesv2.email-template-lifecycle sesv2.import-job-create-get-list-lifecycle
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2.test-render-email-template-does-not-html-escape-substituted-values|run#9|AssertionError|assert _part_text(html_part) == f'<p>esc={_MARKUP} raw={_MARKUP}</p>', (
Issue-Key: sesv2.test-render-email-template-assembles-a-multipart-alternative-mime-message|run#18|AssertionError|assert boundaries[0] and boundaries[1] and boundaries[0] != boundaries[1], (
Issue-Key: sesv2.test-render-email-template-mime-encodes-non-ascii-rendering-data|run#11|AssertionError|assert [p['Content-Transfer-Encoding'] for p in wide_parts] == ['quoted-printable', 'quoted-printable'], (
Issue-Key: sesv2.email-template-contract-limits-rejected|run#19|AssertionError|assert 'Content-Transfer-Encoding: quoted-printable' in rendered, (
Issue-Key: sesv2.email-template-lifecycle|run#31|AssertionError|assert err.get('Message') == expected_message, f"expected the message {expected_message!r}, got {err.get('Message')!r}"
Issue-Key: sesv2.import-job-create-get-list-lifecycle|run#14|AssertionError|assert final_status == "FAILED", f"expected FAILED for a non-existent S3 bucket, got {final_status}"
Issue-Commit: 66e0b44d5c5b54c153b466beead5d8bb9530c165

# sesv2: 5 net-new causes in the template-render, template-store and import-job surfaces — the HTML part is HTML-escaped, the MIME boundary is not minted per render, no part is ever quoted-printable, TestRenderEmailTemplate's not-found sentence is the wrong one, and an import job from a nonexistent bucket reports COMPLETED

> _Note: this issue was identified against the code base at commit `66e0b44d5c5b54c153b466beead5d8bb9530c165`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestRenderEmailTemplate

## Description
Five net-new root causes in the SESv2 email-template rendering surface and the import-job lifecycle. All five are sesv2 defects, none is marked ALREADY FILED or OWNED in this replay.

=== Cause 1 of 5: TestRenderEmailTemplate HTML-escapes substituted values in the text/html part ===

AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) models TestRenderEmailTemplate as POST /v2/email/templates/{TemplateName}/render -> 200 {RenderedTemplate: "The complete MIME message rendered by applying the data in the TemplateData parameter to the template"}; the model says nothing about escaping, so this is recorded behavior only. The recording shows SES substitutes the rendering-data value VERBATIM everywhere it is referenced — in the Subject header, in the text/plain part and in the text/html part — under both the double-stash `{{v}}` and triple-stash `{{{v}}}` spellings, and emits no HTML entity (&lt; &gt; &amp; &quot; &#x27; &#39; &#x60;) of its own. For a template Subject 'S {{v}}', Text 'esc={{v}} raw={{{v}}}', Html '<p>esc={{v}} raw={{{v}}}</p>' and a value containing HTML metacharacters, the value occurs verbatim exactly five times in the rendered message.

Emulator: the text/plain part and the triple-stash reference are verbatim, but the double-stash reference inside the text/html part is HTML-escaped, so the html part reads '<p>esc=&lt;b&gt;&amp;&quot;x&quot;&#x27;y&#x27;&lt;/b&gt; raw=<b>&"x"\'y\'</b></p>' where AWS renders '<p>esc=<b>&"x"\'y\'</b> raw=<b>&"x"\'y\'</b></p>'. This is deliberate in the emulator: services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/template_language.rs documents "The HTML part's substituted values are HTML-escaped and the subject line's and the text part's are not" and leaves the handlebars engine's default escaping on for that part.

Why: every templated HTML email whose data carries an ampersand, an angle bracket, a quote or an apostrophe renders with visible entities instead of the characters (and intended markup) SES would have sent, so a preview no longer predicts the message and markup supplied through a variable — which SES permits — is neutered. A client has no way to opt out: the triple-stash workaround is not what its production template says.

Fix: the template evaluator this operation drives — crate::state::template_language::evaluate in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/template_language.rs — must substitute values without HTML escaping for every part (subject, text and html alike), and crate::state::template_rendering, which the send path shares, must follow the same rule so one template renders identically on both paths.

Traces: sesv2.test-render-email-template-does-not-html-escape-substituted-values.

=== Cause 2 of 5: the multipart boundary is derived from the rendered bodies, so two renders of the same template and data share one delimiter ===

AWS: recorded behavior only (the model constrains only that RenderedTemplate is "the complete MIME message"). The recording shows each call ASSEMBLES A NEW MIME message and mints its own boundary token: two renders of an unchanged template with unchanged data return two different `boundary="..."` parameters (and a fresh Date), while the parts themselves are identical.

Emulator: the boundary is a content digest. services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs::boundary() hashes the part bodies and formats "----=_Part_{hash:016x}", explicitly so that "the same rendered message is always previewed with the same document rather than one that differs per call". Two successive renders therefore returned the SAME token ('----=_Part_<same 16 hex digits>' twice).

Why: the boundary is a per-message delimiter, not a function of the content. A client that nests one render inside another multipart body, or that concatenates two renders, gets an inner delimiter identical to the outer one and the outer part is terminated early — the message silently loses content. It also makes the token a content fingerprint, leaking whether two renders carried identical bodies.

Fix: the operation's serializer — mime_document()/boundary() in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs — must mint a fresh, unpredictable boundary per response (still guaranteeing it appears in no part), rather than deriving it from the parts.

Traces: sesv2.test-render-email-template-assembles-a-multipart-alternative-mime-message.

=== Cause 3 of 5: no rendered part is ever quoted-printable — the parts are labelled 7bit/8bit and never encoded or soft-wrapped ===

AWS: recorded behavior only. The recording shows the transfer encoding follows the rendered text: ASCII rendering data leaves both parts `Content-Transfer-Encoding: 7bit` with a literal Subject, while NON-ASCII rendering data moves both parts onto `Content-Transfer-Encoding: quoted-printable` (the bodies decoding back to exactly the substituted UTF-8 text) and emits the Subject as an RFC 2047 UTF-8 encoded word. The same recording shows a very long substituted value rendered as quoted-printable parts with soft line breaks ('=\r\n') at most every 76 octets, so un-folding the soft breaks recovers the whole value once per alternative.

Emulator: part_headers() in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs chooses only between two labels — `7bit` when the body is_ascii() and `8bit` otherwise — and never encodes or wraps a body. So non-ASCII data yields ['8bit', '8bit'] where AWS yields ['quoted-printable', 'quoted-printable'], and a 262144-byte rendering-data value produces a 524659-byte document with no 'Content-Transfer-Encoding: quoted-printable' and no soft line breaks at all (its lines exceed the 998-octet RFC 5322 limit).

Why: an `8bit` part with raw UTF-8 and unbounded line length is not transportable over a 7-bit SMTP path, which is precisely what quoted-printable exists to avoid; a client that feeds the preview to an MTA or compares it with what SES will actually send gets a different (and in the long-line case malformed) message. Clients that decode the preview with a MIME library see the declared encoding disagree with AWS's.

Fix: the operation's serializer — part_headers()/mime_document() in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs — must quoted-printable-encode (and soft-wrap) a part whose rendered text is not pure short-line US-ASCII and declare that encoding, keeping 7bit for the ASCII case.

Traces: sesv2.test-render-email-template-mime-encodes-non-ascii-rendering-data, sesv2.email-template-contract-limits-rejected.

=== Cause 4 of 5: TestRenderEmailTemplate's not-found refusal uses the template-store sentence instead of its own ===

AWS: Smithy models NotFoundException (HTTP 404, "The resource you attempted to access doesn't exist.") on TestRenderEmailTemplate. The status and code match; the MESSAGE does not. For a template name the account holds nothing under, SES words this operation's refusal 'Unable to find <NAME> for <ACCOUNT-ID>' — fixed text 'Unable to find <', '> for <' and '>', with NAME taken from the request's TemplateName path label and ACCOUNT-ID the caller's own 12-digit account. GetEmailTemplate and UpdateEmailTemplate word the SAME condition differently, 'Template NAME does not exist.' (both of those matched in this replay), so the two sentences must not be collapsed.

Emulator: the render path answers 404 NotFoundException with the store-wide sentence 'Template NAME does not exist.' — services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs builds its NotFoundException from crate::state::email_template_not_found(&name), the helper the Get/Update handlers share.

Why: a client that branches on the message (log triage, user-facing copy, or distinguishing "this account cannot see that template" from "no such template") reads AWS's account-scoped sentence on the render path and the resource sentence on the read path; here both paths say the same thing, so the distinction AWS draws is lost.

Fix: the operation's own refusal wording in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/test_render_email_template.rs — give the render path its own message template 'Unable to find <{template_name}> for <{account_id}>' (account id from the request's resolved account) instead of reusing crate::state::email_template_not_found.

Traces: sesv2.email-template-lifecycle.

=== Cause 5 of 5: an import job never reads its S3 data source, so a job whose S3Url names a bucket that does not exist settles COMPLETED instead of FAILED ===

AWS: Smithy models CreateImportJob -> 200 {JobId} and GetImportJob -> 200 {JobId, ImportDestination, ImportDataSource, FailureInfo, JobStatus, CreatedTimestamp, CompletedTimestamp, ProcessedRecordsCount, FailedRecordsCount}; JobStatus is the enum CREATED | PROCESSING | COMPLETED | FAILED | CANCELLED and FailureInfo is {FailedRecordsS3Url, ErrorMessage: "A message about why the job failed"}. Recorded behavior: outside the sandbox, a job created with ImportDataSource.S3Url pointing at a bucket that does not exist is accepted, becomes visible to GetImportJob/ListImportJobs, and reaches the terminal status FAILED (with FailureInfo explaining the unreadable source) — COMPLETED is reserved for "Job has completed processing successfully". (In a sandboxed account the whole surface is instead refused BadRequestException "Your account is still in the sandbox."; neither path ends COMPLETED.)

Emulator: the S3 source is never consulted at any point. services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/create_import_job.rs validates only the destination shape (and, for a contact-list destination, that the list exists), stores an ImportJobRecord and mints an id; the job's status is then advanced by the clock in crate::state (Store::settle / ImportJobRecord) to COMPLETED regardless of whether ImportDataSource.S3Url is readable, so GetImportJob reported the terminal status COMPLETED where AWS reports FAILED.

Why: a client gets a false success for an import that imported nothing. Retry/alerting logic keyed on FAILED never fires, FailureInfo.ErrorMessage is never populated, and a suppression-list or contact-list import silently appears to have applied — the exact failure mode an emulator is used to rehearse.

Fix: the import-job lifecycle — services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/create_import_job.rs together with the job state machine in crate::state (ImportJobRecord / Store::settle) that get_import_job.rs and list_import_jobs.rs read: resolve ImportDataSource.S3Url against the emulated S3 bucket/object when the job runs and settle the job to FAILED with FailureInfo (ErrorMessage) when the source cannot be read, reserving COMPLETED for a source that was actually processed.

Traces: sesv2.import-job-create-get-list-lifecycle.

## Evidence
Behavioral suite replay (149/211 passed). Surviving sesv2 failures — none marked ALREADY FILED or OWNED:

[mismatch] sesv2.test-render-email-template-does-not-html-escape-substituted-values
  AssertionError: the text/html part must carry the value verbatim under both stash forms -- this is the part where a Handlebars-backed emulator will escape -- got '<p>esc=&lt;b&gt;&amp;&quot;x&quot;&#x27;y&#x27;&lt;/b&gt; raw=<b>&"x"\'y\'</b></p>'
  Expected (AWS): '<p>esc=<b>&"x"\'y\'</b> raw=<b>&"x"\'y\'</b></p>', and no HTML entity anywhere in the message (the value occurs verbatim 5 times).

[mismatch] sesv2.test-render-email-template-assembles-a-multipart-alternative-mime-message
  AssertionError: each render assembles a new MIME message and must mint its own boundary, got the same token twice: ('----=_Part_33805e005483b892', '----=_Part_33805e005483b892')
  Expected (AWS): two different boundary tokens from two renders of the unchanged template/data (the hex digits above are one run's digest value, not a required literal).

[mismatch] sesv2.test-render-email-template-mime-encodes-non-ascii-rendering-data
  AssertionError: non-ASCII rendering data must move both parts off 7bit onto quoted-printable, got ['8bit', '8bit']
  Expected (AWS): ['quoted-printable', 'quoted-printable'] for non-ASCII data (bodies decoding back to the substituted UTF-8), ['7bit','7bit'] for ASCII data, and an RFC 2047 '=?UTF-8?...?=' Subject.

[mismatch] sesv2.email-template-contract-limits-rejected
  AssertionError: TestRenderEmailTemplate returns a MIME document with quoted-printable parts, got a 524659-byte body starting 'Subject: limit probe\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary="----=_Part_f82924d6dd367c3c"\r\n\r\n------=_Part_f82924d6dd367c3c\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-'
  Expected (AWS): the parts carry 'Content-Transfer-Encoding: quoted-printable' with soft line breaks ('=\r\n') at most every 76 octets. (The TemplateData over-limit and 500 KB template-quota refusals earlier in this check were satisfied; only the encoding assertion failed.)

[mismatch] sesv2.email-template-lifecycle
  AssertionError: expected the message 'Unable to find <sim2real-behav-tmpl-db977be0a8be> for <000000000000>', got 'Template sim2real-behav-tmpl-db977be0a8be does not exist.'
  Expected (AWS) for TestRenderEmailTemplate on a deleted template: 404 NotFoundException, message 'Unable to find <TEMPLATE-NAME> for <ACCOUNT-ID>' (fixed text 'Unable to find <', '> for <', '>'; the name is the request's and the account id the caller's). The same check's GetEmailTemplate and UpdateEmailTemplate legs, which require 'Template NAME does not exist.', passed.

[mismatch] sesv2.import-job-create-get-list-lifecycle
  AssertionError: expected FAILED for a non-existent S3 bucket, got COMPLETED
  Expected (AWS): the job created with ImportDataSource.S3Url='s3://<nonexistent-bucket>/data.csv' reaches terminal JobStatus FAILED (FailureInfo populated); COMPLETED means it processed successfully. Create/Get/List all answered 200 and the job was listed, so only the terminal status diverges.

## Steps to Reproduce
Causes 1-4 (one sesv2 client, no other service needed):
1. CreateEmailTemplate with TemplateContent={'Subject': 'S {{v}}', 'Text': 'esc={{v}} raw={{{v}}}', 'Html': '<p>esc={{v}} raw={{{v}}}</p>'}. Call TestRenderEmailTemplate with TemplateData='{"v": "<b>&\"x\"\'y\'</b>"}'. Expect the value verbatim in both parts and the subject (5 occurrences, no entities); observe the double-stash reference in the text/html part HTML-escaped. [Cause 1]
2. CreateEmailTemplate with Subject 'Hi {{name}}', Text 'plain {{name}}', Html '<p>html {{name}}</p>'. Call TestRenderEmailTemplate twice with the identical TemplateData='{"name": "Ada"}' and compare the multipart/alternative boundary parameters. Expect two different tokens; observe one token twice. [Cause 2]
3. On the same template, render TemplateData='{"v": "plain"}' and TemplateData='{"v": "caf\u00e9-\u65e5\u672c"}' and read each part's Content-Transfer-Encoding. Expect 7bit/7bit then quoted-printable/quoted-printable with an '=?UTF-8?' Subject; observe 7bit/7bit then 8bit/8bit with a raw Subject. Also render a template whose substituted value is ~262144 bytes and look for 'Content-Transfer-Encoding: quoted-printable' and '=\r\n' soft breaks; observe neither. [Cause 3]
4. DeleteEmailTemplate, then TestRenderEmailTemplate on the deleted name. Expect 404 NotFoundException 'Unable to find <NAME> for <ACCOUNT-ID>'; observe 'Template NAME does not exist.' (GetEmailTemplate/UpdateEmailTemplate correctly say 'Template NAME does not exist.'). [Cause 4]

Cause 5:
5. CreateImportJob with ImportDataSource={'DataFormat': 'CSV', 'S3Url': 's3://<a-bucket-that-does-not-exist>/data.csv'} and ImportDestination={'SuppressionListDestination': {'SuppressionListImportAction': 'DELETE'}}. Poll GetImportJob(JobId) until JobStatus is terminal. Expect FAILED with FailureInfo; observe COMPLETED.

Replay checks: sesv2.test-render-email-template-does-not-html-escape-substituted-values, sesv2.test-render-email-template-assembles-a-multipart-alternative-mime-message, sesv2.test-render-email-template-mime-encodes-non-ascii-rendering-data, sesv2.email-template-contract-limits-rejected, sesv2.email-template-lifecycle, sesv2.import-job-create-get-list-lifecycle.
