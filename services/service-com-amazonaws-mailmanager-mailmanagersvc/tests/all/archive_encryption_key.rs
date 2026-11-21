// SPDX-License-Identifier: CC0-1.0

//! Whether an archive may be created under the KMS key its request names, end to end over
//! the wire, with Mail Manager and KMS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so every key and every key policy the decision
//! reads is one KMS's own handlers wrote.
//!
//! RULE: a create that would bind a resource to a customer managed KMS key resolves that key
//! in KMS, the component that owns keys, and is refused — with the modelled
//! `AccessDeniedException` (HTTP 403) and the fixed sentence the service publishes for it —
//! unless KMS reports a key whose own key policy lets this service perform the KMS operations
//! the resource needs (`kms:GenerateDataKey` to seal arriving mail, `kms:Decrypt` to serve it
//! back) for that resource. A refused create writes NOTHING: `ListArchives` never shows the
//! name, the name stays free and the request's `ClientToken` records no create.
//!
//! The recorded flow this was derived from creates an archive under a key ARN KMS holds no key
//! for, and one under a key whose policy does not name this service; both are rows of the
//! table below. The rows differ along what the rule generalises over: which document carries
//! the grant (an unconditioned grant, a wildcard action, a grant scoped by each of the three
//! condition keys such a policy is normally written with), which document refuses it (a grant
//! of only one of the two actions, a grant to another service principal, the default key
//! policy, an explicit deny, a grant scoped to another account or to another resource), the
//! form the create names the key by (a key ARN, an alias ARN), and whether KMS resolves the
//! identifier at all (a key ARN of no key, a key ARN naming another Region).
//!
//! Every Mail Manager case drives the real awsJson1_0 request path (`POST /` with
//! `X-Amz-Target: MailManagerSvc.<Operation>`) and every KMS case its awsJson1_1 one, through
//! `MultiServiceRouter::dispatch`, so the statuses, bodies and modelled errors asserted here
//! are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The sentence a refusal of an unusable key carries, spelled out here rather than read off
/// the implementation so a change of wording fails this suite.
const KEY_NOT_USABLE: &str = "Unable to use KMS key";

/// The service principal a key policy written for a Mail Manager archive names: Mail Manager
/// is the Amazon SES feature whose resources live in SES's own ARN namespace.
const MAIL_MANAGER_PRINCIPAL: &str = "ses.amazonaws.com";

/// The two KMS operations an archive needs its key for.
const ARCHIVE_ACTIONS: [&str; 2] = ["kms:GenerateDataKey", "kms:Decrypt"];

/// Mail Manager and KMS over ONE engine, so the key a create names is the record KMS's own
/// handlers hold.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, and the fixed test credential below is
        // not this process's signing key; the router still routes by the SigV4 credential
        // scope, which is where each request's Region comes from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One JSON call, signed for `REGION` under `signing_name`.
    async fn send(
        &self,
        signing_name: &str,
        content_type: &str,
        target: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", content_type)
            .header("x-amz-target", target)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20261117/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a valid HTTP request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            json!({})
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Mail Manager call.
    async fn mail(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.send(
            "ses",
            "application/x-amz-json-1.0",
            &format!("MailManagerSvc.{operation}"),
            body,
        )
        .await
    }

    /// One KMS call that must be answered 200.
    async fn kms(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, response) = self
            .send(
                "kms",
                "application/x-amz-json-1.1",
                &format!("TrentService.{operation}"),
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected KMS {operation} to answer 200, got {status} with body \
             {response}"
        );
        response
    }

    /// A key of this account carrying `policy` — or the default policy KMS writes when none is
    /// given — as its `(KeyId, Arn)`.
    async fn key(&self, case: &str, policy: Option<&Value>) -> (String, String) {
        let mut body = json!({});
        if let Some(policy) = policy {
            body["Policy"] = json!(policy.to_string());
        }
        let created = self.kms(case, "CreateKey", &body).await;
        let metadata = &created["KeyMetadata"];
        (
            metadata["KeyId"].as_str().unwrap_or_default().to_string(),
            metadata["Arn"].as_str().unwrap_or_default().to_string(),
        )
    }

    /// An alias of this account pointing at `key_id`, as its alias ARN — the other form
    /// `KmsKeyArn`'s pattern admits.
    async fn alias(&self, case: &str, alias_name: &str, key_id: &str) -> String {
        self.kms(
            case,
            "CreateAlias",
            &json!({ "AliasName": alias_name, "TargetKeyId": key_id }),
        )
        .await;
        format!("arn:aws:kms:{REGION}:{ACCOUNT}:{alias_name}")
    }

    /// Whether `ListArchives` shows an archive named `name`, walking EVERY page.
    async fn lists_archive(&self, case: &str, name: &str) -> bool {
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({ "PageSize": 50 });
            if let Some(next) = &token {
                body["NextToken"] = json!(next);
            }
            let (status, listed) = self.mail("ListArchives", &body).await;
            assert_eq!(
                status, 200,
                "case {case}: expected ListArchives to answer 200, got {status} {listed:?}"
            );
            if listed["Archives"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["ArchiveName"] == json!(name)))
            {
                return true;
            }
            token = listed["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                return false;
            }
        }
    }
}

/// The wire error code of a failure body: the `__type` member, without its namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// The message a failure body carries, in either spelling the wire uses.
fn error_message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A key policy whose statements under test are `statements`, beside the statement every key
/// policy has to carry: KMS refuses a document that would lock the account out of
/// `kms:PutKeyPolicy` ("The new key policy will not allow you to update the key policy in the
/// future"), and that statement grants the account nothing else — so what decides the archive
/// actions is the statements under test alone.
fn policy(statements: Vec<Value>) -> Value {
    let mut all = vec![json!({
        "Sid": "KeepTheAccountAbleToUpdateThisPolicy",
        "Effect": "Allow",
        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
        "Action": ["kms:PutKeyPolicy", "kms:GetKeyPolicy", "kms:DescribeKey"],
        "Resource": "*",
    })];
    all.extend(statements);
    json!({ "Version": "2012-10-17", "Statement": all })
}

/// One key-policy statement: `effect` on `actions` for `principal`, under `condition` (an
/// empty object for a statement with no `Condition` element). A key policy's `Resource` is
/// `*`, which means the key the policy is attached to.
fn statement(effect: &str, principal: Value, actions: Value, condition: Value) -> Value {
    let mut written = json!({
        "Effect": effect,
        "Principal": principal,
        "Action": actions,
        "Resource": "*",
    });
    if condition != json!({}) {
        written["Condition"] = condition;
    }
    written
}

/// A statement granting the Mail Manager service principal `actions` under `condition`.
fn grant(actions: Value, condition: Value) -> Value {
    statement(
        "Allow",
        json!({ "Service": MAIL_MANAGER_PRINCIPAL }),
        actions,
        condition,
    )
}

/// How the create names the key, and what KMS holds for it.
enum KeyUnder {
    /// The create names no key at all: there is nothing to resolve.
    Unnamed,
    /// A key carrying this policy, named by the key ARN KMS minted for it.
    Policy(Value),
    /// A key created with no policy of its own, so it carries the DEFAULT key policy KMS
    /// writes — "gives the Amazon Web Services account that owns the KMS key permission to
    /// use it", which names the account root and no service.
    DefaultPolicy,
    /// A key carrying this policy, named by an ALIAS ARN that resolves to it.
    AliasToPolicy(Value),
    /// A well-formed key ARN this account and Region hold no key for.
    AbsentArn,
    /// A key carrying this policy, named by a key ARN whose REGION field names another
    /// Region — where that key does not live, a KMS key being a regional resource.
    PolicyInAnotherRegion(Value),
}

/// One row: the key the create names, and whether the create is served.
struct Row {
    case: &'static str,
    key: KeyUnder,
    served: bool,
}

/// The rows the rule is checked over; see the module docs for the dimensions they span.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no key named at all",
            key: KeyUnder::Unnamed,
            served: true,
        },
        Row {
            case: "the service principal is granted both archive actions",
            key: KeyUnder::Policy(policy(vec![grant(json!(ARCHIVE_ACTIONS), json!({}))])),
            served: true,
        },
        Row {
            case: "the grant is written as the kms:* wildcard",
            key: KeyUnder::Policy(policy(vec![grant(json!("kms:*"), json!({}))])),
            served: true,
        },
        Row {
            case: "the grant is scoped to this account",
            key: KeyUnder::Policy(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } }),
            )])),
            served: true,
        },
        Row {
            case: "the grant is scoped to this service's regional endpoint",
            key: KeyUnder::Policy(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({
                    "StringEquals": { "kms:ViaService": format!("ses.{REGION}.amazonaws.com") }
                }),
            )])),
            served: true,
        },
        Row {
            case: "the grant is scoped by ArnLike to this account's archives",
            key: KeyUnder::Policy(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:ses:{REGION}:{ACCOUNT}:mail-manager-archive/*")
                    }
                }),
            )])),
            served: true,
        },
        Row {
            case: "the granting key is reached through an alias ARN",
            key: KeyUnder::AliasToPolicy(policy(vec![grant(json!(ARCHIVE_ACTIONS), json!({}))])),
            served: true,
        },
        Row {
            case: "only one of the two archive actions is granted",
            key: KeyUnder::Policy(policy(vec![grant(json!("kms:Decrypt"), json!({}))])),
            served: false,
        },
        Row {
            case: "the grant names another service principal",
            key: KeyUnder::Policy(policy(vec![statement(
                "Allow",
                json!({ "Service": "s3.amazonaws.com" }),
                json!(ARCHIVE_ACTIONS),
                json!({}),
            )])),
            served: false,
        },
        Row {
            case: "the default key policy, which names the account root alone",
            key: KeyUnder::DefaultPolicy,
            served: false,
        },
        Row {
            case: "an explicit deny stands beside the grant",
            key: KeyUnder::Policy(policy(vec![
                grant(json!(ARCHIVE_ACTIONS), json!({})),
                statement(
                    "Deny",
                    json!({ "Service": MAIL_MANAGER_PRINCIPAL }),
                    json!("kms:Decrypt"),
                    json!({}),
                ),
            ])),
            served: false,
        },
        Row {
            case: "the grant is scoped to another account",
            key: KeyUnder::Policy(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({ "StringEquals": { "aws:SourceAccount": "210987654321" } }),
            )])),
            served: false,
        },
        Row {
            case: "the grant is scoped to another resource of this service",
            key: KeyUnder::Policy(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:ses:{REGION}:{ACCOUNT}:receipt-rule-set/*")
                    }
                }),
            )])),
            served: false,
        },
        Row {
            case: "a well-formed key ARN KMS holds no key for",
            key: KeyUnder::AbsentArn,
            served: false,
        },
        Row {
            case: "a granting key named by an ARN of another Region",
            key: KeyUnder::PolicyInAnotherRegion(policy(vec![grant(
                json!(ARCHIVE_ACTIONS),
                json!({}),
            )])),
            served: false,
        },
    ]
}

/// An archive is created under the key a request names only when KMS reports a key whose key
/// policy lets this service use it for that archive; every other case is refused 403
/// `AccessDeniedException` with the fixed sentence, and creates nothing.
#[tokio::test]
async fn an_archive_is_created_only_under_a_key_mail_manager_may_use() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let name = format!("key-case-{index}");
        let key_arn = match &row.key {
            KeyUnder::Unnamed => None,
            KeyUnder::Policy(document) => Some(emulator.key(case, Some(document)).await.1),
            KeyUnder::DefaultPolicy => Some(emulator.key(case, None).await.1),
            KeyUnder::AliasToPolicy(document) => {
                let (key_id, _) = emulator.key(case, Some(document)).await;
                Some(
                    emulator
                        .alias(case, &format!("alias/archive-key-{index}"), &key_id)
                        .await,
                )
            }
            KeyUnder::AbsentArn => Some(format!(
                "arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-0000-0000-000000000000"
            )),
            KeyUnder::PolicyInAnotherRegion(document) => {
                let (_, arn) = emulator.key(case, Some(document)).await;
                Some(arn.replace(REGION, "us-east-1"))
            }
        };

        let mut body = json!({ "ArchiveName": name });
        if let Some(key_arn) = &key_arn {
            body["KmsKeyArn"] = json!(key_arn);
        }
        let (status, out) = emulator.mail("CreateArchive", &body).await;

        if !row.served {
            assert_eq!(
                status, 403,
                "case {case:?}: a key this service may not use must be refused 403, got \
                 {status} {out:?}"
            );
            assert_eq!(
                error_code(&out),
                "AccessDeniedException",
                "case {case:?}: unexpected error code, got {out:?}"
            );
            assert_eq!(
                error_message(&out),
                KEY_NOT_USABLE,
                "case {case:?}: expected the published sentence {KEY_NOT_USABLE:?}, got {out:?}"
            );
            assert!(
                !emulator.lists_archive(case, &name).await,
                "case {case:?}: a refused create must leave no archive named {name:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: a key this service may use must be served, got {status} {out:?}"
        );
        let archive_id = out["ArchiveId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateArchive returned no ArchiveId: {out:?}"))
            .to_string();

        // Read the record back through the implemented reader: the key the create was served
        // under is the key the archive reports.
        let (status, read) = emulator
            .mail("GetArchive", &json!({ "ArchiveId": archive_id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetArchive to answer 200, got {status} {read:?}"
        );
        assert_eq!(
            read["KmsKeyArn"].as_str(),
            key_arn.as_deref(),
            "case {case:?}: the archive must report the key the create named, got {read:?}"
        );
        assert!(
            emulator.lists_archive(case, &name).await,
            "case {case:?}: a served create must be listed under {name:?}, archive {archive_id}"
        );
    }
}

/// A create this refusal rejects writes nothing: the name stays free, the archive count is
/// unchanged and the request's `ClientToken` records no create — so the same request, with a
/// key this service may use, is served afterwards.
#[tokio::test]
async fn a_refused_create_leaves_the_archive_name_and_its_token_free() {
    // Rows: the two ways a key is unusable — KMS resolves it to nothing, and KMS resolves it
    // to a key whose policy grants this service nothing.
    let absent = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-1111-1111-1111-111111111111");
    for (case, make_key) in [
        ("a key ARN of no key", None),
        (
            "a key whose policy grants nothing",
            Some(policy(vec![grant(json!("kms:Encrypt"), json!({}))])),
        ),
    ] {
        let emulator = Emulator::new();
        let refused_key = match &make_key {
            None => absent.clone(),
            Some(document) => emulator.key(case, Some(document)).await.1,
        };
        let name = "reusable-name";
        let token = "token-0123456789";

        let (status, out) = emulator
            .mail(
                "CreateArchive",
                &json!({
                    "ArchiveName": name,
                    "ClientToken": token,
                    "KmsKeyArn": refused_key,
                }),
            )
            .await;
        assert_eq!(
            status, 403,
            "case {case:?}: the create must be refused 403, got {status} {out:?}"
        );
        let (status, listed) = emulator.mail("ListArchives", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListArchives to answer 200, got {status} {listed:?}"
        );
        assert_eq!(
            listed["Archives"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a refused create must store no archive, got {listed:?}"
        );

        // The same name and the same token, now under a key the policy grants.
        let usable = emulator
            .key(case, Some(&policy(vec![grant(json!(ARCHIVE_ACTIONS), json!({}))])))
            .await
            .1;
        let (status, out) = emulator
            .mail(
                "CreateArchive",
                &json!({
                    "ArchiveName": name,
                    "ClientToken": token,
                    "KmsKeyArn": usable,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refusal must have left the name and the token free, got \
             {status} {out:?}"
        );
        assert!(
            emulator.lists_archive(case, name).await,
            "case {case:?}: the archive created afterwards must be listed"
        );
    }
}
