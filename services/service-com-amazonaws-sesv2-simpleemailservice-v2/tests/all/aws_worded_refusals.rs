// SPDX-License-Identifier: CC0-1.0

//! The sentences this service answers a refusal with, and WHERE each refusal is decided,
//! end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — through the shared harness in the sibling [`crate::configuration_sets`]
//! suite, so routing, the constraint walk, the handler and the error envelope are all
//! exercised.
//!
//! The rules these cases pin, each stated over a SET of inputs rather than one:
//!
//! * a resource NAME outside the one character rule SES documents for its own names is
//!   refused `400 BadRequestException` with the rejected name between angle brackets and
//!   the characters a name may carry — one sentence for every resource kind and every
//!   operation that takes such a name, read path included, and the refusal persists
//!   nothing;
//! * rendering DATA that is not the object the model says it is refused `400
//!   BadRequestException` with one fixed sentence, whichever operation read it, and that
//!   sentence is DISTINCT from the one for data that is an object but states no value for
//!   an attribute the template names;
//! * a value outside a modelled constraint carried by the ELEMENTS of a list is reported
//!   against the LIST member, with the element's requirement nested inside the list's own
//!   clause and no position inside the list named — while a member reached THROUGH an
//!   element is still reported at its own position, which the sibling [`crate::metric_data`]
//!   suite pins;
//! * one operation words that refusal itself: `PutConfigurationSetSuppressionOptions`
//!   answers a sentence about what a suppression reason MEANS, ahead of the constraint
//!   walk, while every other operation carrying the same member answers the walk's report;
//! * a `SendingPoolName` naming no dedicated IP pool is a bad request PARAMETER — `400
//!   BadRequestException` — on every operation that writes the member, while a call that
//!   addresses the pool ITSELF is still the modelled 404;
//! * `SendEmail` settles what the REQUEST carries first, then the sending identity, and
//!   only then the template the content names, so an unverified sender cannot learn from
//!   the refusal which template names the account holds.
//!
//! The rows span what each rule generalises over: the resource kind and the operation that
//! carries the name, the character outside the rule and the length past the bound, the form
//! the malformed data takes, where the list sits in the request and which element of it is
//! outside the value set, the pool name, and whether the named template exists.

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    MAX_CONFIGURATION_SET_NAME_LEN, MAX_TENANT_NAME_CHARS,
};

use crate::configuration_sets::{Reply, Ses};

/// The routes the cases below drive, as the model binds them
/// (`smithy_query(query="operation",operation="...")`).
const CREATE_TENANT: &str = "/v2/email/tenants";
const GET_TENANT: &str = "/v2/email/tenants/get";
const UPDATE_CONFIGURATION_SETS: &str = "/v2/email/update-configuration-sets";
const ACCOUNT_SUPPRESSION: &str = "/v2/email/account/suppression";
const ACCOUNT: &str = "/v2/email/account";
const TEMPLATES: &str = "/v2/email/templates";
const OUTBOUND_EMAILS: &str = "/v2/email/outbound-emails";
const DEDICATED_IP_POOLS: &str = "/v2/email/dedicated-ip-pools";

/// The value set of `SuppressionListReason`, from the model's own enum
/// (`smithy_query(query="shape",shape="SuppressionListReason")`: `BOUNCE`, `COMPLAINT`).
const SUPPRESSION_REASONS: [&str; 2] = ["BOUNCE", "COMPLAINT"];

/// The clause separator the framework renders, shared by every requirement.
const SEPARATOR: &str = " failed to satisfy constraint: ";

/// The sentence AWS answers a resource name it does not accept with: the rejected name
/// between angle brackets, then the characters a name may carry.
fn invalid_name(kind: &str, name: &str) -> String {
    format!("Invalid {kind} name <{name}>: only alphanumeric ASCII characters, '_', and '-' are allowed.")
}

/// The calls these cases drive beside the shared harness's own.
trait RefusalCalls {
    async fn create_tenant(&self, tenant_name: &str) -> Reply;
    async fn get_tenant(&self, tenant_name: &str) -> Reply;
    async fn update_set(&self, body: Value) -> Reply;
    async fn put_account_suppression(&self, body: Value) -> Reply;
    async fn account(&self) -> Reply;
    async fn create_template(&self, name: &str) -> Reply;
    async fn render(&self, name: &str, data: &str) -> Reply;
    async fn send(&self, body: Value) -> Reply;
    async fn put_suppression_options(&self, set: &str, body: Value) -> Reply;
    async fn put_delivery_options(&self, set: &str, body: Value) -> Reply;
    async fn create_pool(&self, pool_name: &str) -> Reply;
    async fn get_pool(&self, pool_name: &str) -> Reply;
}

impl RefusalCalls for Ses {
    async fn create_tenant(&self, tenant_name: &str) -> Reply {
        self.call("POST", CREATE_TENANT, Some(json!({"TenantName": tenant_name})))
            .await
    }

    async fn get_tenant(&self, tenant_name: &str) -> Reply {
        self.call("POST", GET_TENANT, Some(json!({"TenantName": tenant_name})))
            .await
    }

    async fn update_set(&self, body: Value) -> Reply {
        self.call("POST", UPDATE_CONFIGURATION_SETS, Some(body)).await
    }

    async fn put_account_suppression(&self, body: Value) -> Reply {
        self.call("PUT", ACCOUNT_SUPPRESSION, Some(body)).await
    }

    async fn account(&self) -> Reply {
        self.call("GET", ACCOUNT, None).await
    }

    async fn create_template(&self, name: &str) -> Reply {
        self.call(
            "POST",
            TEMPLATES,
            Some(json!({
                "TemplateName": name,
                "TemplateContent": {"Subject": "Hi {{name}}", "Text": "Hello {{name}}"},
            })),
        )
        .await
    }

    async fn render(&self, name: &str, data: &str) -> Reply {
        self.call(
            "POST",
            &format!("{TEMPLATES}/{name}/render"),
            Some(json!({"TemplateData": data})),
        )
        .await
    }

    async fn send(&self, body: Value) -> Reply {
        self.call("POST", OUTBOUND_EMAILS, Some(body)).await
    }

    async fn put_suppression_options(&self, set: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/configuration-sets/{set}/suppression-options"),
            Some(body),
        )
        .await
    }

    async fn put_delivery_options(&self, set: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/configuration-sets/{set}/delivery-options"),
            Some(body),
        )
        .await
    }

    async fn create_pool(&self, pool_name: &str) -> Reply {
        self.call(
            "POST",
            DEDICATED_IP_POOLS,
            Some(json!({"PoolName": pool_name})),
        )
        .await
    }

    async fn get_pool(&self, pool_name: &str) -> Reply {
        self.call("GET", &format!("{DEDICATED_IP_POOLS}/{pool_name}"), None)
            .await
    }
}

/// Assert `reply` is the refusal `code` at `status` carrying exactly `message`.
fn refused(case: &str, reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (status, code),
        "case {case:?}: expected {status} {code}, got {} {} ({})",
        reply.status,
        reply.error_code(),
        reply.message()
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected {message:?}, got {:?}",
        reply.message()
    );
}

/// The names outside the one character rule: a space, each punctuation character a caller
/// is likely to try, a non-ASCII letter, and one character past the documented length bound
/// (the bound itself is `MAX_…`, from the member's own documentation — "the name can
/// contain up to 64 alphanumeric characters, including letters, numbers, hyphens (-) and
/// underscores (_) only").
fn names_outside_the_rule(bound: usize) -> Vec<String> {
    vec![
        "bad name!".to_string(),
        "has.dot".to_string(),
        "plus+sign".to_string(),
        "slash-is-fine/but-not-this".to_string(),
        "star*".to_string(),
        "unicode-\u{e9}".to_string(),
        "n".repeat(bound + 1),
    ]
}

#[tokio::test]
async fn a_name_outside_the_character_rule_is_refused_in_awss_own_sentence() {
    let ses = Ses::new();

    // Each row is one (operation, resource kind) pair the rule covers, over every name
    // outside the rule: the two operations that take a configuration-set name in the body
    // (so the name travels exactly as submitted), and the tenant WRITE and READ paths,
    // which answer the identical sentence because the read validates the name before it
    // looks a tenant up.
    for name in names_outside_the_rule(MAX_CONFIGURATION_SET_NAME_LEN) {
        let created = ses.create_set(json!({"ConfigurationSetName": name})).await;
        refused(
            &format!("CreateConfigurationSet {name:?}"),
            &created,
            400,
            "BadRequestException",
            &invalid_name("configuration set", &name),
        );
        let updated = ses
            .update_set(json!({"ConfigurationSetName": name, "TrackingOptions": null}))
            .await;
        refused(
            &format!("UpdateConfigurationSet {name:?}"),
            &updated,
            400,
            "BadRequestException",
            &invalid_name("configuration set", &name),
        );
    }

    for name in names_outside_the_rule(MAX_TENANT_NAME_CHARS) {
        let created = ses.create_tenant(&name).await;
        refused(
            &format!("CreateTenant {name:?}"),
            &created,
            400,
            "BadRequestException",
            &invalid_name("tenant", &name),
        );
        let read = ses.get_tenant(&name).await;
        refused(
            &format!("GetTenant {name:?}"),
            &read,
            400,
            "BadRequestException",
            &invalid_name("tenant", &name),
        );
    }

    // The other side of the rule: a name the rule admits is not refused for its form. The
    // rows span the characters the rule allows and the bound it stops at.
    for name in [
        "s".to_string(),
        "Set-1_2".to_string(),
        "s".repeat(MAX_CONFIGURATION_SET_NAME_LEN),
    ] {
        let created = ses.create_set(json!({"ConfigurationSetName": name})).await;
        assert_eq!(
            created.status,
            200,
            "case {name:?}: expected a name inside the rule to be created, got {} ({})",
            created.status,
            created.message()
        );
    }
}

#[tokio::test]
async fn a_refused_create_persists_nothing_so_the_name_stays_reusable() {
    let ses = Ses::new();
    let set = "reusable_set_1";

    // A create whose delivery options name a pool the account does not hold is refused, and
    // the name it asked for is left untaken: the next create under the same name succeeds
    // and the read reports the set that create configured.
    let refusal = ses
        .create_set(json!({
            "ConfigurationSetName": set,
            "DeliveryOptions": {"SendingPoolName": "no-such-pool"},
        }))
        .await;
    refused(
        "a create naming a pool the account does not hold",
        &refusal,
        400,
        "BadRequestException",
        "SendingPool <no-such-pool> doesn't exist",
    );
    let read = ses.get_set(set).await;
    assert_eq!(
        (read.status, read.error_code().as_str()),
        (404, "NotFoundException"),
        "a refused create must store nothing, got {} ({})",
        read.status,
        read.message()
    );

    let created = ses
        .create_set(json!({"ConfigurationSetName": set, "TlsPolicy": "REQUIRE"}))
        .await;
    assert_eq!(
        created.status,
        200,
        "the name a refused create asked for must stay reusable, got {} ({})",
        created.status,
        created.message()
    );
    let read = ses.get_set(set).await;
    assert_eq!(
        read.status, 200,
        "the second create must be readable, got {} ({})",
        read.status,
        read.message()
    );
}

/// The forms rendering data takes when it is not the object the model says it is.
const MALFORMED_DATA: [(&str, &str); 7] = [
    ("text that is no JSON at all", "name=Ada"),
    ("a JSON array", r#"["name","Ada"]"#),
    ("a JSON string", r#""Ada""#),
    ("a JSON number", "42"),
    ("a JSON null", "null"),
    ("a JSON boolean", "true"),
    ("an object that never closes", r#"{"name":"Ada""#),
];

/// AWS's sentence for rendering data that is not a set of key-value pairs: fixed text with
/// no variable part and no trailing full stop.
const INVALID_RENDERING_DATA: &str = "Template rendering data is invalid";

#[tokio::test]
async fn malformed_rendering_data_is_refused_in_one_sentence_whoever_read_it() {
    let ses = Ses::new();
    let template = "greeting";
    assert_eq!(
        ses.create_template(template).await.status,
        200,
        "the template must be saved first"
    );

    for (case, data) in MALFORMED_DATA {
        // The operation that previews a render.
        let rendered = ses.render(template, data).await;
        refused(
            &format!("TestRenderEmailTemplate {case}"),
            &rendered,
            400,
            "BadRequestException",
            INVALID_RENDERING_DATA,
        );

        // And the send that renders the same template from the same data. The sender is one
        // this account has NOT verified, which is the ordering half of the rule: what the
        // request carries is settled before the identity gate, so the data is refused for
        // being malformed whatever the sender.
        let sent = ses
            .send(json!({
                "FromEmailAddress": "stranger@example.com",
                "Destination": {"ToAddresses": ["reader@example.net"]},
                "Content": {"Template": {"TemplateName": template, "TemplateData": data}},
            }))
            .await;
        refused(
            &format!("SendEmail {case}"),
            &sent,
            400,
            "BadRequestException",
            INVALID_RENDERING_DATA,
        );
    }

    // The DISTINCT refusal beside it: data that IS an object, but states no value for an
    // attribute the template names. The rows span the objects that state nothing for it.
    for (case, data) in [
        ("the empty object", "{}"),
        ("an object naming another attribute", r#"{"other":"Ada"}"#),
        ("an object whose member differs in case", r#"{"Name":"Ada"}"#),
    ] {
        let rendered = ses.render(template, data).await;
        refused(
            &format!("TestRenderEmailTemplate {case}"),
            &rendered,
            400,
            "BadRequestException",
            "Attribute 'name' is not present in the rendering data.",
        );
    }

    // And the other side of both: data that states the attribute renders the message.
    let rendered = ses.render(template, r#"{"name":"Ada"}"#).await;
    assert_eq!(
        rendered.status,
        200,
        "data that states every attribute must render, got {} ({})",
        rendered.status,
        rendered.message()
    );
}

#[tokio::test]
async fn an_unverified_sender_masks_which_templates_the_account_holds() {
    let ses = Ses::new();
    let template = "stored-template";
    assert_eq!(
        ses.create_template(template).await.status,
        200,
        "the template must be saved first"
    );

    // The same request against a template that exists and against names the account holds
    // nothing under: with the sender unverified the identity gate fires first, so every row
    // is the identical refusal and none of them says whether the template is there.
    for (case, named) in [
        ("a template the account holds", template),
        ("a name the account holds nothing under", "absent-template"),
        ("another name it holds nothing under", "also_absent_1"),
    ] {
        let sent = ses
            .send(json!({
                "FromEmailAddress": "stranger@example.com",
                "Destination": {"ToAddresses": ["reader@example.net"]},
                "Content": {"Template": {"TemplateName": named, "TemplateData": "{\"name\":\"Ada\"}"}},
            }))
            .await;
        refused(
            case,
            &sent,
            400,
            "MessageRejected",
            "Email address is not verified. The following identities failed the check in \
             region US-EAST-1: stranger@example.com",
        );
    }
}

#[tokio::test]
async fn a_list_element_outside_its_value_set_is_reported_against_the_list_member() {
    // The clause the walk reports for a value outside `SuppressionListReason`: named at the
    // LIST member, with the element's requirement nested and no position inside the list.
    let clause = |path: &str| {
        format!(
            "1 validation error detected: Value at '{path}'{SEPARATOR}Member must satisfy \
             constraint: [Member must satisfy enum value set: [{}]]",
            SUPPRESSION_REASONS.join(", ")
        )
    };

    // The rows span which element of the list is outside the set (the only one, the first of
    // two, the last of several), the shape the value takes (an unknown name, a modelled
    // value in the wrong case, the empty value) and WHERE the list sits in the request —
    // directly under the operation's input, and nested inside an option object.
    for (case, reasons) in [
        ("the only element", json!(["SPAM"])),
        ("the last of two", json!(["BOUNCE", "INVALID"])),
        ("the first of two", json!(["UNSUBSCRIBE", "COMPLAINT"])),
        ("the last of three", json!(["BOUNCE", "COMPLAINT", "SPAM"])),
        ("a modelled value in the wrong case", json!(["bounce"])),
        ("the empty value", json!([""])),
    ] {
        let ses = Ses::new();
        // A known state to refuse against, so the refusal is seen to leave it alone.
        let settled = json!({"SuppressedReasons": ["COMPLAINT"]});
        assert_eq!(
            ses.put_account_suppression(settled.clone()).await.status,
            200,
            "case {case:?}: the account must be prepared first"
        );

        let refusal = ses
            .put_account_suppression(json!({"SuppressedReasons": reasons}))
            .await;
        refused(
            &format!("PutAccountSuppressionAttributes {case}"),
            &refusal,
            400,
            "BadRequestException",
            &clause("suppressedReasons"),
        );
        assert_eq!(
            ses.account().await.member("SuppressionAttributes"),
            settled,
            "case {case:?}: the refused write must leave the account's reasons alone, got {:?}",
            ses.account().await.member("SuppressionAttributes")
        );

        // The same list, nested inside a create's `SuppressionOptions`: the clause names the
        // list member it sits at, whatever that path is.
        let created = ses
            .create_set(json!({
                "ConfigurationSetName": "nested_list_1",
                "SuppressionOptions": {"SuppressedReasons": reasons},
            }))
            .await;
        refused(
            &format!("CreateConfigurationSet {case}"),
            &created,
            400,
            "BadRequestException",
            &clause("suppressionOptions.suppressedReasons"),
        );
    }

    // The other side of the rule: every value the model declares is accepted, so the list
    // the clause names is the member's own and not a sentence that drifted from it.
    let ses = Ses::new();
    for reasons in [
        json!([]),
        json!(["BOUNCE"]),
        json!(["COMPLAINT"]),
        json!(SUPPRESSION_REASONS),
    ] {
        let written = ses
            .put_account_suppression(json!({"SuppressedReasons": reasons}))
            .await;
        assert_eq!(
            written.status,
            200,
            "case {reasons:?}: expected the modelled reasons to be accepted, got {} ({})",
            written.status,
            written.message()
        );
    }
}

#[tokio::test]
async fn the_suppression_options_write_words_its_own_invalid_reason() {
    let ses = Ses::new();
    let set = "suppression_wording";
    assert_eq!(
        ses.create_set(json!({"ConfigurationSetName": set})).await.status,
        200,
        "the set must be created first"
    );
    // The preferences the set holds while the table runs, so a refused write is seen to
    // leave them exactly as they were.
    let held = json!({"SuppressedReasons": ["BOUNCE"], "SuppressionScope": "TENANT"});
    assert_eq!(
        ses.put_suppression_options(set, held.clone()).await.status,
        200,
        "the preferences must be seeded first"
    );

    // The rows span which element is outside the value set and the shape it takes; the
    // sentence states the REJECTED value verbatim and the values the member takes.
    for (case, reasons, rejected) in [
        ("the only element", json!(["INVALID"]), "INVALID"),
        ("the last of two", json!(["BOUNCE", "SPAM"]), "SPAM"),
        (
            "the first of two",
            json!(["UNSUBSCRIBE", "COMPLAINT"]),
            "UNSUBSCRIBE",
        ),
        ("a modelled value in the wrong case", json!(["complaint"]), "complaint"),
    ] {
        let refusal = ses
            .put_suppression_options(set, json!({"SuppressedReasons": reasons}))
            .await;
        refused(
            case,
            &refusal,
            400,
            "BadRequestException",
            &format!(
                "Reason {rejected} is invalid, must be one of [{}].",
                SUPPRESSION_REASONS.join(", ")
            ),
        );
        let read = ses.get_set(set).await;
        assert_eq!(
            read.member("SuppressionOptions"),
            held,
            "case {case:?}: the refused write must store nothing, got {:?}",
            read.member("SuppressionOptions")
        );
    }

    // The other side: the modelled reasons are written and read back, so the refusal above
    // is about the value and not about the member.
    let written = ses
        .put_suppression_options(set, json!({"SuppressedReasons": SUPPRESSION_REASONS}))
        .await;
    assert_eq!(
        written.status,
        200,
        "expected the modelled reasons to be accepted, got {} ({})",
        written.status,
        written.message()
    );
    assert_eq!(
        ses.get_set(set).await.member("SuppressionOptions"),
        json!({"SuppressedReasons": SUPPRESSION_REASONS}),
        "the accepted write must be read back, got {:?}",
        ses.get_set(set).await.member("SuppressionOptions")
    );
}

#[tokio::test]
async fn an_unknown_sending_pool_is_refused_as_a_bad_request_parameter() {
    let ses = Ses::new();
    let set = "pool_parameter";
    assert_eq!(
        ses.create_set(json!({"ConfigurationSetName": set})).await.status,
        200,
        "the set must be created first"
    );

    // The rows span the form of the pool name: a plain one, one character, a hyphenated
    // name and the longest a caller is likely to send. None of them is a pool the account
    // holds, and the sentence is the same for each.
    for pool in ["warm-pool", "p", "pool_with_underscores", &"p".repeat(64)] {
        let written = ses
            .put_delivery_options(set, json!({"SendingPoolName": pool}))
            .await;
        refused(
            &format!("PutConfigurationSetDeliveryOptions {pool:?}"),
            &written,
            400,
            "BadRequestException",
            &format!("SendingPool <{pool}> doesn't exist"),
        );
        assert_eq!(
            ses.get_set(set).await.member("DeliveryOptions"),
            Value::Null,
            "case {pool:?}: the refused write must store nothing, got {:?}",
            ses.get_set(set).await.member("DeliveryOptions")
        );

        // The same member on the create path, which writes the same configuration.
        let created = ses
            .create_set(json!({
                "ConfigurationSetName": "pool_at_create",
                "DeliveryOptions": {"SendingPoolName": pool},
            }))
            .await;
        refused(
            &format!("CreateConfigurationSet {pool:?}"),
            &created,
            400,
            "BadRequestException",
            &format!("SendingPool <{pool}> doesn't exist"),
        );

        // The contrast that places the rule: a call that addresses the POOL itself, rather
        // than carrying its name as a parameter, is still answered with the modelled 404.
        let read = ses.get_pool(pool).await;
        refused(
            &format!("GetDedicatedIpPool {pool:?}"),
            &read,
            404,
            "NotFoundException",
            &format!("The requested pool <{pool}> does not exist."),
        );
    }

    // The other side of the rule: once the account holds the pool, the same association is
    // accepted and read back.
    let pool = "warm-pool";
    assert_eq!(
        ses.create_pool(pool).await.status,
        200,
        "creating the dedicated IP pool must answer 200"
    );
    let written = ses
        .put_delivery_options(set, json!({"SendingPoolName": pool}))
        .await;
    assert_eq!(
        written.status,
        200,
        "expected the association to be accepted, got {} ({})",
        written.status,
        written.message()
    );
    assert_eq!(
        ses.get_set(set).await.member("DeliveryOptions"),
        json!({"SendingPoolName": pool}),
        "the accepted association must be read back, got {:?}",
        ses.get_set(set).await.member("DeliveryOptions")
    );
}
