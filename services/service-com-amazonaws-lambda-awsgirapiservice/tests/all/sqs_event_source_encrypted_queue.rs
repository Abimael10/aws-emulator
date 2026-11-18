// SPDX-License-Identifier: CC0-1.0

//! What an ENABLED event source mapping CONSUMES of a queue whose messages are encrypted with
//! a CUSTOMER MANAGED KMS key, while the execution role of the function it invokes may — or
//! may not — have that key used on its behalf.
//!
//! RULE: a message of a queue on a customer managed key is sealed under a data key only KMS
//! can open, so handing one over performs `kms:Decrypt` on that key as the CONSUMER; a
//! consumer the key does not let decrypt — by the key's own policy, by the identity-based
//! policies in force on that consumer, by the grants issued to it, or because the key is gone
//! or in a state no cryptographic operation can use — is handed nothing, so it hands nothing
//! to the function and deletes nothing, and the batch it took is still the queue's own. A
//! mapping's poller is that consumer, reading as the invoked function's execution role
//! ("Lambda reads items from the event source and invokes the function",
//! `CreateEventSourceMapping`, as "the ARN of the function's execution role",
//! `FunctionConfiguration$Role`), so while the key withholds the decrypt no invocation is
//! made and no message leaves the queue, and the pass that follows the grant hands that same
//! message to the function, which is what consumes it.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with KMS,
//! IAM and Amazon SQS mounted beside Lambda on ONE engine: each key and key policy is written
//! through KMS's own `CreateKey`/`CreateGrant`/`DisableKey`, each role document through IAM's
//! own `CreateRole`/`PutRolePolicy`, each queue and each message through Amazon SQS's own API,
//! each function's package really RUNS the handler below, and the mapping is read back through
//! `GetEventSourceMapping` — so a pass that drained a queue it may not decrypt fails here.
//!
//! The rows differ along what the rule generalises over: WHERE the decrypt is granted (the
//! key's own policy, the role's identity policy, a KMS grant, nowhere), HOW that grant names
//! the action and the key (exactly, through a namespace wildcard, on another key of the
//! account), whether a grant is overturned by an explicit `Deny` in the key policy, which KMS
//! condition withholds the use of the key (no grant, a disabled key), and which server-side
//! encryption option the queue is on at all — the SSE-SQS twin, which needs nothing of KMS, is
//! the control.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_completion::kms_call;
use crate::event_source_mapping_role_access::{allow, given_role, iam, logs_only, QUEUE_READS};
use crate::event_source_mappings::{
    await_function_active, call, call_sqs, create_queue, get_mapping, held_until, messages_held,
    queue_message_counts, reports_no_attempt, router_on, DrivenPolling, LOOK_AGAIN,
};
use crate::event_source_poll_authorization::{
    given_enabled_mapping, BLOCKED_PASSES, STATE_ENABLED,
};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `VisibilityTimeout` every queue below hides a delivered message for, and the `Timeout`
/// of the function it maps onto: a queue only maps onto a function it outlasts, and a short
/// window makes a batch no hand-over consumed come back promptly.
const VISIBILITY_TIMEOUT: i64 = 3;

/// How long a row watches a QUEUE for what the passes owe it — a batch polled, handed to the
/// function and deleted, or one no hand-over consumed coming back.
///
/// Taken from what the EMULATOR allows one attempt rather than picked here: the wait for a
/// share of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]), with room for the several passes a row may need, because a
/// whole-workspace run shares its cores with every other suite.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(30));

/// The handler every function below runs: it reads the batch it was handed and reports every
/// record of it as processed, so a message that reached it is one the mapping deletes.
const PROCESSING_HANDLER: &str = r#"
def lambda_handler(event, context):
    return {"processed": [record["messageId"] for record in event["Records"]]}
"#;

/// The KMS action a consumer of an encrypted queue performs on the queue's key: the data key
/// the message was sealed under has to be opened before the body can be handed over. Source:
/// the `KmsMasterKeyId` attribute of `CreateQueue`, whose key "Amazon SQS ... uses ... to
/// encrypt and decrypt" a queue's messages.
const DECRYPT: &str = "Decrypt";

/// The name every role below carries its one inline policy under, so a row REPLACES that
/// document rather than adding a second one beside it — the name
/// [`given_role`] writes the first one under.
const POLICY_NAME: &str = "event-source-reads";

/// Lambda, KMS, IAM and Amazon SQS on ONE engine — the key where KMS holds it, the role where
/// IAM holds it, the queue where Amazon SQS holds it, and the mapping polling all of them
/// in-process, the shape `core/router` assembles — with the poll passes of its mappings the
/// suite's own ([`DrivenPolling`]).
///
/// A pass is background work and nothing a caller can read says whether one has happened yet,
/// so a row that waited a span of its own choosing for one would be betting on how fast the
/// host runs. A row makes each pass itself and awaits it, and then reads what that pass did.
fn driven_emulator() -> (MultiServiceRouter, DrivenPolling, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    let driven = DrivenPolling::claiming(&engine, &router);
    (router, driven, engine)
}

/// The statement the default key policy carries, which a row writing a key policy of its own
/// keeps beside its own statements: "the default key policy ... gives the Amazon Web Services
/// account that owns the KMS key permission to use it in all cryptographic operations", which
/// is also what keeps the document past the policy lockout safety check.
fn account_root_statement() -> Value {
    json!({
        "Sid": "Enable IAM User Permissions",
        "Effect": "Allow",
        "Principal": { "AWS": "arn:aws:iam::000000000000:root" },
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// What the KEY itself says about the role, in its own key policy.
enum KeyPolicy {
    /// The policy KMS writes for a key created without one, which delegates to the owning
    /// account's identity-based policies.
    Delegating,
    /// The default statement with an `Allow` of the decrypt NAMING the role, so the key
    /// permits it whatever the role's own documents say.
    NamesTheRole,
    /// The default statement with an explicit `Deny` of the decrypt for the role.
    DeniesTheRole,
}

impl KeyPolicy {
    /// The document to create the key with, or `None` for the default policy KMS writes.
    fn document(&self, role_arn: &str) -> Option<String> {
        let statements = match self {
            Self::Delegating => return None,
            Self::NamesTheRole => json!([
                account_root_statement(),
                {
                    "Sid": "LetTheRoleDecrypt",
                    "Effect": "Allow",
                    "Principal": { "AWS": role_arn },
                    "Action": format!("kms:{DECRYPT}"),
                    "Resource": "*",
                },
            ]),
            Self::DeniesTheRole => json!([
                account_root_statement(),
                {
                    "Sid": "KeepTheRoleOut",
                    "Effect": "Deny",
                    "Principal": { "AWS": role_arn },
                    "Action": format!("kms:{DECRYPT}"),
                    "Resource": "*",
                },
            ]),
        };
        Some(json!({ "Version": "2012-10-17", "Statement": statements }).to_string())
    }
}

/// What the ROLE's own identity-based policy grants of KMS, beside the queue's reads.
enum DecryptGrant {
    /// Nothing of KMS at all.
    Nothing,
    /// The decrypt, on the key the queue is on.
    OnTheKey,
    /// The decrypt, on ANOTHER key of the same account.
    OnAnotherKey,
    /// Every action of the KMS namespace, on every resource.
    NamespaceWildcard,
}

impl DecryptGrant {
    /// The statement this grant adds, for a queue on `key_arn` with `other_key_arn` beside
    /// it, or `None` when it adds none.
    fn statement(&self, key_arn: &str, other_key_arn: &str) -> Option<Value> {
        match self {
            Self::Nothing => None,
            Self::OnTheKey => Some(allow("kms", &[DECRYPT], key_arn)),
            Self::OnAnotherKey => Some(allow("kms", &[DECRYPT], other_key_arn)),
            Self::NamespaceWildcard => Some(allow("kms", &["*"], "*")),
        }
    }
}

/// One row: how the queue is encrypted, what the key says about the role, what the role's own
/// documents grant of KMS, whether a KMS grant on the key was issued to the role, whether the
/// key is still usable when the pass runs, and whether the mapping may then take the message.
struct Row {
    case: &'static str,
    /// The control: a queue on the Amazon SQS owned key (`SqsManagedSseEnabled`), which needs
    /// nothing of KMS, rather than on a customer managed key.
    sqs_managed: bool,
    key_policy: KeyPolicy,
    grant: DecryptGrant,
    /// Whether a KMS GRANT of the decrypt on the key is issued to the role — "a grant is a
    /// policy instrument that allows Amazon Web Services principals to use KMS keys"
    /// (`CreateGrant`), which is permission of its own beside the two policies.
    kms_grant: bool,
    /// Whether the key is disabled after the message is sent and before the pass is made.
    disable_key: bool,
    /// Whether the pass must take the message out of the queue.
    polls: bool,
}

/// The catalog: the three places a decrypt can be granted, the two forms of grant that miss
/// the key, the explicit deny that overturns a grant, the key state that withholds every use
/// of the key, and the SSE-SQS control.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the role's own policy grants the decrypt on the key",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::OnTheKey,
            kms_grant: false,
            disable_key: false,
            polls: true,
        },
        Row {
            case: "a KMS grant of the decrypt issued to the role, which is granted nothing \
                   else",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::Nothing,
            kms_grant: true,
            disable_key: false,
            polls: true,
        },
        Row {
            case: "the role's own policy grants every KMS action through a namespace wildcard",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::NamespaceWildcard,
            kms_grant: false,
            disable_key: false,
            polls: true,
        },
        Row {
            case: "the key's own policy names the role, which is granted nothing of KMS",
            sqs_managed: false,
            key_policy: KeyPolicy::NamesTheRole,
            grant: DecryptGrant::Nothing,
            kms_grant: false,
            disable_key: false,
            polls: true,
        },
        Row {
            case: "a queue on the Amazon SQS owned key, with no KMS grant anywhere",
            sqs_managed: true,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::Nothing,
            kms_grant: false,
            disable_key: false,
            polls: true,
        },
        Row {
            case: "no decrypt granted anywhere",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::Nothing,
            kms_grant: false,
            disable_key: false,
            polls: false,
        },
        Row {
            case: "the decrypt granted on another key of the account",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::OnAnotherKey,
            kms_grant: false,
            disable_key: false,
            polls: false,
        },
        Row {
            case: "the key's own policy denies the role beside the role's own grant",
            sqs_managed: false,
            key_policy: KeyPolicy::DeniesTheRole,
            grant: DecryptGrant::OnTheKey,
            kms_grant: false,
            disable_key: false,
            polls: false,
        },
        Row {
            case: "the key disabled while the role may decrypt",
            sqs_managed: false,
            key_policy: KeyPolicy::Delegating,
            grant: DecryptGrant::OnTheKey,
            kms_grant: false,
            disable_key: true,
            polls: false,
        },
    ]
}

/// `CreateGrant` of the decrypt on the key `key_id` names to the principal `grantee`: "a grant
/// is a policy instrument that allows Amazon Web Services principals to use KMS keys in
/// cryptographic operations", which is permission of its own beside the key's policy and the
/// grantee's own documents.
async fn created_grant(router: &MultiServiceRouter, case: &str, key_id: &str, grantee: &str) {
    let granted = kms_call(
        router,
        "CreateGrant",
        &json!({
            "KeyId": key_id,
            "GranteePrincipal": grantee,
            "Operations": [DECRYPT],
        }),
    )
    .await;
    assert_eq!(
        granted.status,
        200,
        "case {case:?}: expected CreateGrant to answer 200, got {} {}",
        granted.status,
        String::from_utf8_lossy(&granted.body)
    );
}

/// A symmetric encryption key this account holds, created with the key policy `policy` names
/// (`None` for the default policy KMS writes), as `(key id, key ARN)`.
async fn created_key(
    router: &MultiServiceRouter,
    case: &str,
    policy: Option<String>,
) -> (String, String) {
    let mut request = json!({ "Description": format!("the key for {case}") });
    if let Some(policy) = policy {
        request["Policy"] = json!(policy);
    }
    let created = kms_call(router, "CreateKey", &request).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected CreateKey to answer 200, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let answered = created.json();
    let read = |member: &str| {
        answered
            .pointer(&format!("/KeyMetadata/{member}"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: CreateKey answered no {member}: {}",
                    String::from_utf8_lossy(&created.body)
                )
            })
            .to_string()
    };
    (read("KeyId"), read("Arn"))
}

/// `DisableKey` on the key `key_id` names: "sets the state of a KMS key to disabled", which
/// takes it out of every cryptographic operation.
async fn disable_key(router: &MultiServiceRouter, case: &str, key_id: &str) {
    let disabled = kms_call(router, "DisableKey", &json!({ "KeyId": key_id })).await;
    assert_eq!(
        disabled.status,
        200,
        "case {case:?}: expected DisableKey to answer 200, got {} {}",
        disabled.status,
        String::from_utf8_lossy(&disabled.body)
    );
}

/// Replace the role's one inline policy with `statements`, through IAM's own API, so what the
/// gate reads is the document a client wrote.
async fn write_policy(router: &MultiServiceRouter, case: &str, role_name: &str, statements: Value) {
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, answered) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the row's policy must be written through IAM, got {status}: {answered}"
    );
}

/// Create the function `name`, running the handler above as the execution role `role_arn`,
/// and wait until it reports `Active`: a mapping polls nothing until its function can be
/// invoked at all.
///
/// It is [`crate::event_source_mappings::create_running_function`] with the ROLE the row
/// chose, which is the whole point here — the poller reads the queue, and asks KMS for the
/// queue's key, as this role.
async fn create_running_function_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": VISIBILITY_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(PROCESSING_HANDLER)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// `SendMessage` of `body` to the queue `url` addresses.
async fn send_message(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, sent) = call_sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SendMessage to answer 200, got {status} {sent}"
    );
}

/// The statements a role carries to read the queue `queue_arn` names, with the row's own KMS
/// grant beside them.
fn role_statements(queue_arn: &str, kms_grant: Option<Value>) -> Value {
    let mut statements = vec![logs_only(), allow("sqs", &QUEUE_READS, queue_arn)];
    statements.extend(kms_grant);
    json!(statements)
}

#[tokio::test]
async fn a_mapping_consumes_an_encrypted_queue_only_while_its_role_may_decrypt_it() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (router, mut driven, _engine) = driven_emulator();

        // The role first, so the key policy that names it names a principal this account
        // really holds; its documents are replaced with the row's own below.
        let role_name = format!("esm-kms-role-{index}");
        let role = given_role(&router, case, &role_name, Some(json!([logs_only()]))).await;
        let (key_id, key_arn) = created_key(&router, case, row.key_policy.document(&role)).await;
        // Another key of the same account, for the row whose grant names the wrong one.
        let (_, other_key_arn) = created_key(&router, case, None).await;

        // The queue, on the encryption option the row states: the customer managed key, or
        // the Amazon SQS owned one for the control.
        let mut attributes = json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() });
        if row.sqs_managed {
            attributes["SqsManagedSseEnabled"] = json!("true");
        } else {
            attributes["KmsMasterKeyId"] = json!(key_arn);
        }
        let (url, queue_arn) =
            create_queue(&router, case, &format!("esm-kms-queue-{index}"), &attributes).await;
        write_policy(
            &router,
            case,
            &role_name,
            role_statements(&queue_arn, row.grant.statement(&key_arn, &other_key_arn)),
        )
        .await;
        if row.kms_grant {
            created_grant(&router, case, &key_id, &role).await;
        }

        let function = format!("esm-kms-fn-{index}");
        create_running_function_as(&router, case, &function, &role).await;
        let uuid = given_enabled_mapping(
            &router,
            case,
            &json!({ "FunctionName": function, "EventSourceArn": queue_arn, "BatchSize": 1 }),
        )
        .await;

        // The message goes in while the key is still usable — a producer of a queue on a
        // disabled key is refused too — and only then is the key put into the state the row
        // states.
        send_message(&router, case, &url, &format!("{case} payload")).await;
        if row.disable_key {
            disable_key(&router, case, &key_id).await;
        }

        if row.polls {
            // The passes this row needs, each awaited: one reads the queue as the role, opens
            // the data key, takes the batch, hands it to the function and deletes what the
            // attempt processed, all before it answers. The row looks again until the queue
            // holds nothing, so a host that makes the run wait for capacity delays the
            // observation instead of weakening it.
            let counts = polled_until(&router, case, &url, &mut driven, 0).await;
            assert_eq!(
                counts,
                (0, 0, 0),
                "case {case:?}: the queue's key may be used for the mapping's role, so the \
                 message must be handed to the function and deleted, got {counts:?} as \
                 (visible, in flight, delayed)"
            );
            let reading = get_mapping(&router, &uuid).await;
            assert_eq!(
                reading.string("LastProcessingResult").as_deref(),
                Some(LAST_PROCESSING_RESULT_OK),
                "case {case:?}: the handler reported every record of the batch as processed, \
                 so the mapping must report that attempt as the successful one it was, got {}",
                reading.text()
            );
            continue;
        }

        // The complement: a batch the role may not have decrypted reaches no function and is
        // deleted by nobody, so the message is still the queue's own after every pass — in
        // flight while the hold the delivery took lasts — and no attempt at a batch is
        // reported, because no batch was handed to the function.
        for pass in 0..BLOCKED_PASSES {
            driven.poll_once().await;
            let counts = queue_message_counts(&router, case, &url).await;
            assert_eq!(
                messages_held(counts),
                1,
                "case {case:?}: the queue's key may not be used for the mapping's role, so \
                 pass {pass} must leave the message in the queue, got {counts:?} as (visible, \
                 in flight, delayed)"
            );
            let reading = get_mapping(&router, &uuid).await;
            assert!(
                reports_no_attempt(&reading),
                "case {case:?}: no batch reached the function on pass {pass}, so there is no \
                 processing attempt to report, got {}",
                reading.text()
            );
        }
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            reading.string("State").as_deref(),
            Some(STATE_ENABLED),
            "case {case:?}: a mapping whose queue it may not decrypt is still the enabled \
             mapping it was, got {}",
            reading.text()
        );
    }
}

/// Make passes until the queue `url` addresses holds `wanted` messages, and answer the counts
/// it reported last — its `(visible, in flight, delayed)` as ONE answer.
///
/// A pass hands its batch to a function, and what an invocation waits for is the emulator's
/// own ([`WATCH_BUDGET`]), so a row waits for the CONDITION rather than for a span: it looks
/// again until the queue says what the rule owes or the budget runs out, and the assertion
/// that follows reads the last counts either way.
async fn polled_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    driven: &mut DrivenPolling,
    wanted: u32,
) -> (u32, u32, u32) {
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        driven.poll_once().await;
        let counts = queue_message_counts(router, case, url).await;
        if messages_held(counts) == wanted || Instant::now() >= deadline {
            return counts;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

#[tokio::test]
async fn the_message_an_undecryptable_queue_kept_is_delivered_once_the_decrypt_is_granted() {
    // The other side of the rule: the key is consulted afresh on every pass, so the grant
    // written after the message arrived delivers THAT message — the one the blocked passes
    // left in the queue.
    let case = "the decrypt granted after the passes that could not decrypt";
    let body = "held-until-the-role-may-decrypt";
    let (router, mut driven, engine) = driven_emulator();
    let role_name = "esm-kms-granted-role";
    let role = given_role(&router, case, role_name, Some(json!([logs_only()]))).await;
    let (_, key_arn) = created_key(&router, case, None).await;
    let (url, queue_arn) = create_queue(
        &router,
        case,
        "esm-kms-granted-queue",
        &json!({
            "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string(),
            "KmsMasterKeyId": key_arn,
        }),
    )
    .await;
    write_policy(&router, case, role_name, role_statements(&queue_arn, None)).await;
    create_running_function_as(&router, case, "esm-kms-granted-fn", &role).await;
    given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": "esm-kms-granted-fn",
            "EventSourceArn": queue_arn,
            "BatchSize": 1,
        }),
    )
    .await;

    send_message(&router, case, &url, body).await;
    for pass in 0..BLOCKED_PASSES {
        driven.poll_once().await;
        let counts = queue_message_counts(&router, case, &url).await;
        assert_eq!(
            messages_held(counts),
            1,
            "case {case:?}: while the role may not decrypt it, pass {pass} must leave the \
             message in the queue, got {counts:?} as (visible, in flight, delayed)"
        );
    }
    // The body is still the one that was sent: nothing consumed it, and nothing rewrote it.
    // Read out of Amazon SQS's own store, because a receive of this suite's own would be a
    // delivery — it would hide what it read from the mapping's next pass.
    let waiting: Vec<String> = held_until(&engine, &queue_arn).into_keys().collect();
    assert_eq!(
        waiting,
        vec![body.to_string()],
        "case {case:?}: the message the passes could not decrypt must still be the queue's \
         own, got {waiting:?}"
    );

    // And the grant: the same role, now allowed the decrypt on the same key, is handed that
    // same message — so the queue drains, and the function that processed it is what emptied
    // it.
    write_policy(
        &router,
        case,
        role_name,
        role_statements(&queue_arn, Some(allow("kms", &[DECRYPT], &key_arn))),
    )
    .await;
    let counts = polled_until(&router, case, &url, &mut driven, 0).await;
    assert_eq!(
        counts,
        (0, 0, 0),
        "case {case:?}: once the decrypt is granted the passes must hand that same message to \
         the function and delete it, got {counts:?} as (visible, in flight, delayed)"
    );
}

