// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the tools the SERVICE provides: the browser and the code
//! interpreter whose ids `BrowserId` and `CodeInterpreterId` reserve
//! (`aws.browser.v1`, `aws.codeinterpreter.v1`), which every account holds without
//! anything having created one.
//!
//! The rule under test: a read of a reserved tool id reports the tool the service
//! provides — `READY`, ARN-stamped for the region the request was signed for, with the
//! resource type that carries no `-custom` and the account segment of the SERVICE rather
//! than of the caller — while a DELETE, which addresses only a tool this account created,
//! does not find it and changes nothing. The same rule bounds the tool: it holds none of
//! the state a CREATE keeps for the account's own tools (so the name it reports occupies
//! no created name), and every OTHER id — any id the shape admits for a created tool but
//! nothing has created — is still the modelled 404 on both the read and the delete route.
//!
//! The same rule decides what a tool the service owns REPORTS, in the inventory
//! (`ListBrowsers`) as in the read: the name and the description the SERVICE publishes for
//! the tool rather than values derived from the caller's account or from the tool's own id,
//! beside a tool the ACCOUNT created, which carries the account's id, the `-custom`
//! resource type and the values its create supplied.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised. The store is the one shared
//! engine, so what one request writes is what the next request reads.

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The account id every ARN of a resource this ACCOUNT created carries
/// (`engine::mock_account_id`).
pub(crate) const ACCOUNT: &str = "000000000000";

/// The account segment an ARN of a resource the SERVICE owns carries: the `aws`
/// alternative of `BrowserArn` / `CodeInterpreterArn`'s `(aws|[0-9]{12})`
/// (`smithy_query shape=BrowserArn,CodeInterpreterArn`), which goes with the bare
/// resource type the built-in tools are published under.
pub(crate) const SERVICE_OWNED_ACCOUNT: &str = "aws";

/// One tool the service provides, as the routes and responses spell it.
pub(crate) struct Tool {
    /// The case name every assertion of this row names.
    pub(crate) case: &'static str,
    /// The path segment the tool's routes hang off (`smithy_query
    /// operation=GetBrowser,GetCodeInterpreter`).
    pub(crate) route: &'static str,
    /// The id the tool's own id shape reserves for it.
    pub(crate) reserved_id: &'static str,
    /// The response member carrying the tool's id.
    pub(crate) id_member: &'static str,
    /// The response member carrying the tool's ARN.
    pub(crate) arn_member: &'static str,
    /// The ARN resource type of the SERVICE-provided tool: the spelling of
    /// `browser(-custom)?` / `code-interpreter(-custom)?` without `-custom`.
    pub(crate) arn_resource: &'static str,
    /// The `networkMode` the tool reports: the modelled default of its own network
    /// configuration shape (`smithy_query shape=BrowserNetworkConfiguration,
    /// CodeInterpreterNetworkConfiguration`).
    network_mode: &'static str,
    /// How the tool's own not-found sentence words a missing resource.
    not_found_message: fn(&str) -> String,
    /// The `name` the SERVICE publishes for this tool, which every report of it carries.
    pub(crate) published_name: &'static str,
    /// The `description` the SERVICE publishes for this tool, where it publishes one.
    pub(crate) published_description: Option<&'static str>,
    /// A `name` + `networkConfiguration` body that creates a tool of this kind, so the
    /// account's own tool can be told apart from the service's.
    pub(crate) create_body: fn(&str) -> Value,
}

/// The tools under test: every reserved id this service's model declares.
///
/// Source for the ids: the first alternative of `BrowserId` and of
/// `CodeInterpreterId` (`smithy_query operation=GetBrowser,GetCodeInterpreter`). Source for
/// the browser's published name and description: the inventory AWS answers `ListBrowsers`
/// with for the built-in tool.
pub(crate) fn tools() -> [Tool; 2] {
    [
        Tool {
            case: "the service-provided browser",
            route: "browsers",
            reserved_id: "aws.browser.v1",
            id_member: "browserId",
            arn_member: "browserArn",
            arn_resource: "browser",
            network_mode: "PUBLIC",
            not_found_message: |id| format!("Browser with ID {id} does not exist"),
            published_name: "AgentCore Browser Tool",
            published_description: Some("AWS built-in browser sandbox for secure web browsing"),
            create_body: |name| {
                json!({"name": name, "networkConfiguration": {"networkMode": "PUBLIC"}})
            },
        },
        Tool {
            case: "the service-provided code interpreter",
            route: "code-interpreters",
            reserved_id: "aws.codeinterpreter.v1",
            id_member: "codeInterpreterId",
            arn_member: "codeInterpreterArn",
            arn_resource: "code-interpreter",
            network_mode: "SANDBOX",
            not_found_message: |id| format!("Code interpreter with ID {id} does not exist"),
            // Nothing this crate can cite states a published name or description for the
            // built-in code interpreter, so it reports the reserved id's own spelling and
            // no description (`crate::state::SYSTEM_CODE_INTERPRETER_NAME`).
            published_name: "aws_codeinterpreter_v1",
            published_description: None,
            create_body: |name| {
                json!({"name": name, "networkConfiguration": {"networkMode": "SANDBOX"}})
            },
        },
    ]
}

/// The regions the reads are signed for, so the region the ARN carries is the request's
/// own rather than one baked into the handler. One is the emulator's default
/// (`us-east-1`), the others are not.
pub(crate) const REGIONS: [&str; 3] = ["us-east-1", "eu-west-2", "ap-northeast-3"];

/// One router over one engine, whose calls are signed as a principal the access decision
/// allows ([`crate::authorized_signing_identity`]): the rows below read what the OPERATIONS
/// answer, including one addressing a tool by the OTHER tool's reserved identifier, which the
/// published credential alone is refused for (`unnameable_identifier_authorization.rs`).
pub(crate) fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::authorized_signing_identity::given_authorized_caller(&engine);
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service in `region`. It is what the router
/// routes on, what the handlers read the ARN region from, and the identity it names.
fn authorization(region: &str) -> String {
    let key = crate::authorized_signing_identity::AUTHORIZED_KEY_ID;
    format!(
        "AWS4-HMAC-SHA256 Credential={key}/20240101/{region}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    region: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization(region))
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetBrowser` / `GetCodeInterpreter`: `GET /{route}/{id}`.
pub(crate) async fn get_tool(
    router: &MultiServiceRouter,
    tool: &Tool,
    id: &str,
    region: &str,
) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/{}/{id}", tool.route),
        region,
        None,
    )
    .await
}

/// `DeleteBrowser` / `DeleteCodeInterpreter`: `DELETE /{route}/{id}`.
async fn delete_tool(
    router: &MultiServiceRouter,
    tool: &Tool,
    id: &str,
    region: &str,
) -> (u16, Value) {
    send(
        router,
        "DELETE",
        &format!("/{}/{id}", tool.route),
        region,
        None,
    )
    .await
}

/// `CreateBrowser` / `CreateCodeInterpreter`: `PUT /{route}`.
pub(crate) async fn create_tool(
    router: &MultiServiceRouter,
    tool: &Tool,
    body: &Value,
    region: &str,
) -> (u16, Value) {
    send(router, "PUT", &format!("/{}", tool.route), region, Some(body)).await
}

/// `ListBrowsers`: `POST /browsers`, optionally filtered by `type` (`smithy_query
/// operation=ListBrowsers` binds `type` to the query string).
pub(crate) async fn list_browsers(
    router: &MultiServiceRouter,
    region: &str,
    kind: Option<&str>,
) -> (u16, Value) {
    let uri = match kind {
        Some(kind) => format!("/browsers?type={kind}"),
        None => "/browsers".to_string(),
    };
    send(router, "POST", &uri, region, Some(&json!({}))).await
}

/// The entry of `browserSummaries` naming `browser_id`, if the listing reports one.
pub(crate) fn browser_summary(listing: &Value, browser_id: &str) -> Option<Value> {
    listing
        .get("browserSummaries")?
        .as_array()?
        .iter()
        .find(|entry| text(entry, "browserId") == browser_id)
        .cloned()
}

/// The ARN a resource the SERVICE owns carries: the bare resource type under the
/// service's own account segment.
pub(crate) fn service_owned_arn(region: &str, arn_resource: &str, id: &str) -> String {
    format!("arn:aws:bedrock-agentcore:{region}:{SERVICE_OWNED_ACCOUNT}:{arn_resource}/{id}")
}

/// The ARN a resource THIS ACCOUNT created carries: the same form with the account's own
/// id and the `-custom` resource type.
pub(crate) fn account_owned_arn(region: &str, arn_resource: &str, id: &str) -> String {
    format!("arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:{arn_resource}-custom/{id}")
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `SandboxName`'s pattern `^[a-zA-Z][a-zA-Z0-9_]{0,47}$` (`smithy_query
/// operation=GetBrowser`), checked by hand because this crate carries no regex engine.
fn is_modelled_sandbox_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && name.len() <= 48
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Assert one `DateTimestamp` member's wire form: a JSON STRING in the canonical RFC
/// 3339 form, naming a real instant.
pub(crate) fn assert_date_time(case: &str, member: &str, value: &Value) {
    let rendered = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be an RFC 3339 date-time string, got {value:?}")
    });
    let parsed = DateTime::from_str(rendered, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must parse as a date-time ({error}), got {rendered:?}")
    });
    assert!(
        parsed.secs() > 0,
        "case {case:?}: {member} must name a real instant, got {rendered:?}"
    );
}

/// Every tool the service provides is readable by its reserved id in every region,
/// without anything having created one, and two reads report the same tool.
#[tokio::test]
async fn a_system_tool_is_readable_by_its_reserved_id_in_every_region() {
    let router = router();

    for tool in tools() {
        for region in REGIONS {
            let case = format!("{} in {region}", tool.case);
            let (status, read) = get_tool(&router, &tool, tool.reserved_id, region).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a reserved tool id is readable with nothing having created \
                 it, got {status} ({read})"
            );
            assert_eq!(
                text(&read, tool.id_member),
                tool.reserved_id,
                "case {case:?}: the read reports the reserved id it was asked for, got {}",
                text(&read, tool.id_member)
            );
            assert_eq!(
                text(&read, tool.arn_member),
                service_owned_arn(region, tool.arn_resource, tool.reserved_id),
                "case {case:?}: expected the service-provided ARN for {}, got {}",
                tool.reserved_id,
                text(&read, tool.arn_member)
            );
            assert_eq!(
                text(&read, "status"),
                "READY",
                "case {case:?}: a tool nobody created is READY, got {}",
                text(&read, "status")
            );
            assert_eq!(
                text(&read, "name"),
                tool.published_name,
                "case {case:?}: expected the name the SERVICE publishes for the tool, got {:?}",
                text(&read, "name")
            );
            assert_eq!(
                read["networkConfiguration"],
                json!({"networkMode": tool.network_mode}),
                "case {case:?}: expected the modelled default network mode {}, got {}",
                tool.network_mode,
                read["networkConfiguration"]
            );
            assert!(
                read["executionRoleArn"].is_null(),
                "case {case:?}: no caller supplied an execution role for a service-provided \
                 tool, got {}",
                read["executionRoleArn"]
            );
            assert!(
                read["failureReason"].is_null(),
                "case {case:?}: a READY tool reports no failureReason, got {}",
                read["failureReason"]
            );
            for member in ["createdAt", "lastUpdatedAt"] {
                assert_date_time(&case, member, &read[member]);
            }

            // A read neither creates the tool nor re-dates it: the next read of the same
            // id reports exactly the same resource.
            let (status, again) = get_tool(&router, &tool, tool.reserved_id, region).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the second read also answers HTTP 200, got {status} ({again})"
            );
            assert_eq!(
                again, read,
                "case {case:?}: a read does not change what a later read sees, got {again} \
                 against {read}"
            );
        }
    }
}

/// A delete addresses only a tool this account created, so one naming a reserved id is
/// the modelled 404 — whether sent once or repeated — and the tool is still READY after.
#[tokio::test]
async fn a_system_tool_is_not_deletable_and_the_refused_delete_changes_nothing() {
    let router = router();
    let region = REGIONS[0];

    for tool in tools() {
        let (_, before) = get_tool(&router, &tool, tool.reserved_id, region).await;

        for attempt in 1..=2 {
            let case = format!("{} (delete {attempt})", tool.case);
            let (status, refused) = delete_tool(&router, &tool, tool.reserved_id, region).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a delete of a reserved tool id is the modelled 404, got \
                 {status} ({refused})"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {} ({refused})",
                error_code(&refused)
            );
            assert_eq!(
                text(&refused, "message"),
                (tool.not_found_message)(tool.reserved_id),
                "case {case:?}: the refusal names the id the request carried, got {}",
                text(&refused, "message")
            );
        }

        let (status, after) = get_tool(&router, &tool, tool.reserved_id, region).await;
        assert_eq!(
            status, 200,
            "case {:?}: the tool is still readable after the refused delete, got {status} \
             ({after})",
            tool.case
        );
        assert_eq!(
            after, before,
            "case {:?}: a refused delete changes nothing about the tool, got {after} against \
             {before}",
            tool.case
        );
    }
}

/// A tool the ACCOUNT creates is its own resource beside the service-provided one: its
/// id is minted, its ARN carries the `-custom` resource type, and deleting it leaves the
/// service-provided tool exactly where it was.
#[tokio::test]
async fn a_created_tool_is_its_own_resource_beside_the_system_one() {
    let router = router();
    let region = REGIONS[1];

    for tool in tools() {
        let case = tool.case;
        let (status, created) =
            create_tool(&router, &tool, &(tool.create_body)("ownTool"), region).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from the create, got {status} ({created})"
        );
        let created_id = text(&created, tool.id_member);
        assert_ne!(
            created_id, tool.reserved_id,
            "case {case:?}: a create never mints the reserved id, got {created_id:?}"
        );
        assert_eq!(
            text(&created, tool.arn_member),
            account_owned_arn(region, tool.arn_resource, &created_id),
            "case {case:?}: a created tool carries the -custom resource type, got {}",
            text(&created, tool.arn_member)
        );

        // The created tool is deletable; the service-provided one is not, and the delete
        // of the account's own tool does not disturb it.
        let (status, deleted) = delete_tool(&router, &tool, &created_id, region).await;
        assert_eq!(
            status, 202,
            "case {case:?}: a tool this account created is deletable, got {status} ({deleted})"
        );

        let (status, system) = get_tool(&router, &tool, tool.reserved_id, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the service-provided tool is unaffected, got {status} ({system})"
        );
        assert_eq!(
            text(&system, "status"),
            "READY",
            "case {case:?}: the service-provided tool is still READY, got {}",
            text(&system, "status")
        );
    }
}

/// Ids of the form a CREATE mints — the second alternative of `BrowserId` /
/// `CodeInterpreterId`, `[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}` (`smithy_query
/// operation=GetBrowser,GetCodeInterpreter`) — that name no tool this account created.
///
/// One row per way the pattern's created alternative can be spelled: the shortest name
/// part, the longest, a name carrying the underscores it admits, and suffixes of digits,
/// of letters and of both.
fn ids_naming_no_created_tool() -> Vec<(String, String)> {
    vec![
        (
            "the shortest id the created alternative admits".to_string(),
            "a-0123456789".to_string(),
        ),
        (
            "an id whose name part is the longest the pattern admits".to_string(),
            format!("z{}-abcdefghij", "_".repeat(47)),
        ),
        (
            "an id whose name part carries underscores".to_string(),
            "tool_Name-ABCDEFGHIJ".to_string(),
        ),
        (
            "an id whose suffix mixes letters and digits".to_string(),
            "Tool9-a1b2C3d4e5".to_string(),
        ),
    ]
}

/// Only the RESERVED id resolves without a create: every other id the shape admits names
/// nothing, so both the read and the delete route answer the modelled 404 naming the id
/// the request carried, and the service-provided tool is still readable afterwards.
#[tokio::test]
async fn an_id_that_names_no_created_tool_is_the_modelled_404_on_read_and_on_delete() {
    let router = router();
    let region = REGIONS[2];

    for tool in tools() {
        for (form, id) in ids_naming_no_created_tool() {
            assert_ne!(
                id, tool.reserved_id,
                "case {form:?}: this row must not be the reserved id"
            );
            let answers = [
                ("read", get_tool(&router, &tool, &id, region).await),
                ("delete", delete_tool(&router, &tool, &id, region).await),
            ];
            for (verb, (status, refused)) in answers {
                let case = format!("{form} on the {} route ({verb})", tool.route);
                assert_eq!(
                    status, 404,
                    "case {case:?}: an id naming no created tool is the modelled 404, got \
                     {status} ({refused})"
                );
                assert_eq!(
                    error_code(&refused),
                    "ResourceNotFoundException",
                    "case {case:?}: expected ResourceNotFoundException, got {} ({refused})",
                    error_code(&refused)
                );
                assert_eq!(
                    text(&refused, "message"),
                    (tool.not_found_message)(&id),
                    "case {case:?}: the refusal names the id the request carried, got {}",
                    text(&refused, "message")
                );
            }
        }

        // None of those misses disturbed the tool the service provides.
        let case = tool.case;
        let (status, system) = get_tool(&router, &tool, tool.reserved_id, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the reserved id still resolves after ids that name nothing, got \
             {status} ({system})"
        );
        assert_eq!(
            text(&system, "status"),
            "READY",
            "case {case:?}: the service-provided tool is still READY, got {}",
            text(&system, "status")
        );
    }
}

/// The tool the service provides holds none of the state a CREATE keeps for the account's
/// own tools: the name a create may carry is free whatever the service-provided tool
/// reports (the uniqueness index "the name must be unique within your account" holds only
/// created names), the created tool is a second, distinct resource readable by its minted
/// id, and a repeat of THAT name is the conflict.
///
/// The name the create carries is the service-provided tool's own where `SandboxName`
/// admits it — the row where a collision with the service's name is expressible at all —
/// and otherwise a name of that pattern, because the name the SERVICE publishes for a tool
/// of its own is not bound by the pattern constraining the name a REQUEST may carry.
#[tokio::test]
async fn the_service_provided_tool_does_not_occupy_the_created_tool_name_index() {
    let router = router();
    let region = REGIONS[0];

    for tool in tools() {
        let case = tool.case;
        let (status, system) = get_tool(&router, &tool, tool.reserved_id, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the reserved id resolves before anything is created, got {status} \
             ({system})"
        );
        let system_name = text(&system, "name");
        let create_name = if is_modelled_sandbox_name(&system_name) {
            system_name
        } else {
            "builtinNameProbe".to_string()
        };

        let body = (tool.create_body)(&create_name);
        let (status, created) = create_tool(&router, &tool, &body, region).await;
        assert_eq!(
            status, 202,
            "case {case:?}: the name {create_name:?} is free for a created tool, got {status} \
             ({created})"
        );
        let created_id = text(&created, tool.id_member);
        assert_ne!(
            created_id, tool.reserved_id,
            "case {case:?}: a create never mints the reserved id, got {created_id:?}"
        );

        let (status, read_back) = get_tool(&router, &tool, &created_id, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created tool is readable by its minted id, got {status} \
             ({read_back})"
        );
        assert_eq!(
            text(&read_back, "name"),
            create_name,
            "case {case:?}: the created tool keeps the name the create supplied, got {}",
            text(&read_back, "name")
        );
        assert_eq!(
            text(&read_back, tool.arn_member),
            account_owned_arn(region, tool.arn_resource, &created_id),
            "case {case:?}: the created tool carries the -custom resource type, got {}",
            text(&read_back, tool.arn_member)
        );

        // That created name IS in the index now, so a second create of it conflicts —
        // which is how the first create proves the service-provided tool was never there.
        let (status, conflict) = create_tool(&router, &tool, &body, region).await;
        assert_eq!(
            status, 409,
            "case {case:?}: a repeated created name is the modelled conflict, got {status} \
             ({conflict})"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {} ({conflict})",
            error_code(&conflict)
        );

        let (status, after) = get_tool(&router, &tool, tool.reserved_id, region).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the service-provided tool is unaffected, got {status} ({after})"
        );
        assert_eq!(
            after, system,
            "case {case:?}: a create sharing its name changes nothing about the \
             service-provided tool, got {after} against {system}"
        );
    }
}

/// The reserved id of one tool is not an id of the OTHER tool's route: each id shape
/// reserves its own, so the boundary constraint walk refuses the swap.
#[tokio::test]
async fn one_tools_reserved_id_is_not_an_id_of_the_other_tools_route() {
    let router = router();
    let region = REGIONS[0];
    let [browser, code_interpreter] = tools();

    // (case, the route addressed, the id sent)
    let rows = [
        (
            "the interpreter's reserved id on the browser route",
            &browser,
            code_interpreter.reserved_id,
        ),
        (
            "the browser's reserved id on the interpreter route",
            &code_interpreter,
            browser.reserved_id,
        ),
    ];

    for (case, tool, id) in rows {
        let (status, refused) = get_tool(&router, tool, id, region).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an id the shape's pattern refuses is a 400, got {status} ({refused})"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {} ({refused})",
            error_code(&refused)
        );
    }
}
