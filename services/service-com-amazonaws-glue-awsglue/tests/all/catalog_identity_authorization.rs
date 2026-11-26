// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a Data Catalog request perform the action it names, end to
//! end over the wire.
//!
//! RULE: the Data Catalog publishes its catalog, its databases and its tables as IAM
//! resources, and every call that acts on one is authorized against the identity-based
//! policies in force on the principal that signed it — so a caller whose readable documents do
//! not allow `glue:<Operation>` on the resource the request ADDRESSES is refused
//! `AccessDeniedException`, naming the principal, the action and that resource, and one whose
//! documents allow it is served. The decision rests on the (principal, action, resource)
//! triple: a grant written for one database's tables says nothing about another's, an explicit
//! `Deny` refuses whatever else allows the call, and the account's own published credential —
//! which holds no IAM policies at all — is not bounded by it.
//!
//! IAM and STS are mounted beside this service on ONE engine, because the policies the decision
//! is taken from are the records `iam:CreateRole` / `iam:PutRolePolicy` wrote and the principal
//! a request speaks for is the session `sts:AssumeRole` minted: what a row is judged against is
//! what those services persisted, never anything this suite handed Glue.
//!
//! The rows differ along what the rule generalises over: WHICH catalog action is asked about (a
//! database read, a table read, a table write, a listing and a partition read), WHOSE policy
//! decides (a role granted every catalog action, one granted a single database's tables, one
//! carrying an explicit deny, one carrying no document at all, and the account's own
//! credential), and WHICH resource the request addresses (a covered database and table, and one
//! outside the grant's scope).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// The account every ARN of this suite names: the one canonical account id this emulator
/// presents.
fn account_id() -> String {
    engine::emulator_account_id().to_string()
}

/// The database the rows read and write in.
const DATABASE: &str = "authorized_lake";

/// The table inside it.
const TABLE: &str = "orders";

/// A database no row's grant covers, so a grant written for [`DATABASE`] is shown to decide the
/// resource it names and nothing else.
const OTHER_DATABASE: &str = "unlisted_lake";

/// One emulator carrying this service, IAM and STS over ONE shared engine: the policies in
/// force on a principal are IAM's records, and the principal a request speaks for is the
/// session STS minted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// The access key id a request that speaks for the ACCOUNT presents: one STS never minted, so
/// the request resolves to the account's own published identity.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// One Glue call (`awsJson1_1`, `AWSGlue.<Operation>`) signed with `access_key_id` in its
/// credential scope, so a row speaks as the principal that key belongs to.
async fn glue_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://glue.{REGION}.amazonaws.com/"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/glue/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("build the Glue request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsQuery call (IAM, STS), answering its body text; it must be served.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("build the awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup {action:?} must succeed, got {status} with {text}"
    );
    text
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the trip.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// Create the role `name` carrying `document` as its one inline policy (none for a role that
/// carries no document at all), open a session of it, and answer that session's access key id.
async fn session_of(
    router: &MultiServiceRouter,
    name: &str,
    document: Option<Value>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    if let Some(document) = document {
        query(
            router,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", format!("{name}-policy")),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    }
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{}:role/{name}", account_id()),
            ),
            ("RoleSessionName", "probe".to_string()),
        ],
    )
    .await;
    xml_element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("sts:AssumeRole must answer an AccessKeyId, got {assumed}"))
}

/// A policy allowing `actions` on `resources`.
fn allow(actions: &[&str], resources: &[String]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resources }],
    })
}

/// The ARN of a database of this account's catalog, as AWS addresses one.
fn database_arn(database: &str) -> String {
    format!("arn:aws:glue:{REGION}:{}:database/{database}", account_id())
}

/// The ARN of a table under it, as AWS addresses one.
fn table_arn(database: &str, table: &str) -> String {
    format!(
        "arn:aws:glue:{REGION}:{}:table/{database}/{table}",
        account_id()
    )
}

/// The table one row's `CreateTable` writes, named for that row so each write of a served row
/// stores a table of its own.
fn written_table(row: usize) -> String {
    format!("written_by_row_{row}")
}

/// The row of [`a_catalog_call_is_decided_by_the_callers_own_identity_policies`]'s table whose
/// write every policy refuses — the role carrying no document at all, which is the last one.
const REFUSED_WRITE_ROW: usize = 4;

/// One call a row makes: the operation, the body, and the action a refusal of it names.
struct Call {
    operation: &'static str,
    body: Value,
    action: &'static str,
}

/// The calls every row is asked to make — one per catalog surface the rule covers.
fn calls() -> Vec<Call> {
    vec![
        Call {
            operation: "GetDatabase",
            body: json!({ "Name": DATABASE }),
            action: "glue:GetDatabase",
        },
        Call {
            operation: "GetTable",
            body: json!({ "DatabaseName": DATABASE, "Name": TABLE }),
            action: "glue:GetTable",
        },
        Call {
            operation: "GetTables",
            body: json!({ "DatabaseName": DATABASE }),
            action: "glue:GetTables",
        },
        Call {
            operation: "GetPartitions",
            body: json!({ "DatabaseName": DATABASE, "TableName": TABLE }),
            action: "glue:GetPartitions",
        },
        Call {
            operation: "CreateTable",
            body: json!({
                "DatabaseName": DATABASE,
                "TableInput": { "Name": written_table(0) },
            }),
            action: "glue:CreateTable",
        },
    ]
}

/// The message a refusal carries, whatever member the envelope spells it in.
fn refusal_message(answered: &Value) -> String {
    answered["Message"]
        .as_str()
        .or_else(|| answered["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// RULE: a Data Catalog call a caller's own identity-based policies do not allow is refused
/// `AccessDeniedException` naming the principal, the action and the resource the request
/// addressed; one they allow is served.
#[tokio::test]
async fn a_catalog_call_is_decided_by_the_callers_own_identity_policies() {
    let router = emulator();
    // The database and the table every row addresses, created as the account itself — which
    // holds no IAM policies and is therefore not bounded by this decision.
    let (status, answered) = glue_as(
        &router,
        ACCOUNT_KEY,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the account's own credential must be served, got {status} with {answered}"
    );
    let (status, answered) = glue_as(
        &router,
        ACCOUNT_KEY,
        "CreateTable",
        &json!({ "DatabaseName": DATABASE, "TableInput": { "Name": TABLE } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the account's own credential must be served, got {status} with {answered}"
    );

    // Every catalog action, on every resource of this account's catalog.
    let everything = session_of(
        &router,
        "catalog-everything",
        Some(allow(&["glue:*"], &["*".to_string()])),
    )
    .await;
    // Every catalog action, but only on ONE database and its tables.
    let one_database = session_of(
        &router,
        "catalog-one-database",
        Some(allow(
            &["glue:*"],
            &[
                database_arn(DATABASE),
                table_arn(DATABASE, "*"),
                format!("arn:aws:glue:{REGION}:{}:catalog", account_id()),
            ],
        )),
    )
    .await;
    // A grant of everything, and an explicit deny of ONE action over it.
    let denied_one = session_of(
        &router,
        "catalog-denied-one",
        Some(json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": "glue:*", "Resource": "*" },
                { "Effect": "Deny", "Action": "glue:CreateTable", "Resource": "*" },
            ],
        })),
    )
    .await;
    // A role the account holds no document for at all.
    let no_document = session_of(&router, "catalog-no-document", None).await;

    for call in calls() {
        let operation = call.operation;
        // (case, the key the call is signed with, whether it is served, the clause a refusal of
        // it carries — which of IAM's two ways of reaching a deny withheld the grant)
        let rows: [(&str, &str, bool, &str); 5] = [
            (
                "the account's own credential",
                ACCOUNT_KEY,
                true,
                "",
            ),
            (
                "a role granted every catalog action",
                &everything,
                true,
                "",
            ),
            (
                "a role granted this database and its tables",
                &one_database,
                true,
                "",
            ),
            (
                "a role carrying an explicit deny of the write",
                &denied_one,
                call.action != "glue:CreateTable",
                "with an explicit deny in an identity-based policy",
            ),
            (
                "a role carrying no document at all",
                &no_document,
                false,
                "because no identity-based policy allows",
            ),
        ];
        for (index, (case, key, served, clause)) in rows.into_iter().enumerate() {
            // A served write really stores a table, so each row's write names a table of its
            // own: what a row establishes is the DECISION, not what another row left behind.
            let mut body = call.body.clone();
            if operation == "CreateTable" {
                body["TableInput"]["Name"] = json!(written_table(index));
            }
            let (status, answered) = glue_as(&router, key, operation, &body).await;
            if served {
                assert_ne!(
                    status, 400,
                    "case {case:?} / {operation}: expected the call to be served, got {status} \
                     with {answered}"
                );
                assert!(
                    !refusal_message(&answered).contains("not authorized"),
                    "case {case:?} / {operation}: expected no authorization refusal, got \
                     {answered}"
                );
                continue;
            }
            assert_eq!(
                answered["__type"].as_str(),
                Some("AccessDeniedException"),
                "case {case:?} / {operation}: expected the modelled refusal, got {status} with \
                 {answered}"
            );
            let message = refusal_message(&answered);
            for held in ["is not authorized to perform:", call.action, clause] {
                assert!(
                    message.contains(held),
                    "case {case:?} / {operation}: expected the refusal to carry {held:?}, got \
                     {message:?}"
                );
            }
        }
        // ... and the SAME grant decides nothing about a resource it does not name: the role
        // scoped to one database is refused the identical call addressed at another.
        let outside = match operation {
            "GetDatabase" => json!({ "Name": OTHER_DATABASE }),
            "GetTables" => json!({ "DatabaseName": OTHER_DATABASE }),
            "GetTable" => json!({ "DatabaseName": OTHER_DATABASE, "Name": TABLE }),
            "GetPartitions" => json!({ "DatabaseName": OTHER_DATABASE, "TableName": TABLE }),
            _ => json!({
                "DatabaseName": OTHER_DATABASE,
                "TableInput": { "Name": "written_by_a_row" },
            }),
        };
        let (status, answered) = glue_as(&router, &one_database, operation, &outside).await;
        assert_eq!(
            answered["__type"].as_str(),
            Some("AccessDeniedException"),
            "case \"a database the grant does not name\" / {operation}: expected the modelled \
             refusal, got {status} with {answered}"
        );
        let message = refusal_message(&answered);
        assert!(
            message.contains(OTHER_DATABASE) && message.contains(call.action),
            "case \"a database the grant does not name\" / {operation}: expected the refusal to \
             name the resource it addressed, got {message:?}"
        );
    }

    // The refused write left NOTHING behind: a call the decision refuses reaches no store, so
    // the table the last row's write would have created is not there for the account's own
    // reader either.
    let refused_table = written_table(REFUSED_WRITE_ROW);
    let (status, answered) = glue_as(
        &router,
        ACCOUNT_KEY,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": refused_table }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a table no refused write created must not be there, got {status} with {answered}"
    );
    assert_eq!(
        answered["__type"].as_str(),
        Some("EntityNotFoundException"),
        "a table no refused write created must be reported absent, got {answered}"
    );
}
