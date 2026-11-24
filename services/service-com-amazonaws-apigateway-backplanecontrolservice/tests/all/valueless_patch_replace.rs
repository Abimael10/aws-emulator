// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `patchOperations` entry that names NO value: a `replace` whose
//! `value` is absent, and one whose `value` carries no characters.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder, the PATCH wrappers
//! and the `CreateRestApi`/`PutMethod`/`PutIntegration`/`CreateApiKey` helpers are this
//! crate's existing harness, reused from `rest_api_lifecycle`, `patch_updates`,
//! `method_integration` and `api_key_lifecycle`.
//!
//! The RULE under test: `value` is "applicable for the add or replace operation"
//! (`PatchOperation$value`) and is the member's new value, so a `replace` that names none —
//! by omitting `value` or by carrying no characters — names no value to set and UNSETS the
//! single-valued member its path addresses, the way a caller clears an optional field
//! through a path that publishes a `replace`; every later reading of the record then omits
//! that member. A single-valued member the service always reports a value for cannot be
//! unset, so such a request is refused with a modelled error and changes nothing.
//!
//! The tables span what the rule generalises over rather than one recorded call: the two
//! forms a `replace` names no value in, the member's modelled type (enum, nullable integer,
//! optional string, a string the service normalizes), the resource the member hangs off (an
//! integration, a REST API, an API key), and — on the refusal side — a string, an integer,
//! an enum-typed member and a flag the service reports a value for on every record.

use serde_json::{json, Value};

use crate::api_key_lifecycle::create_key;
use crate::method_integration::{get_method, put_integration, resource_with_a_method};
use crate::patch_updates::{op, patch, replace};
use crate::rest_api_lifecycle::{assert_failure, create_api, string_member, Gateway};

/// One row of either table: the case, the URI the record is patched and read at, the patch
/// path, the member that path addresses, the value that member is restored to before each
/// form is driven, and a member of the same record that must be reported unchanged
/// afterwards — the clear has to be confined to the member it names.
struct Row {
    case: &'static str,
    uri: String,
    path: &'static str,
    member: &'static str,
    restore: &'static str,
    sibling: &'static str,
    sibling_value: Value,
}

/// The two forms a `replace` names no value in: the `value` member absent from the entry
/// altogether, and a `value` carrying no characters.
fn forms(path: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("no value member at all", op("replace", path, None)),
        ("a value of no characters", replace(path, "")),
    ]
}

/// The integration URI of one method, which both `GetIntegration` and the PATCH are
/// addressed at.
fn integration_uri(api: &str, resource: &str, verb: &str) -> String {
    format!("/restapis/{api}/resources/{resource}/methods/{verb}/integration")
}

/// Assert `body` reports no `member` at all, which is what an unset optional member looks
/// like on the wire.
fn assert_absent(body: &Value, member: &str, case: &str, source: &str) {
    assert!(
        body.get(member).is_none_or(Value::is_null),
        "case {case}: {source} must omit the cleared {member}, got {:?}",
        body.get(member)
    );
}

/// The records every row patches, as the rows address them.
struct Seeded {
    /// The REST API's id, for the reading through `GetMethod`.
    api: String,
    /// `UpdateRestApi`/`GetRestApi` URI of that API.
    api_uri: String,
    /// The resource carrying the one method whose integration the rows patch.
    resource: String,
    /// `UpdateIntegration`/`GetIntegration` URI of that method's integration.
    integration: String,
    /// `UpdateApiKey`/`GetApiKey` URI of the seeded key.
    key_uri: String,
}

/// The verb of the one method the seeded integration hangs off.
const VERB: &str = "GET";

/// The three records every row patches: a REST API with each clearable member set, an HTTP
/// integration on one of its methods, and an API key.
async fn records(gateway: &Gateway, name: &str) -> Seeded {
    let (api, root) = create_api(gateway, name).await;
    let api_uri = format!("/restapis/{api}");
    let seeded = patch(
        gateway,
        &api_uri,
        vec![
            replace("/version", "2026-11-17"),
            replace("/description", "the seeded api"),
            replace("/minimumCompressionSize", "1"),
        ],
    )
    .await;
    assert_eq!(
        seeded.status, 200,
        "case {name}: expected 200 from the seeding UpdateRestApi, got {} with {:?}",
        seeded.status, seeded.body
    );

    let resource = resource_with_a_method(gateway, &api, &root, "pets", VERB, json!({})).await;
    let created = put_integration(
        gateway,
        &api,
        &resource,
        VERB,
        json!({
            "type": "HTTP",
            "httpMethod": "GET",
            "uri": "https://example.com/pets",
            "credentials": "arn:aws:iam::123456789012:role/apigateway-pets",
            "contentHandling": "CONVERT_TO_BINARY",
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {name}: expected 201 from PutIntegration, got {} with {:?}",
        created.status, created.body
    );

    let key = create_key(
        gateway,
        name,
        json!({ "name": "valueless-key", "description": "the seeded key", "enabled": true }),
    )
    .await;
    let key_id = string_member(&key, "id", name);
    Seeded {
        integration: integration_uri(&api, &resource, VERB),
        api,
        api_uri,
        resource,
        key_uri: format!("/apikeys/{key_id}"),
    }
}

#[tokio::test]
async fn a_valueless_replace_clears_the_member_it_addresses_and_later_reads_omit_it() {
    let gateway = Gateway::new();
    let seeded = records(&gateway, "valueless-replace-clears").await;
    let Seeded {
        api,
        api_uri,
        resource,
        integration,
        key_uri,
    } = &seeded;

    // Rows differ along the member's modelled type and the resource it hangs off; each is
    // driven in both forms a `replace` names no value in.
    let rows: Vec<Row> = vec![
        Row {
            case: "an enum-typed member of an integration",
            uri: integration.clone(),
            path: "/contentHandling",
            member: "contentHandling",
            restore: "CONVERT_TO_BINARY",
            sibling: "uri",
            sibling_value: json!("https://example.com/pets"),
        },
        Row {
            case: "an optional string member of an integration",
            uri: integration.clone(),
            path: "/credentials",
            member: "credentials",
            restore: "arn:aws:iam::123456789012:role/apigateway-pets",
            sibling: "httpMethod",
            sibling_value: json!("GET"),
        },
        Row {
            case: "a nullable integer member of a rest api",
            uri: api_uri.clone(),
            path: "/minimumCompressionSize",
            member: "minimumCompressionSize",
            restore: "1",
            sibling: "description",
            sibling_value: json!("the seeded api"),
        },
        Row {
            case: "an optional string member of a rest api",
            uri: api_uri.clone(),
            path: "/version",
            member: "version",
            restore: "2026-11-17",
            sibling: "id",
            sibling_value: json!(api),
        },
        Row {
            case: "a normalized string member of an api key",
            uri: key_uri.clone(),
            path: "/description",
            member: "description",
            restore: "the seeded key",
            sibling: "name",
            sibling_value: json!("valueless-key"),
        },
    ];

    for row in &rows {
        for (form, entry) in forms(row.path) {
            let case = format!("{} by {form}", row.case);
            // Each form clears a member that HAS a value, whatever the form before it did.
            let restored = patch(&gateway, &row.uri, vec![replace(row.path, row.restore)]).await;
            assert_eq!(
                restored.status, 200,
                "case {case}: expected 200 from the restoring patch, got {} with {:?}",
                restored.status, restored.body
            );
            assert!(
                restored.body.get(row.member).is_some(),
                "case {case}: the restoring patch must report {}, got {:?}",
                row.member,
                restored.body
            );

            let patched = patch(&gateway, &row.uri, vec![entry]).await;
            assert_eq!(
                patched.status, 200,
                "case {case}: expected 200 from the replace of {} with no value, got {} with {:?}",
                row.path, patched.status, patched.body
            );
            assert_absent(&patched.body, row.member, &case, "the patch answer");
            assert_eq!(
                patched.body[row.sibling], row.sibling_value,
                "case {case}: the patch answer must still report {} as {:?}, got {:?}",
                row.sibling, row.sibling_value, patched.body[row.sibling]
            );

            let read = gateway.call("GET", &row.uri, None).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected 200 from the read-back, got {} with {:?}",
                read.status, read.body
            );
            assert_absent(&read.body, row.member, &case, "the read-back");
            assert_eq!(
                read.body[row.sibling], row.sibling_value,
                "case {case}: the read-back must still report {} as {:?}, got {:?}",
                row.sibling, row.sibling_value, read.body[row.sibling]
            );
        }
    }

    // The integration is also read through the method that owns it, which embeds the same
    // stored record: a member cleared on the integration is gone from there too.
    let method = get_method(&gateway, api, resource, VERB).await;
    for member in ["contentHandling", "credentials"] {
        assert_absent(
            &method.body["methodIntegration"],
            member,
            "the embedded methodIntegration",
            "GetMethod",
        );
    }
}

#[tokio::test]
async fn a_valueless_replace_of_a_member_the_service_always_reports_is_refused_and_changes_nothing()
{
    let gateway = Gateway::new();
    let Seeded {
        api_uri,
        integration,
        key_uri,
        ..
    } = records(&gateway, "valueless-replace-refusals").await;

    // Rows differ along the modelled type of the member the service always reports a value
    // for: a string it fills in, an integer it defaults, an enum it defaults, the name a
    // rest api always carries, and a boolean flag. Each is driven in both forms a `replace`
    // names no value in.
    let rows: Vec<Row> = vec![
        Row {
            case: "the cache namespace the service fills in",
            uri: integration.clone(),
            path: "/cacheNamespace",
            member: "cacheNamespace",
            restore: "pets",
            sibling: "uri",
            sibling_value: json!("https://example.com/pets"),
        },
        Row {
            case: "the timeout the service defaults",
            uri: integration.clone(),
            path: "/timeoutInMillis",
            member: "timeoutInMillis",
            restore: "5000",
            sibling: "uri",
            sibling_value: json!("https://example.com/pets"),
        },
        Row {
            case: "the passthrough behaviour the service defaults",
            uri: integration.clone(),
            path: "/passthroughBehavior",
            member: "passthroughBehavior",
            restore: "WHEN_NO_MATCH",
            sibling: "uri",
            sibling_value: json!("https://example.com/pets"),
        },
        Row {
            case: "the name a rest api always reports",
            uri: api_uri.clone(),
            path: "/name",
            member: "name",
            restore: "valueless-replace-refusals",
            sibling: "version",
            sibling_value: json!("2026-11-17"),
        },
        Row {
            case: "the enabled flag an api key always reports",
            uri: key_uri.clone(),
            path: "/enabled",
            member: "enabled",
            restore: "true",
            sibling: "description",
            sibling_value: json!("the seeded key"),
        },
    ];

    for row in &rows {
        for (form, entry) in forms(row.path) {
            let case = format!("{} by {form}", row.case);
            let before = gateway.call("GET", &row.uri, None).await;
            assert_eq!(
                before.status, 200,
                "case {case}: expected 200 from the read before the patch, got {} with {:?}",
                before.status, before.body
            );
            let refused = patch(&gateway, &row.uri, vec![entry]).await;
            assert_failure(&refused, 400, "BadRequestException", &case);
            let after = gateway.call("GET", &row.uri, None).await;
            assert_eq!(
                after.body, before.body,
                "case {case}: a refused patch must leave the record as it was {:?}, got {:?}",
                before.body, after.body
            );
            assert_eq!(
                after.body[row.sibling], row.sibling_value,
                "case {case}: the record must still report {} as {:?}, got {:?}",
                row.sibling, row.sibling_value, after.body[row.sibling]
            );
            assert!(
                after.body.get(row.member).is_some(),
                "case {case}: the member the service always reports must still be there, \
                 got {:?}",
                after.body
            );
            // The member is still settable through the same path, so the refusals above are
            // about the absent value rather than about the path.
            let restored = patch(&gateway, &row.uri, vec![replace(row.path, row.restore)]).await;
            assert_eq!(
                restored.status, 200,
                "case {case}: expected 200 from the restoring patch, got {} with {:?}",
                restored.status, restored.body
            );
        }
    }
}
