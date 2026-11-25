// SPDX-License-Identifier: CC0-1.0

//! The tagging surface over an EMAIL TEMPLATE's ARN — `ListTagsForResource`,
//! `TagResource` and `UntagResource` — end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization are
//! all exercised.
//!
//! The rule these cases pin: the tagging surface resolves a `ResourceArn` against the
//! table this service keeps the resource KIND the ARN's resource field names in, for
//! every kind it records tags for — so an ARN whose resource field is `template/<name>`
//! resolves to the account's email template of that name, reports exactly the tags that
//! template carries, and takes a tag write in place; a `template/<name>` ARN is refused
//! with the modelled terminal 404 only while the account holds no template of that name.
//!
//! The accepted table below spans what the rule generalises over: how many tags the
//! template was created with (none, one, several, the documented maximum), the order
//! they were supplied in (which is not their sorted order), the characters a key and a
//! value carry, an empty tag value, and the form of the template name. The write rows
//! span adding a key the template does not carry, overwriting one it does, removing one
//! of several and removing the last one so the list empties. The refusal rows span the
//! resolution legs of a `template/<name>` ARN — a name never created, a name whose
//! template was deleted, another account, another Region — over each of the three
//! operations that resolve a `ResourceArn`.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// `ListTagsForResource` and `UntagResource` bind their inputs to `@httpQuery`, and an
// ARN carries `:` and `/` — characters a query string gives its own meaning to — so the
// values are escaped the way an SDK escapes them, by the one escape this test binary
// shares.
use crate::contact_lists::query_escaped;

/// The Region every call in this suite is signed for, so the Region an ARN it resolves
/// has to carry.
const HOME: &str = "us-east-1";

/// A Region these calls are not signed for.
const AWAY: &str = "eu-west-1";

/// An account id that is not this emulator's.
const OTHER_ACCOUNT_ID: &str = "999999999999";

/// Tags one resource may carry, taken from the crate's own constant so the bound is
/// stated once: the model's `Tag` documentation states "A resource can have as many as
/// 50 tags" (`smithy_query(query="shape",shape="Tag")`).
const MAX_TAGS_PER_RESOURCE: usize =
    service_com_amazonaws_sesv2_simpleemailservice_v2::state::MAX_TAGS_PER_RESOURCE;

/// The account the emulator serves, which every ARN these cases resolve carries.
fn account_id() -> &'static str {
    emulator_server::emulator_account_id()
}

/// `arn:aws:ses:<region>:<account>:template/<name>`, the form SES writes an email
/// template in the resource field of its ARN.
fn template_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:ses:{region}:{account}:template/{name}")
}

/// The sentence the tagging surface answers a `ResourceArn` naming no email template the
/// account holds: the resource KIND and the NAME the ARN addresses it by, never the whole
/// ARN. Spelled out here rather than read from the crate, so a change to the handler's
/// wording fails these rows.
fn not_present(name: &str) -> String {
    format!("No Template present with name: {name}")
}

/// A tag pair.
fn tag(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `Tags` the response reported, as key/value pairs in the order reported.
    fn tags(&self) -> Vec<(String, String)> {
        self.body
            .get("Tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .map(|tag| {
                        (
                            tag.get("Key")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            tag.get("Value")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the response carries a `Tags` member at all.
    fn has_tags(&self) -> bool {
        self.body.get("Tags").is_some()
    }

    /// The `TemplateContent` the response reported, or `Value::Null` when absent.
    fn content(&self) -> Value {
        self.body
            .get("TemplateContent")
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// The emulated service behind its router.
struct Ses {
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{HOME}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The SigV4 credential scope is what routes the request to the `ses` signing
            // name, and what names the Region the call executes in; the signature itself
            // is not checked here.
            .header(
                "authorization",
                format!("AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{HOME}/ses/aws4_request, SignedHeaders=host, Signature=00"),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
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
            error_type,
            body,
        }
    }

    /// `CreateEmailTemplate` with the content and the inline tags the row carries.
    async fn create_template(
        &self,
        name: &str,
        content: Value,
        tags: &[(String, String)],
    ) -> Reply {
        let tags: Vec<Value> = tags
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect();
        self.call(
            "POST",
            "/v2/email/templates",
            Some(json!({
                "TemplateName": name,
                "TemplateContent": content,
                "Tags": tags,
            })),
        )
        .await
    }

    /// `GetEmailTemplate`, the read that reports the same template's content and tags.
    async fn get_template(&self, name: &str) -> Reply {
        self.call("GET", &format!("/v2/email/templates/{name}"), None)
            .await
    }

    /// `DeleteEmailTemplate`.
    async fn delete_template(&self, name: &str) -> Reply {
        self.call("DELETE", &format!("/v2/email/templates/{name}"), None)
            .await
    }

    /// `ListTagsForResource` for `resource_arn`, which the operation binds to
    /// `@httpQuery`.
    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v2/email/tags?ResourceArn={}", query_escaped(resource_arn)),
            None,
        )
        .await
    }

    /// `TagResource` for `resource_arn` with `tags`, which the operation carries in its
    /// POST body.
    async fn tag_resource(&self, resource_arn: &str, tags: &[(String, String)]) -> Reply {
        let tags: Vec<Value> = tags
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect();
        self.call(
            "POST",
            "/v2/email/tags",
            Some(json!({"ResourceArn": resource_arn, "Tags": tags})),
        )
        .await
    }

    /// `UntagResource` for `resource_arn` naming `keys`, both of which the operation
    /// binds to `@httpQuery`: the keys are repeated across the query string, as the
    /// model's own example spells them.
    async fn untag_resource(&self, resource_arn: &str, keys: &[String]) -> Reply {
        let mut query = format!("ResourceArn={}", query_escaped(resource_arn));
        for key in keys {
            query.push_str(&format!("&TagKeys={}", query_escaped(key)));
        }
        self.call("DELETE", &format!("/v2/email/tags?{query}"), None)
            .await
    }
}

/// The content every row that is not about the content itself creates its template with.
fn content() -> Value {
    json!({"Subject": "Your receipt", "Text": "Thanks for your order."})
}

/// `count` distinct tags, for the row that fills a template to the documented maximum.
fn generated_tags(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|index| tag(&format!("key{index}"), &format!("value{index}")))
        .collect()
}

/// One row of the inline-tag table: the name the template is created under and the tags
/// the create declares inline, which a tag read of its ARN has to report in that order.
struct InlineCase {
    case: &'static str,
    name: &'static str,
    tags: Vec<(String, String)>,
}

fn inline_cases() -> Vec<InlineCase> {
    vec![
        InlineCase {
            case: "a create declaring no tag at all",
            name: "untagged-template",
            tags: Vec::new(),
        },
        InlineCase {
            case: "a create declaring one tag",
            name: "audit_template",
            tags: vec![tag("audit", "original")],
        },
        InlineCase {
            case: "a create declaring several tags, not in sorted order",
            name: "template.with.dots",
            tags: vec![tag("env", "dev"), tag("team", "core"), tag("app", "mailer")],
        },
        InlineCase {
            case: "a create declaring a tag whose value is empty",
            name: "empty-valued",
            tags: vec![tag("owner", "")],
        },
        InlineCase {
            case: "a create declaring tags carrying spaces, digits and the allowed symbols",
            name: "SymbolTemplate9",
            tags: vec![
                tag("cost centre", "team 42"),
                tag("path/name", "a_b-c.d:e+f=g@h"),
            ],
        },
        InlineCase {
            case: "a create declaring the documented maximum of tags",
            name: "fully-tagged",
            tags: generated_tags(MAX_TAGS_PER_RESOURCE),
        },
    ]
}

#[tokio::test]
async fn list_tags_for_resource_reports_the_tags_a_template_was_created_with() {
    for case in inline_cases() {
        let ses = Ses::new();
        let created = ses.create_template(case.name, content(), &case.tags).await;
        assert_eq!(
            created.status,
            200,
            "case {:?}: CreateEmailTemplate answered {} {:?}, expected 200",
            case.case,
            created.status,
            created.message()
        );

        let arn = template_arn(HOME, account_id(), case.name);
        let reply = ses.list_tags(&arn).await;
        assert_eq!(
            reply.status,
            200,
            "case {:?}: ListTagsForResource for {arn:?} answered {} {:?}, expected 200",
            case.case,
            reply.status,
            reply.message()
        );
        assert!(
            reply.has_tags(),
            "case {:?}: the response carried no Tags member: {:?}",
            case.case,
            reply.body
        );
        let reported = reply.tags();
        assert_eq!(
            reported, case.tags,
            "case {:?}: ListTagsForResource reported {reported:?}, expected {:?}",
            case.case, case.tags
        );

        // The template's own read reports the same set, so the two readers of one
        // template's tags cannot disagree.
        let read = ses.get_template(case.name).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: GetEmailTemplate answered {} {:?}, expected 200",
            case.case,
            read.status,
            read.message()
        );
        let read_tags = read.tags();
        assert_eq!(
            read_tags, case.tags,
            "case {:?}: GetEmailTemplate reported the tags {read_tags:?}, expected {:?}",
            case.case, case.tags
        );
    }
}

#[tokio::test]
async fn reading_a_templates_tags_leaves_every_later_caller_seeing_what_it_saw() {
    let ses = Ses::new();
    let name = "repeat-read";
    let tags = vec![tag("env", "dev"), tag("team", "core")];
    ses.create_template(name, content(), &tags).await;
    let arn = template_arn(HOME, account_id(), name);

    for attempt in 0..3 {
        let reported = ses.list_tags(&arn).await.tags();
        assert_eq!(
            reported, tags,
            "case {attempt:?}: a repeated read reported {reported:?}, expected {tags:?}"
        );
    }
    let content_after = ses.get_template(name).await.content();
    assert_eq!(
        content_after,
        content(),
        "after the reads the template's content was {content_after:?}, expected {:?}",
        content()
    );
}

/// One row of the write table: the tags the template starts out with, the tags
/// `TagResource` adds, the keys `UntagResource` then removes, and what the template
/// carries after each of those two writes.
struct WriteCase {
    case: &'static str,
    name: &'static str,
    initial: Vec<(String, String)>,
    added: Vec<(String, String)>,
    after_add: Vec<(String, String)>,
    removed: Vec<String>,
    after_remove: Vec<(String, String)>,
}

fn write_cases() -> Vec<WriteCase> {
    vec![
        WriteCase {
            case: "overwriting the one key the template carries, then removing it",
            name: "overwrite-one",
            initial: vec![tag("audit", "original")],
            added: vec![tag("audit", "updated")],
            after_add: vec![tag("audit", "updated")],
            removed: vec!["audit".to_string()],
            after_remove: Vec::new(),
        },
        WriteCase {
            case: "adding a key an untagged template does not carry, then removing it",
            name: "add-to-untagged",
            initial: Vec::new(),
            added: vec![tag("env", "prod")],
            after_add: vec![tag("env", "prod")],
            removed: vec!["env".to_string()],
            after_remove: Vec::new(),
        },
        WriteCase {
            case: "overwriting one key and appending another, then removing one of them",
            name: "overwrite-and-append",
            initial: vec![tag("env", "dev"), tag("team", "core")],
            added: vec![tag("env", "prod"), tag("app", "mailer")],
            after_add: vec![tag("env", "prod"), tag("team", "core"), tag("app", "mailer")],
            removed: vec!["team".to_string()],
            after_remove: vec![tag("env", "prod"), tag("app", "mailer")],
        },
        WriteCase {
            case: "removing every key the template carries, so the list empties",
            name: "remove-every-key",
            initial: vec![tag("env", "dev"), tag("team", "core")],
            added: vec![tag("app", "mailer")],
            after_add: vec![tag("env", "dev"), tag("team", "core"), tag("app", "mailer")],
            removed: vec![
                "env".to_string(),
                "team".to_string(),
                "app".to_string(),
            ],
            after_remove: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn tag_and_untag_resource_change_a_templates_tags_in_place() {
    for case in write_cases() {
        let ses = Ses::new();
        ses.create_template(case.name, content(), &case.initial).await;
        let arn = template_arn(HOME, account_id(), case.name);

        let tagged = ses.tag_resource(&arn, &case.added).await;
        assert_eq!(
            tagged.status,
            200,
            "case {:?}: TagResource answered {} {:?}, expected 200",
            case.case,
            tagged.status,
            tagged.message()
        );
        let after_add = ses.list_tags(&arn).await.tags();
        assert_eq!(
            after_add, case.after_add,
            "case {:?}: after TagResource the template reported {after_add:?}, expected {:?}",
            case.case, case.after_add
        );

        let untagged = ses.untag_resource(&arn, &case.removed).await;
        assert_eq!(
            untagged.status,
            200,
            "case {:?}: UntagResource answered {} {:?}, expected 200",
            case.case,
            untagged.status,
            untagged.message()
        );
        let after_remove = ses.list_tags(&arn).await;
        assert_eq!(
            after_remove.status,
            200,
            "case {:?}: ListTagsForResource answered {} {:?}, expected 200",
            case.case,
            after_remove.status,
            after_remove.message()
        );
        assert!(
            after_remove.has_tags(),
            "case {:?}: a template carrying no tag reported no Tags member: {:?}",
            case.case,
            after_remove.body
        );
        let reported = after_remove.tags();
        assert_eq!(
            reported, case.after_remove,
            "case {:?}: after UntagResource the template reported {reported:?}, expected {:?}",
            case.case, case.after_remove
        );

        // Neither write touched the template's content, which is what the template
        // operations own.
        let read = ses.get_template(case.name).await;
        assert_eq!(
            read.content(),
            content(),
            "case {:?}: the content after the tag writes was {:?}, expected {:?}",
            case.case,
            read.content(),
            content()
        );
        let read_tags = read.tags();
        assert_eq!(
            read_tags, case.after_remove,
            "case {:?}: GetEmailTemplate reported the tags {read_tags:?}, expected {:?}",
            case.case, case.after_remove
        );
    }
}

#[tokio::test]
async fn tagging_a_template_reaches_only_the_template_its_arn_names() {
    let ses = Ses::new();
    ses.create_template("first", content(), &[tag("env", "dev")])
        .await;
    ses.create_template("second", content(), &[tag("env", "test")])
        .await;

    let first = template_arn(HOME, account_id(), "first");
    let second = template_arn(HOME, account_id(), "second");
    ses.tag_resource(&first, &[tag("env", "prod")]).await;
    ses.untag_resource(&second, &["env".to_string()]).await;

    let first_tags = ses.list_tags(&first).await.tags();
    assert_eq!(
        first_tags,
        vec![tag("env", "prod")],
        "the tagged template reported {first_tags:?}, expected the overwritten value"
    );
    let second_tags = ses.list_tags(&second).await.tags();
    assert_eq!(
        second_tags,
        Vec::new(),
        "the untagged template reported {second_tags:?}, expected no tag"
    );
}

/// One refusal row: the `template/<name>` ARN that resolves to no template the call can
/// reach, and the name the refusal has to report.
struct RefusalCase {
    case: &'static str,
    resource_arn: String,
    named: String,
}

#[tokio::test]
async fn every_tag_operation_refuses_a_template_arn_resolving_to_no_template() {
    let ses = Ses::new();
    // One template the account does hold, which every refusal below has to leave alone,
    // and one it created and then deleted.
    let held_tags = vec![tag("env", "dev")];
    ses.create_template("held", content(), &held_tags).await;
    ses.create_template("deleted", content(), &[tag("env", "dev")])
        .await;
    let removed = ses.delete_template("deleted").await;
    assert_eq!(
        removed.status,
        200,
        "DeleteEmailTemplate answered {} {:?}, expected 200",
        removed.status,
        removed.message()
    );

    let cases = vec![
        RefusalCase {
            case: "a template name the account never created",
            resource_arn: template_arn(HOME, account_id(), "never-created"),
            named: "never-created".to_string(),
        },
        RefusalCase {
            case: "a template the account created and then deleted",
            resource_arn: template_arn(HOME, account_id(), "deleted"),
            named: "deleted".to_string(),
        },
        RefusalCase {
            case: "the held template's name under another account",
            resource_arn: template_arn(HOME, OTHER_ACCOUNT_ID, "held"),
            named: "held".to_string(),
        },
        RefusalCase {
            case: "the held template's name in another Region",
            resource_arn: template_arn(AWAY, account_id(), "held"),
            named: "held".to_string(),
        },
    ];

    for case in &cases {
        let expected = not_present(&case.named);
        let read = ses.list_tags(&case.resource_arn).await;
        assert_refused(&read, case.case, "ListTagsForResource", &expected);
        assert!(
            !read.has_tags(),
            "case {:?}: a refused ListTagsForResource reported tags: {:?}",
            case.case,
            read.body
        );

        let write = ses
            .tag_resource(&case.resource_arn, &[tag("env", "prod")])
            .await;
        assert_refused(&write, case.case, "TagResource", &expected);

        let removal = ses
            .untag_resource(&case.resource_arn, &["env".to_string()])
            .await;
        assert_refused(&removal, case.case, "UntagResource", &expected);
    }

    // Every refusal left the held template exactly as it was, so a refused call writes
    // nothing.
    let still_held = ses
        .list_tags(&template_arn(HOME, account_id(), "held"))
        .await
        .tags();
    assert_eq!(
        still_held, held_tags,
        "after the refusals the held template reported {still_held:?}, expected {held_tags:?}"
    );
}

/// Assert `reply` is the modelled terminal 404 the tagging surface answers a
/// `ResourceArn` naming no resource the account holds, carrying `expected`.
fn assert_refused(reply: &Reply, case: &str, operation: &str, expected: &str) {
    assert_eq!(
        reply.status,
        404,
        "case {case:?}: {operation} answered {} {:?}, expected 404",
        reply.status,
        reply.message()
    );
    assert_eq!(
        reply.error_code(),
        "NotFoundException",
        "case {case:?}: {operation} named the error {:?}, expected NotFoundException",
        reply.error_code()
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: {operation} carried the message {:?}, expected {expected:?}",
        reply.message()
    );
}
