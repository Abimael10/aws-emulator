// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the AWS-MANAGED JOB TEMPLATE catalog:
//! `ListManagedJobTemplates` and `DescribeManagedJobTemplate`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `GET /managed-job-templates[/{templateName}]` request signed for the `iot`
//! signing name — so what is asserted is what an AWS SDK client would see, not an
//! internal function's return value.
//!
//! The rules under test:
//!
//! * the catalog is PUBLISHED data: every member of an entry — its description, the
//!   document of its version and the `documentParameters` that document leaves open — is
//!   the SERVICE's text, the same for every caller, so a client that renders the catalog
//!   or validates a parameter value against a published `regex` reads the service's rule
//!   and not one this emulator invented;
//! * both operations report the same entries on a store nothing has written, no request
//!   creates or changes one, and reading one changes nothing a later caller sees;
//! * a listing carries ONE ENTRY PER (template, version) PAIR in the order the service
//!   PUBLISHES the catalog in — which is not alphabetical — each with the ARN of that
//!   version (`arn:aws:iot:<region>::jobtemplate/<name>:<version>`, with the EMPTY
//!   account segment of AWS-managed data) scoped to the region the request was signed
//!   for;
//! * `templateName` narrows the listing to that template's versions, and a `templateName`
//!   of the modelled shape that names no published template is a NOT-FOUND
//!   (`ResourceNotFoundException`, 404) with the one sentence every operation that looks
//!   a `templateName` up words a miss with;
//! * `maxResults` bounds the page and `nextToken` resumes the SAME listing exactly once
//!   per entry, for EVERY page size from one to past the number of entries;
//! * every listed entry is DESCRIBABLE: the describe reports the listing's five members
//!   unchanged plus the `document` of the requested version and the `documentParameters`
//!   that document leaves open — every placeholder in the document is a declared
//!   parameter and every required parameter has a placeholder;
//! * `templateVersion` selects the version, a request that names none gets the default
//!   (latest) version, and a version the published template does not publish is refused
//!   `InvalidRequestException` (400) with a sentence naming the version that was asked
//!   for.
//!
//! The tables span the class each rule generalises over — every entry of the catalog,
//! every page size, both filter states, the versions a request can ask for and more than
//! one signed region — rather than one recorded request.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region used wherever a case is not varying the region.
const REGION: &str = "eu-west-2";

/// `InvalidRequestException` as this service NAMES it in `x-amzn-errortype`: the shape
/// name, a `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The wire error shape of a managed job template the catalog does not publish.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// One `documentParameter` the catalog publishes: the pattern a caller replaces and the
/// guidance published with it. `example` is `None` for a parameter the service publishes
/// no example for, which is then absent from the response.
struct PublishedParameter {
    key: &'static str,
    description: &'static str,
    regex: &'static str,
    example: Option<&'static str>,
    optional: bool,
}

/// Which user a step runs as: published by EVERY managed template, because every one of
/// them runs something.
const RUN_AS_USER: PublishedParameter = PublishedParameter {
    key: "runAsUser",
    description: "Execute handler as another user. If not specified, then handler is \
                  executed as the same user as device client.",
    regex: "(.){0,256}",
    example: Some("user1"),
    optional: true,
};

/// Where the device client's job handlers live: published by every template whose step
/// RUNS a handler, and by no other.
const PATH_TO_HANDLER: PublishedParameter = PublishedParameter {
    key: "pathToHandler",
    description: "Path to handler on the device. If not specified, then device client \
                  will use the current working directory.",
    regex: "(.){0,4096}",
    example: Some("/path/to/handler/script"),
    optional: true,
};

/// One entry the catalog publishes: the template's name and description, the one step
/// its document runs, and its OWN document parameters in the order the document passes
/// them to that step.
///
/// Written out by hand rather than read from the crate, so the test pins the published
/// surface instead of restating the table that produces it. Source: the managed job
/// templates AWS publishes for the AWS IoT Device Client, of which `AWS-Reboot` is the
/// one the model itself names
/// (`smithy_query(query="doc",shape="DescribeManagedJobTemplateResponse",member="templateName")`:
/// "The unique name of a managed template, such as AWS-Reboot").
struct PublishedTemplate {
    template_name: &'static str,
    description: &'static str,
    /// The `name` of the one step the template's document runs.
    step_name: &'static str,
    /// The device client's job handler that step runs, or `None` for a step that runs no
    /// handler but the command the caller supplies.
    handler: Option<&'static str>,
    parameters: &'static [PublishedParameter],
}

impl PublishedTemplate {
    /// The step's `type`: a template that names a handler runs it, and one that names
    /// none runs the caller's command.
    fn step_type(&self) -> &'static str {
        if self.handler.is_some() {
            "runHandler"
        } else {
            "runCommand"
        }
    }

    /// Every parameter this template publishes, in order: its own, then the ones that
    /// come with the kind of step it runs.
    fn published_parameters(&self) -> Vec<&PublishedParameter> {
        let mut parameters: Vec<&PublishedParameter> = self.parameters.iter().collect();
        parameters.push(&RUN_AS_USER);
        if self.handler.is_some() {
            parameters.push(&PATH_TO_HANDLER);
        }
        parameters
    }
}

/// The expression AWS publishes for a space-separated list of name tokens, which is how
/// the device client's handlers take a list of packages or of system services.
const NAME_LIST_REGEX: &str = r"([a-zA-Z0-9+_\-\.]+[\s]?)+";

/// The whole published catalog, in the order the service publishes it — the order a
/// listing must report it in, which is NOT the alphabetical order of the names.
const PUBLISHED: &[PublishedTemplate] = &[
    PublishedTemplate {
        template_name: "AWS-Run-Command",
        description: "A managed job template for running a shell command.",
        step_name: "Run-Command",
        handler: None,
        parameters: &[PublishedParameter {
            key: "command",
            description: "A comma separated string of command, any comma contained in \
                          the command itself needs to be escaped.",
            regex: "(.){0,4096}",
            example: Some(r"echo,Hello\,World!"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Reboot",
        description: "A managed job template for rebooting the device.",
        step_name: "Reboot",
        handler: Some("reboot.sh"),
        parameters: &[],
    },
    PublishedTemplate {
        template_name: "AWS-Remove-Application",
        description: "A managed job template for uninstalling one or more applications.",
        step_name: "Remove-Application",
        handler: Some("remove-packages.sh"),
        parameters: &[PublishedParameter {
            key: "packages",
            description: "A space separated list of packages to uninstall.",
            regex: NAME_LIST_REGEX,
            example: Some("lftp dos2unix tcpdump"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Stop-Application",
        description: "A managed job template for stopping one or more system services.",
        step_name: "Stop-Application",
        handler: Some("stop-services.sh"),
        parameters: &[PublishedParameter {
            key: "services",
            description: "A space separated list of system services to stop.",
            regex: NAME_LIST_REGEX,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Start-Application",
        description: "A managed job template for starting one or more system services.",
        step_name: "Start-Application",
        handler: Some("start-services.sh"),
        parameters: &[PublishedParameter {
            key: "services",
            description: "A space separated list of system services to start.",
            regex: NAME_LIST_REGEX,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Restart-Application",
        description: "A managed job template for restarting one or more system services.",
        step_name: "Restart-Application",
        handler: Some("restart-services.sh"),
        parameters: &[PublishedParameter {
            key: "services",
            description: "A space separated list of system services to restart.",
            regex: NAME_LIST_REGEX,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Install-Application",
        description: "A managed job template for installing one or more applications.",
        step_name: "Install-Application",
        handler: Some("install-packages.sh"),
        parameters: &[PublishedParameter {
            key: "packages",
            description: "A space separated list of packages to install.",
            regex: NAME_LIST_REGEX,
            example: Some("lftp dos2unix tcpdump"),
            optional: false,
        }],
    },
    PublishedTemplate {
        template_name: "AWS-Download-File",
        description: "A managed job template for downloading a file.",
        step_name: "Download-File",
        handler: Some("download-file.sh"),
        parameters: &[
            PublishedParameter {
                key: "downloadUrl",
                description: "URL of file to download.",
                regex: "(.*?)",
                example: None,
                optional: false,
            },
            PublishedParameter {
                key: "filePath",
                description: "Path on the device where downloaded file is written.",
                regex: "(.*?)",
                example: Some("/path/to/file"),
                optional: false,
            },
        ],
    },
];

/// The version every published template carries, in the `<major>.<minor>` form
/// `ManagedTemplateVersion` (`^[1-9]+.[0-9]+$`) fixes.
const PUBLISHED_VERSION: &str = "1.0";

/// The `version` every published job document declares.
const DOCUMENT_VERSION: &str = "1.0";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name
/// and `region`: the scope is how the router picks the claiming service, and how the
/// service picks the region its ARNs are scoped to. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `managedJobTemplates` member as the list it is, or an empty list when it is
    /// absent.
    fn templates(&self) -> Vec<Value> {
        self.body
            .get("managedJobTemplates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The `(templateName, templateVersion)` of every entry, in the order the response
    /// carries them.
    fn pairs(&self) -> Vec<(String, String)> {
        self.templates()
            .iter()
            .map(|entry| (member(entry, "templateName"), member(entry, "templateVersion")))
            .collect()
    }

    fn next_token(&self) -> Option<String> {
        self.body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error response.
    fn message(&self) -> String {
        member(&self.body, "message")
    }
}

/// One string member of a JSON object, or the empty string when it is absent.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, path: &str, region: &str) -> Reply {
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::new())
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `GET /managed-job-templates` with the already-encoded query string `query`.
    async fn list(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/managed-job-templates".to_string()
        } else {
            format!("/managed-job-templates?{query}")
        };
        self.call(&path, REGION).await
    }

    async fn list_in(&self, region: &str) -> Reply {
        self.call("/managed-job-templates", region).await
    }

    /// `GET /managed-job-templates/{template_name}` with the already-encoded query
    /// string `query`.
    async fn describe(&self, template_name: &str, query: &str) -> Reply {
        let path = if query.is_empty() {
            format!("/managed-job-templates/{template_name}")
        } else {
            format!("/managed-job-templates/{template_name}?{query}")
        };
        self.call(&path, REGION).await
    }
}

/// The ARN a published version must carry in `region`: the account segment is EMPTY,
/// because the catalog is AWS-managed data and belongs to no account.
fn expected_arn(region: &str, template_name: &str, template_version: &str) -> String {
    format!("arn:aws:iot:{region}::jobtemplate/{template_name}:{template_version}")
}

/// Every published `(templateName, templateVersion)` pair, in listing order.
fn expected_pairs() -> Vec<(String, String)> {
    PUBLISHED
        .iter()
        .map(|published| {
            (
                published.template_name.to_string(),
                PUBLISHED_VERSION.to_string(),
            )
        })
        .collect()
}

/// The sentence every operation that looks a `templateName` up words a miss with.
fn not_found_message(template_name: &str) -> String {
    format!("ManagedJobTemplate with name: {template_name} not found")
}

/// The `${aws:iot:parameter:<key>}` form a managed template's document leaves a
/// parameter's value open with.
fn placeholder(key: &str) -> String {
    format!("${{aws:iot:parameter:{key}}}")
}

/// The `${aws:iot:parameter:<key>}` placeholders a document carries, found over the
/// whole document text so a placeholder nested anywhere in the step is seen.
fn placeholders(document: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = document;
    while let Some(start) = rest.find("${aws:iot:parameter:") {
        rest = &rest[start + "${aws:iot:parameter:".len()..];
        let Some(end) = rest.find('}') else { break };
        found.insert(rest[..end].to_string());
        rest = &rest[end..];
    }
    found
}

/// The `document` the catalog publishes for `published`: one step of the kind the row
/// runs, with a placeholder wherever one of the row's own parameters goes — passed to a
/// handler as positional arguments beside the directory it is found in, or to a command
/// as an input field of its own name — and the user as the step's `runAsUser`.
fn expected_document(published: &PublishedTemplate) -> Value {
    let mut input = Map::new();
    match published.handler {
        Some(handler) => {
            input.insert("handler".to_string(), json!(handler));
            if !published.parameters.is_empty() {
                let args: Vec<Value> = published
                    .parameters
                    .iter()
                    .map(|parameter| json!(placeholder(parameter.key)))
                    .collect();
                input.insert("args".to_string(), Value::Array(args));
            }
            input.insert(
                "path".to_string(),
                json!(placeholder(PATH_TO_HANDLER.key)),
            );
        }
        None => {
            for parameter in published.parameters {
                input.insert(parameter.key.to_string(), json!(placeholder(parameter.key)));
            }
        }
    }
    json!({
        "version": DOCUMENT_VERSION,
        "steps": [{
            "action": {
                "name": published.step_name,
                "type": published.step_type(),
                "input": Value::Object(input),
                "runAsUser": placeholder(RUN_AS_USER.key),
            }
        }]
    })
}

/// The `documentParameters` the catalog publishes for `published`, as the JSON a client
/// reads: every member of every parameter, in order.
fn expected_parameters(published: &PublishedTemplate) -> Vec<Value> {
    published
        .published_parameters()
        .into_iter()
        .map(|parameter| {
            let mut object = Map::new();
            object.insert("key".to_string(), json!(parameter.key));
            object.insert("description".to_string(), json!(parameter.description));
            object.insert("regex".to_string(), json!(parameter.regex));
            if let Some(example) = parameter.example {
                object.insert("example".to_string(), json!(example));
            }
            object.insert("optional".to_string(), json!(parameter.optional));
            Value::Object(object)
        })
        .collect()
}

#[tokio::test]
async fn listing_reports_the_whole_published_catalog_in_published_order() {
    let emulator = Emulator::new();
    let reply = emulator.list("").await;
    assert_eq!(
        reply.status, 200,
        "unfiltered listing: expected 200, got {reply:?}"
    );
    assert_eq!(
        reply.pairs(),
        expected_pairs(),
        "unfiltered listing: expected every published pair in published order, got {:?}",
        reply.pairs()
    );
    assert_eq!(
        reply.next_token(),
        None,
        "unfiltered listing with no maxResults: expected no nextToken, got {:?}",
        reply.next_token()
    );

    for (published, entry) in PUBLISHED.iter().zip(reply.templates()) {
        let name = published.template_name;
        assert_eq!(
            member(&entry, "templateArn"),
            expected_arn(REGION, name, PUBLISHED_VERSION),
            "case {name:?}: expected the version's ARN, got {:?}",
            member(&entry, "templateArn")
        );
        assert_eq!(
            member(&entry, "description"),
            published.description,
            "case {name:?}: expected the published description, got {:?}",
            member(&entry, "description")
        );
        assert_eq!(
            entry.get("environments"),
            Some(&json!(["LINUX"])),
            "case {name:?}: expected the environments the template is supported with, got {:?}",
            entry.get("environments")
        );
    }
}

#[tokio::test]
async fn listing_scopes_every_arn_to_the_signed_region() {
    // The region is the only thing that differs between the rows: the catalog itself is
    // the same published data in every region.
    let regions = ["us-east-1", "eu-west-2", "ap-southeast-2"];
    let emulator = Emulator::new();
    for region in regions {
        let reply = emulator.list_in(region).await;
        assert_eq!(
            reply.status, 200,
            "case {region:?}: expected 200, got {reply:?}"
        );
        assert_eq!(
            reply.pairs(),
            expected_pairs(),
            "case {region:?}: expected every published pair, got {:?}",
            reply.pairs()
        );
        for entry in reply.templates() {
            let name = member(&entry, "templateName");
            assert_eq!(
                member(&entry, "templateArn"),
                expected_arn(region, &name, PUBLISHED_VERSION),
                "case {region:?}/{name:?}: expected an ARN of the signed region, got {:?}",
                member(&entry, "templateArn")
            );
        }
    }
}

#[tokio::test]
async fn listing_narrows_to_one_templates_versions() {
    let emulator = Emulator::new();
    // Every published entry is a row: a `templateName` selects that template's versions
    // and nothing else.
    for published in PUBLISHED {
        let name = published.template_name;
        let reply = emulator.list(&format!("templateName={name}")).await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: expected 200, got {reply:?}"
        );
        assert_eq!(
            reply.pairs(),
            vec![(name.to_string(), PUBLISHED_VERSION.to_string())],
            "case {name:?}: expected only that template's versions, got {:?}",
            reply.pairs()
        );
    }
}

#[tokio::test]
async fn a_filter_naming_no_published_template_is_a_not_found() {
    // The rows differ along WHY the name names nothing: a name of the right shape, one
    // that differs from a published name only in case, one that is a prefix of a
    // published name, and the longest name the model admits (`length 1..64`).
    let cases = [
        "Managed-Nothing",
        "aws-reboot",
        "AWS-Reboot-",
        "AWS-Rebo",
        &"n".repeat(64),
    ];
    let emulator = Emulator::new();
    for name in cases {
        let reply = emulator.list(&format!("templateName={name}")).await;
        assert_eq!(
            reply.status, 404,
            "case {name:?}: expected 404, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {name:?}: expected {NOT_FOUND}, got {reply:?}"
        );
        assert_eq!(
            reply.message(),
            not_found_message(name),
            "case {name:?}: expected the catalog's not-found sentence, got {:?}",
            reply.message()
        );
    }
}

#[tokio::test]
async fn pages_of_every_size_walk_the_catalog_exactly_once() {
    let emulator = Emulator::new();
    let expected = expected_pairs();
    // Every page size from one to one past the number of entries: the page boundary has
    // to fall inside, exactly on, and past the end of the listing.
    for page_size in 1..=expected.len() + 1 {
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        // One request per entry at most, plus one: a listing that issued a token for
        // every entry would still terminate, and a loop that did not would be a defect
        // this bound catches.
        for _ in 0..=expected.len() {
            let query = match token.as_deref() {
                Some(token) => format!("maxResults={page_size}&nextToken={token}"),
                None => format!("maxResults={page_size}"),
            };
            let reply = emulator.list(&query).await;
            assert_eq!(
                reply.status, 200,
                "case page size {page_size}: expected 200, got {reply:?}"
            );
            let page = reply.pairs();
            assert!(
                page.len() <= page_size,
                "case page size {page_size}: expected at most {page_size} entries, got {}",
                page.len()
            );
            walked.extend(page);
            token = reply.next_token();
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case page size {page_size}: expected the walk to end without a token, got {token:?}"
        );
        assert_eq!(
            walked, expected,
            "case page size {page_size}: expected the whole catalog once in order, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_token_of_another_listing_or_of_no_listing_is_refused() {
    let emulator = Emulator::new();
    // A token issued by the UNFILTERED listing, which the filtered listing must not
    // accept.
    let unfiltered = emulator.list("maxResults=1").await;
    let unfiltered_token = unfiltered
        .next_token()
        .expect("a catalog of more than one entry issues a token at page size one");

    let filtered = emulator
        .list(&format!("templateName=AWS-Reboot&nextToken={unfiltered_token}"))
        .await;
    assert_eq!(
        filtered.status, 400,
        "case token of another listing: expected 400, got {filtered:?}"
    );
    assert_eq!(
        filtered.error_code.as_deref(),
        Some(QUALIFIED_INVALID_REQUEST),
        "case token of another listing: expected InvalidRequestException, got {filtered:?}"
    );

    // The rows differ along HOW the token is not one this listing issued: not hex, hex
    // of text with no scope in it, hex of a well-formed position whose version is
    // outside `ManagedTemplateVersion`, and hex of a well-formed position naming a
    // template this listing does not walk.
    let cases = [
        ("not hex", "zzzz".to_string()),
        ("hex of a bare word", hex::encode("AWS-Reboot")),
        (
            "hex of an unmodelled version",
            hex::encode("n\u{1f}AWS-Reboot\u{1f}0.1"),
        ),
        (
            "hex of a position outside the catalog",
            hex::encode("n\u{1f}Managed-Nothing\u{1f}1.0"),
        ),
    ];
    for (case, token) in cases {
        let reply = emulator.list(&format!("nextToken={token}")).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(QUALIFIED_INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {reply:?}"
        );
    }
}

#[tokio::test]
async fn describe_reports_the_published_document_and_parameters() {
    let emulator = Emulator::new();
    // Every published entry is a row: what the describe reports for it is the service's
    // own text, not a rendering of the request.
    for published in PUBLISHED {
        let name = published.template_name;
        let reply = emulator.describe(name, "").await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: expected 200, got {reply:?}"
        );
        assert_eq!(
            member(&reply.body, "description"),
            published.description,
            "case {name:?}: expected the published description, got {:?}",
            member(&reply.body, "description")
        );

        let document = member(&reply.body, "document");
        let parsed: Value = serde_json::from_str(&document)
            .unwrap_or_else(|error| panic!("case {name:?}: document is not JSON: {error}"));
        assert_eq!(
            parsed,
            expected_document(published),
            "case {name:?}: expected the published document {}, got {document:?}",
            expected_document(published)
        );

        let parameters = reply
            .body
            .get("documentParameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            parameters,
            expected_parameters(published),
            "case {name:?}: expected the published documentParameters {:?}, got {parameters:?}",
            expected_parameters(published)
        );
    }
}

#[tokio::test]
async fn every_listed_entry_is_describable_with_its_document() {
    let emulator = Emulator::new();
    let listing = emulator.list("").await;
    assert_eq!(listing.status, 200, "listing: expected 200, got {listing:?}");

    for (published, entry) in PUBLISHED.iter().zip(listing.templates()) {
        let name = published.template_name;
        let reply = emulator.describe(name, "").await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: expected 200, got {reply:?}"
        );

        // The five members the listing carries are reported unchanged by the describe:
        // one catalog entry, read two ways.
        for shared in [
            "templateName",
            "templateArn",
            "description",
            "templateVersion",
        ] {
            assert_eq!(
                member(&reply.body, shared),
                member(&entry, shared),
                "case {name:?}/{shared}: expected the listing's value {:?}, got {:?}",
                member(&entry, shared),
                member(&reply.body, shared)
            );
        }
        assert_eq!(
            reply.body.get("environments"),
            entry.get("environments"),
            "case {name:?}: expected the listing's environments {:?}, got {:?}",
            entry.get("environments"),
            reply.body.get("environments")
        );

        // The document and the parameters are two halves of one entry: every pattern the
        // document leaves open is a declared parameter, and every REQUIRED parameter has
        // a pattern in the document.
        let declared: Vec<(String, bool)> = reply
            .body
            .get("documentParameters")
            .and_then(Value::as_array)
            .map(|parameters| {
                parameters
                    .iter()
                    .map(|parameter| {
                        (
                            member(parameter, "key"),
                            parameter
                                .get("optional")
                                .and_then(Value::as_bool)
                                .unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let document = member(&reply.body, "document");
        assert!(
            !document.is_empty(),
            "case {name:?}: expected the version's document, got {reply:?}"
        );
        let found = placeholders(&document);
        let declared_keys: BTreeSet<String> =
            declared.iter().map(|(key, _)| key.clone()).collect();
        assert!(
            found.is_subset(&declared_keys),
            "case {name:?}: expected every placeholder to be a declared parameter, got \
             {found:?} against {declared_keys:?}"
        );
        for (key, optional) in &declared {
            assert!(
                *optional || found.contains(key),
                "case {name:?}: expected required parameter {key:?} to be a placeholder of \
                 the document, got {found:?}"
            );
        }
    }
}

#[tokio::test]
async fn describe_selects_the_version_and_defaults_to_the_published_one() {
    let emulator = Emulator::new();
    // The rows differ along which version the request asks for: none (the default
    // version), the published version by name, and versions the template does not
    // publish — a higher minor, a two-digit minor (which sorts after `1.9` by number and
    // before it by text) and a higher major, each a well-formed
    // `ManagedTemplateVersion`.
    let cases: [(&str, Option<&str>, Option<&str>); 5] = [
        ("default version", None, Some(PUBLISHED_VERSION)),
        ("published version", Some("1.0"), Some(PUBLISHED_VERSION)),
        ("higher minor", Some("1.1"), None),
        ("two-digit minor", Some("1.10"), None),
        ("higher major", Some("2.0"), None),
    ];
    for published in PUBLISHED {
        let name = published.template_name;
        for (case, requested, expected) in &cases {
            let query = requested.map_or_else(String::new, |version| {
                format!("templateVersion={version}")
            });
            let reply = emulator.describe(name, &query).await;
            match expected {
                Some(version) => {
                    assert_eq!(
                        reply.status, 200,
                        "case {name:?}/{case}: expected 200, got {reply:?}"
                    );
                    assert_eq!(
                        member(&reply.body, "templateVersion"),
                        *version,
                        "case {name:?}/{case}: expected version {version:?}, got {:?}",
                        member(&reply.body, "templateVersion")
                    );
                    assert_eq!(
                        member(&reply.body, "templateArn"),
                        expected_arn(REGION, name, version),
                        "case {name:?}/{case}: expected the version's ARN, got {:?}",
                        member(&reply.body, "templateArn")
                    );
                }
                None => {
                    let requested = requested.unwrap_or_default();
                    assert_eq!(
                        reply.status, 400,
                        "case {name:?}/{case}: expected 400, got {reply:?}"
                    );
                    assert_eq!(
                        reply.error_code.as_deref(),
                        Some(QUALIFIED_INVALID_REQUEST),
                        "case {name:?}/{case}: expected InvalidRequestException, got {reply:?}"
                    );
                    assert_eq!(
                        reply.message(),
                        format!("Invalid ManagedJobTemplate version: {requested}"),
                        "case {name:?}/{case}: expected the sentence naming the version, got \
                         {:?}",
                        reply.message()
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn describing_a_template_the_catalog_does_not_publish_is_a_not_found() {
    // The rows differ along why the name names nothing, as the listing's filter rows do.
    let cases = ["Managed-Nothing", "aws-reboot", "AWS-Rebo", &"n".repeat(64)];
    let emulator = Emulator::new();
    for name in cases {
        let reply = emulator.describe(name, "").await;
        assert_eq!(
            reply.status, 404,
            "case {name:?}: expected 404, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {name:?}: expected {NOT_FOUND}, got {reply:?}"
        );
        assert_eq!(
            reply.message(),
            not_found_message(name),
            "case {name:?}: expected the catalog's not-found sentence, got {:?}",
            reply.message()
        );
    }
}

#[tokio::test]
async fn reading_the_catalog_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let before = emulator.list("").await;
    // Every read the surface has, including the ones that are refused: none of them may
    // create, change or consume an entry.
    for published in PUBLISHED {
        let _ = emulator.describe(published.template_name, "").await;
        let _ = emulator
            .describe(published.template_name, "templateVersion=9.9")
            .await;
        let _ = emulator
            .list(&format!("templateName={}", published.template_name))
            .await;
    }
    let _ = emulator.describe("Managed-Nothing", "").await;
    let _ = emulator.list("templateName=Managed-Nothing").await;
    let _ = emulator.list("maxResults=1").await;

    let after = emulator.list("").await;
    assert_eq!(
        after.body, before.body,
        "after every read: expected the same catalog as before, got {:?}",
        after.body
    );
}
