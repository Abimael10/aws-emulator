// SPDX-License-Identifier: CC0-1.0

//! `UpdateNotifyCodeConfiguration`: the mutable members of a stored notify code
//! configuration, changed over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch` (this suite's parent
//! harness), so it exercises the same restJson1 path a real SDK client takes: routing,
//! deserialization, boundary constraint validation, the handler, and error rendering.
//! The configuration each case updates is created by `CreateNotifyCodeConfiguration`
//! over that same wire and read back through `GetNotifyCodeConfiguration`, so what is
//! asserted is the state a caller actually wrote.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * an update changes only the members the request supplies: a supplied member
//!   replaces the stored value, a member the request carries this API's empty value for
//!   is cleared, and every member it omits — in the passcode policy, in each channel,
//!   and in the resource itself — keeps the value it had;
//! * a channel the request omits is untouched, a channel whose every member ends up
//!   cleared is dropped, and the whole channel structure goes with its last channel;
//! * a request that asks for no change changes nothing, `updatedAt` included, and
//!   repeating one update twice is the same as applying it once;
//! * a value outside a modelled bound is `ValidationException` (400) and each published
//!   bound itself is served;
//! * the configuration is addressed by its bare id or by its full ARN, and an
//!   identifier that names no stored configuration is `ResourceNotFoundException` (404)
//!   carrying this error class's templated sentence.

use serde_json::{json, Value};

use super::{Emulator, Reply, ACCOUNT_ID, CONFIGURATION_TYPE, SIGNING_NAME};

/// The configuration every case starts from: a name, deletion protection, every member
/// of the passcode policy, and a member of every channel — so a case that changes or
/// clears one member proves the others survived it.
///
/// `voiceMessageBodyTextType` is deliberately left out of the voice channel, so one case
/// can prove that a member absent from the stored record is ADDED by an update that
/// supplies it.
fn base_configuration() -> Value {
    json!({
        "notifyCodeConfigurationName": "signin-otp",
        "deletionProtectionEnabled": false,
        "codeConfigurationParameters": {
            "codeType": "NUMERIC",
            "codeLength": 6,
            "validityPeriodMinutes": 15,
            "maxAttempts": 3,
        },
        "channelParameters": {
            "text": {
                "inlineTemplateBody": "Your code is {{code}}",
                "destinationCountryParameters": { "US": "ORIG-1" },
            },
            "voice": {
                "inlineTemplateBody": "Your code is {{code}}",
                "languageCode": "en-US",
                "voiceId": "Joanna",
            },
            "notify": { "notifyTemplateId": "tmpl-1", "voiceId": "Matthew" },
            "whatsApp": { "whatsAppTemplateName": "otp_template", "languageCode": "en" },
        },
    })
}

/// One case of the update table: what it asks for, and which members of the stored
/// resource that is expected to change.
struct Case {
    what: &'static str,
    update: Value,
    /// The expected difference from the resource as it stood before the update: the
    /// `/`-separated path of each member, and the value it must now carry — or `None`
    /// for a member that must no longer be reported at all. An EMPTY list is a request
    /// that must change nothing.
    changes: Vec<(&'static str, Option<Value>)>,
}

/// The update table: the rows differ in which members they supply, in whether each
/// supplied member carries a value or this API's empty clear value, and in how many
/// members and channels one request touches at once.
fn cases() -> Vec<Case> {
    vec![
        Case {
            what: "no member at all",
            update: json!({}),
            changes: vec![],
        },
        Case {
            what: "the name alone",
            update: json!({ "notifyCodeConfigurationName": "signin otp-2" }),
            changes: vec![(
                "notifyCodeConfigurationName",
                Some(json!("signin otp-2")),
            )],
        },
        Case {
            what: "deletion protection alone",
            update: json!({ "deletionProtectionEnabled": true }),
            changes: vec![("deletionProtectionEnabled", Some(json!(true)))],
        },
        Case {
            what: "one member of the passcode policy",
            update: json!({ "codeConfigurationParameters": { "codeLength": 8 } }),
            changes: vec![("codeConfigurationParameters/codeLength", Some(json!(8)))],
        },
        Case {
            what: "two members of the passcode policy",
            update: json!({
                "codeConfigurationParameters": { "codeType": "ALPHA", "maxAttempts": 5 }
            }),
            changes: vec![
                ("codeConfigurationParameters/codeType", Some(json!("ALPHA"))),
                ("codeConfigurationParameters/maxAttempts", Some(json!(5))),
            ],
        },
        Case {
            what: "a passcode policy with no member set",
            update: json!({ "codeConfigurationParameters": {} }),
            changes: vec![],
        },
        Case {
            what: "one channel member replaced",
            update: json!({
                "channelParameters": { "text": { "inlineTemplateBody": "Code: {{code}}" } }
            }),
            changes: vec![(
                "channelParameters/text/inlineTemplateBody",
                Some(json!("Code: {{code}}")),
            )],
        },
        Case {
            what: "a channel member absent from the stored record added",
            update: json!({
                "channelParameters": { "voice": { "voiceMessageBodyTextType": "SSML" } }
            }),
            changes: vec![(
                "channelParameters/voice/voiceMessageBodyTextType",
                Some(json!("SSML")),
            )],
        },
        Case {
            what: "a string channel member cleared",
            update: json!({ "channelParameters": { "voice": { "languageCode": "" } } }),
            changes: vec![("channelParameters/voice/languageCode", None)],
        },
        Case {
            what: "the destination-country map cleared",
            update: json!({
                "channelParameters": { "text": { "destinationCountryParameters": {} } }
            }),
            changes: vec![("channelParameters/text/destinationCountryParameters", None)],
        },
        Case {
            what: "the destination-country map replaced whole",
            update: json!({
                "channelParameters": {
                    "text": { "destinationCountryParameters": { "GB": "ORIG-2" } }
                }
            }),
            changes: vec![(
                "channelParameters/text/destinationCountryParameters",
                Some(json!({ "GB": "ORIG-2" })),
            )],
        },
        Case {
            what: "every member of one channel cleared",
            update: json!({
                "channelParameters": { "notify": { "notifyTemplateId": "", "voiceId": "" } }
            }),
            changes: vec![("channelParameters/notify", None)],
        },
        Case {
            what: "a channel with no member set",
            update: json!({ "channelParameters": { "whatsApp": {} } }),
            changes: vec![],
        },
        Case {
            what: "two channels at once, one replaced and one cleared",
            update: json!({
                "channelParameters": {
                    "text": { "inlineTemplateBody": "Hi {{code}}" },
                    "whatsApp": { "languageCode": "" },
                }
            }),
            changes: vec![
                (
                    "channelParameters/text/inlineTemplateBody",
                    Some(json!("Hi {{code}}")),
                ),
                ("channelParameters/whatsApp/languageCode", None),
            ],
        },
        Case {
            what: "every channel cleared",
            update: json!({
                "channelParameters": {
                    "text": { "inlineTemplateBody": "", "destinationCountryParameters": {} },
                    "voice": { "inlineTemplateBody": "", "languageCode": "", "voiceId": "" },
                    "notify": { "notifyTemplateId": "", "voiceId": "" },
                    "whatsApp": { "whatsAppTemplateName": "", "languageCode": "" },
                }
            }),
            changes: vec![("channelParameters", None)],
        },
        Case {
            what: "every updatable member at once",
            update: json!({
                "notifyCodeConfigurationName": "everything_at_once",
                "deletionProtectionEnabled": true,
                "codeConfigurationParameters": {
                    "codeType": "ALPHANUMERIC",
                    "codeLength": 4,
                    "validityPeriodMinutes": 60,
                    "maxAttempts": 1,
                },
                "channelParameters": {
                    "text": { "inlineTemplateBody": "One time: {{code}}" },
                    "notify": { "notifyTemplateId": "tmpl-9" },
                },
            }),
            changes: vec![
                (
                    "notifyCodeConfigurationName",
                    Some(json!("everything_at_once")),
                ),
                ("deletionProtectionEnabled", Some(json!(true))),
                (
                    "codeConfigurationParameters",
                    Some(json!({
                        "codeType": "ALPHANUMERIC",
                        "codeLength": 4,
                        "validityPeriodMinutes": 60,
                        "maxAttempts": 1,
                    })),
                ),
                (
                    "channelParameters/text/inlineTemplateBody",
                    Some(json!("One time: {{code}}")),
                ),
                (
                    "channelParameters/notify",
                    Some(json!({ "notifyTemplateId": "tmpl-9", "voiceId": "Matthew" })),
                ),
            ],
        },
    ]
}

/// `UpdateNotifyCodeConfiguration` over the wire: a `PUT` on the configuration the
/// identifier names.
async fn update(em: &Emulator, id_or_arn: &str, body: Value) -> Reply {
    em.call(
        "PUT",
        &format!("/v1/notify-code-configurations/{id_or_arn}"),
        Some(body),
    )
    .await
}

/// `resource` with each listed member set to the value given, or removed where the
/// value is `None`: what the resource must look like once one case's update has been
/// applied to it.
fn with_changes(resource: &Value, changes: &[(&str, Option<Value>)]) -> Value {
    let mut expected = resource.clone();
    for (path, value) in changes {
        let segments: Vec<&str> = path.split('/').collect();
        let (last, parents) = segments
            .split_last()
            .unwrap_or_else(|| panic!("member path {path:?} names no member"));
        let mut cursor = &mut expected;
        for segment in parents {
            cursor = cursor
                .get_mut(segment)
                .unwrap_or_else(|| panic!("member path {path:?} does not reach {segment:?}"));
        }
        let object = cursor
            .as_object_mut()
            .unwrap_or_else(|| panic!("member path {path:?} does not reach an object"));
        match value {
            Some(value) => object.insert((*last).to_string(), value.clone()),
            None => object.remove(*last),
        };
    }
    expected
}

/// `resource` without `updatedAt`, which moves with the clock and so is asserted on its
/// own.
fn without_updated_at(resource: &Value) -> Value {
    let mut stripped = resource.clone();
    stripped
        .as_object_mut()
        .expect("the resource is an object")
        .remove("updatedAt");
    stripped
}

/// A reported `Timestamp`, as the whole epoch second the wire carries.
fn seconds(resource: &Value, member: &str) -> i64 {
    resource[member]
        .as_i64()
        .unwrap_or_else(|| panic!("member {member} is not an epoch second in {resource}"))
}

#[tokio::test]
async fn an_update_changes_only_the_members_it_supplies() {
    let em = Emulator::new();

    for case in cases() {
        let what = case.what;
        // A configuration of its own per case, so one case's write cannot be read as
        // another's.
        let created = em.create(base_configuration()).await;
        assert_eq!(
            created.status, 200,
            "case {what}: the create must be served, got {} / {:?}",
            created.status, created.body
        );
        let id = created.string("notifyCodeConfigurationId");
        let before = em.get(&id).await;
        let expected = with_changes(before.resource(), &case.changes);

        let updated = update(&em, &id, case.update.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {what}: expected 200, got {} / {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            without_updated_at(updated.resource()),
            without_updated_at(&expected),
            "case {what}: the update must change exactly {:?}, got {:?}",
            case.changes,
            updated.body
        );

        let before_updated_at = seconds(before.resource(), "updatedAt");
        let after_updated_at = seconds(updated.resource(), "updatedAt");
        if case.changes.is_empty() {
            assert_eq!(
                after_updated_at, before_updated_at,
                "case {what}: a request that changes nothing must leave updatedAt alone, got {:?}",
                updated.body
            );
        } else {
            assert!(
                after_updated_at >= seconds(updated.resource(), "createdAt")
                    && after_updated_at >= before_updated_at,
                "case {what}: updatedAt must not run behind createdAt or the previous \
                 update, got {:?}",
                updated.body
            );
        }

        // The update reports the record it wrote: a later reader sees the same thing.
        let read_back = em.get(&id).await;
        assert_eq!(
            read_back.resource(),
            updated.resource(),
            "case {what}: the configuration read back must be the one the update \
             reported, got {:?}",
            read_back.body
        );
    }
}

#[tokio::test]
async fn repeating_an_update_changes_nothing_the_second_time() {
    let em = Emulator::new();

    for case in cases() {
        let what = case.what;
        let created = em.create(base_configuration()).await;
        let id = created.string("notifyCodeConfigurationId");

        let once = update(&em, &id, case.update.clone()).await;
        assert_eq!(
            once.status, 200,
            "case {what}: expected 200, got {} / {:?}",
            once.status, once.body
        );
        let twice = update(&em, &id, case.update.clone()).await;
        assert_eq!(
            twice.status, 200,
            "case {what}: expected 200 on the repeat, got {} / {:?}",
            twice.status, twice.body
        );
        assert_eq!(
            twice.resource(),
            once.resource(),
            "case {what}: repeating one update must leave the configuration where the \
             first one left it, got {:?}",
            twice.body
        );
    }
}

/// The published window of each bounded member an update can carry, from the model:
/// `NotifyCodeConfigurationName` `@length(1..64)`, `CodeLength` `@range(4..8)`,
/// `ValidityPeriodMinutes` `@range(1..60)` and `MaxVerificationAttempts` `@range(1..5)`.
/// Each row is the member's path in the update body, the values at its published bounds,
/// and the values just past each of them.
fn bounded_members() -> Vec<(&'static str, Vec<Value>, Vec<Value>)> {
    vec![
        (
            "notifyCodeConfigurationName",
            vec![json!("a"), json!("N".repeat(64))],
            vec![json!(""), json!("N".repeat(65))],
        ),
        (
            "codeConfigurationParameters/codeLength",
            vec![json!(4), json!(8)],
            vec![json!(3), json!(9)],
        ),
        (
            "codeConfigurationParameters/validityPeriodMinutes",
            vec![json!(1), json!(60)],
            vec![json!(0), json!(61)],
        ),
        (
            "codeConfigurationParameters/maxAttempts",
            vec![json!(1), json!(5)],
            vec![json!(0), json!(6)],
        ),
    ]
}

/// An update body setting just `path` to `value`.
fn update_body(path: &str, value: &Value) -> Value {
    let mut body = json!({});
    let segments: Vec<&str> = path.split('/').collect();
    let (last, parents) = segments.split_last().expect("a member path");
    let mut cursor = &mut body;
    for segment in parents {
        cursor = cursor
            .as_object_mut()
            .expect("an object body")
            .entry((*segment).to_string())
            .or_insert_with(|| json!({}));
    }
    cursor
        .as_object_mut()
        .expect("an object body")
        .insert((*last).to_string(), value.clone());
    body
}

#[tokio::test]
async fn each_published_bound_is_served_and_one_past_it_is_refused() {
    let em = Emulator::new();
    let created = em.create(base_configuration()).await;
    let id = created.string("notifyCodeConfigurationId");

    for (path, accepted, refused) in bounded_members() {
        for value in accepted {
            let body = update_body(path, &value);
            let case = format!("{path}={value}");
            let updated = update(&em, &id, body).await;
            assert_eq!(
                updated.status, 200,
                "case {case}: a value at a published bound must be served, got {} / {:?}",
                updated.status, updated.body
            );
            let expected = updated.resource().clone();
            let reported = path
                .split('/')
                .fold(&expected, |resource, segment| &resource[segment]);
            assert_eq!(
                reported, &value,
                "case {case}: the member must be reported as supplied, got {:?}",
                updated.body
            );
        }
        for value in refused {
            let body = update_body(path, &value);
            let case = format!("{path}={value}");
            let refusal = update(&em, &id, body).await;
            assert_eq!(
                refusal.status, 400,
                "case {case}: a value past a published bound must be refused, got {} / {:?}",
                refusal.status, refusal.body
            );
            assert_eq!(
                refusal.error.as_deref(),
                Some("ValidationException"),
                "case {case}: expected ValidationException, got {:?} / {:?}",
                refusal.error,
                refusal.body
            );
        }
    }
}

#[tokio::test]
async fn an_update_resolves_the_configuration_by_bare_id_and_by_full_arn() {
    let em = Emulator::new();
    let created = em.create(base_configuration()).await;
    let id = created.string("notifyCodeConfigurationId");
    let arn = created.string("notifyCodeConfigurationArn");

    for (case, identifier, name) in [
        ("bare id", id.clone(), "named_by_id"),
        ("full arn", arn.clone(), "named_by_arn"),
    ] {
        let updated = update(
            &em,
            &identifier,
            json!({ "notifyCodeConfigurationName": name }),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            updated.string("notifyCodeConfigurationId"),
            id,
            "case {case}: both identifier forms must reach the one configuration, got {:?}",
            updated.body
        );
        assert_eq!(
            updated.string("notifyCodeConfigurationName"),
            name,
            "case {case}: the supplied name must be the one stored, got {:?}",
            updated.body
        );
        let read_back = em.get(&id).await;
        assert_eq!(
            read_back.string("notifyCodeConfigurationName"),
            name,
            "case {case}: the stored name must be the one the update wrote, got {:?}",
            read_back.body
        );
    }
}

#[tokio::test]
async fn an_identifier_that_names_no_configuration_is_not_found() {
    let em = Emulator::new();
    // A configuration exists, so what is refused is the identifier and not an empty
    // store.
    em.create(base_configuration()).await;

    let unknown = "ncc-000000000deadbeef";
    for (case, identifier) in [
        ("a bare id", unknown.to_string()),
        (
            "an arn",
            format!(
                "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/{unknown}"
            ),
        ),
    ] {
        let refusal = update(
            &em,
            &identifier,
            json!({ "notifyCodeConfigurationName": "renamed" }),
        )
        .await;
        assert_eq!(
            refusal.status, 404,
            "case {case}: expected 404, got {} / {:?}",
            refusal.status, refusal.body
        );
        assert_eq!(
            refusal.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {:?} / {:?}",
            refusal.error,
            refusal.body
        );
        assert_eq!(
            refusal.top("message"),
            format!(
                "Resource not found - ResourceType=\"{CONFIGURATION_TYPE}\" \
                 ResourceId=\"{identifier}\""
            ),
            "case {case}: the refusal must carry this error class's sentence, got {:?}",
            refusal.body
        );
    }
}
