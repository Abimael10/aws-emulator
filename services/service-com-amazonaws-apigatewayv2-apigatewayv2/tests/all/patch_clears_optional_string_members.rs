// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ONE rule this service's patches share, across the resources
//! that hold such a member.
//!
//! The rule these cases assert: **a PATCH that carries an OPTIONAL string member as the
//! EMPTY string REMOVES the stored value, so neither the patch's own answer nor any later
//! read of the resource reports that member** — an empty value describes no value to hold,
//! so it asks for the stored one to be cleared rather than rewritten, and a cleared member
//! is left out of the serialized document instead of being reported as a zero-length
//! string. Only the empty string clears: any other text, whitespace included, is a value
//! and is stored as written, and clearing a member that is already clear is the state the
//! call asks for, so the patch is idempotent and a value sent afterwards is stored again.
//!
//! The rows span what the rule generalises over: WHICH operation applies the patch
//! (`UpdateApi`, `UpdatePortalProduct`, `UpdatePortal` — a top-level api, a portal product
//! and a portal, three resources of three different collections answering two different
//! success statuses), WHICH member of the resource carries it, and the FORM and LENGTH of
//! the value being cleared (a short value, one at the member's modelled maximum, one
//! outside ASCII). The recorded case that filed the issue — an api's `description` cleared
//! with the empty string, whose answer must carry no `description` member at all — is ONE
//! row of the first table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator};
use crate::portal_lifecycle::portal_content;

/// The longest `Description` an api and a portal product take:
/// `StringWithLengthBetween0And1024` (`UpdateApi$Description`) and `__stringMin0Max1024`
/// (`UpdatePortalProduct$Description`) both bound it at 1024.
const MAX_DESCRIPTION_LEN: usize = 1024;

/// The longest `Version` an api takes: `StringWithLengthBetween1And64`
/// (`UpdateApi$Version`).
const MAX_VERSION_LEN: usize = 64;

/// The longest `RumAppMonitorName` a portal takes: `__stringMin0Max255`
/// (`UpdatePortal$RumAppMonitorName`).
const MAX_RUM_APP_MONITOR_NAME_LEN: usize = 255;

/// Which resource a row patches: the collection it is created in, the status its patch
/// answers, and the request that creates one.
#[derive(Clone, Copy, Debug)]
enum Resource {
    /// `CreateApi`/`UpdateApi`, `POST`/`PATCH /v2/apis`.
    Api,
    /// `CreatePortalProduct`/`UpdatePortalProduct`, `POST`/`PATCH /v2/portalproducts`.
    PortalProduct,
    /// `CreatePortal`/`UpdatePortal`, `POST`/`PATCH /v2/portals`.
    Portal,
}

impl Resource {
    /// The collection URI this resource is created in and hangs off.
    fn collection(self) -> &'static str {
        match self {
            Self::Api => "/v2/apis",
            Self::PortalProduct => "/v2/portalproducts",
            Self::Portal => "/v2/portals",
        }
    }

    /// The member the create's answer reports the minted id under.
    fn id_member(self) -> &'static str {
        match self {
            Self::Api => "apiId",
            Self::PortalProduct => "portalProductId",
            Self::Portal => "portalId",
        }
    }

    /// The status this resource's PATCH answers — the status API Gateway answers each of
    /// these patches with, which is not the same for all three.
    fn patch_status(self) -> u16 {
        match self {
            Self::Api => 201,
            Self::PortalProduct | Self::Portal => 200,
        }
    }

    /// A create request for this resource carrying `member` set to `value`, beside the
    /// members the create requires.
    fn create_request(self, name: &str, member: &str, value: &str) -> Value {
        let mut request = match self {
            Self::Api => json!({ "name": name, "protocolType": "HTTP" }),
            Self::PortalProduct => json!({ "displayName": name }),
            Self::Portal => json!({
                "authorization": { "none": {} },
                "endpointConfiguration": { "none": {} },
                "portalContent": portal_content(name)
            }),
        };
        request[member] = json!(value);
        request
    }
}

/// One row: the resource, the member cleared, the value it is created with, and a value it
/// is set to again after the clear.
struct Row {
    case: &'static str,
    resource: Resource,
    member: &'static str,
    created: String,
    restored: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an api's description, the recorded case",
            resource: Resource::Api,
            member: "description",
            created: "an HTTP API with explicit default selection expressions".to_string(),
            restored: "re-described after the clear",
        },
        Row {
            case: "an api's description at the member's modelled maximum length",
            resource: Resource::Api,
            member: "description",
            created: "d".repeat(MAX_DESCRIPTION_LEN),
            restored: "short again",
        },
        Row {
            case: "an api's description written outside ASCII",
            resource: Resource::Api,
            member: "description",
            created: "決済 API — ünïcödé, emoji 🎉, and a combining é".to_string(),
            restored: "ascii again",
        },
        Row {
            case: "an api's version",
            resource: Resource::Api,
            member: "version",
            created: "2026-01-01".to_string(),
            restored: "2026-02-01",
        },
        Row {
            case: "an api's version at the member's modelled maximum length",
            resource: Resource::Api,
            member: "version",
            created: "v".repeat(MAX_VERSION_LEN),
            restored: "v2",
        },
        Row {
            case: "a portal product's description",
            resource: Resource::PortalProduct,
            member: "description",
            created: "everything a payments integrator needs".to_string(),
            restored: "re-described after the clear",
        },
        Row {
            case: "a portal product's description at the member's modelled maximum length",
            resource: Resource::PortalProduct,
            member: "description",
            created: "d".repeat(MAX_DESCRIPTION_LEN),
            restored: "short again",
        },
        Row {
            case: "a portal's RUM app monitor name",
            resource: Resource::Portal,
            member: "rumAppMonitorName",
            created: "portal-rum".to_string(),
            restored: "portal-rum-again",
        },
        Row {
            case: "a portal's RUM app monitor name at the member's modelled maximum length",
            resource: Resource::Portal,
            member: "rumAppMonitorName",
            created: "m".repeat(MAX_RUM_APP_MONITOR_NAME_LEN),
            restored: "short-rum",
        },
    ]
}

/// Create one resource carrying `member` set to `value`, and answer the URI the single
/// resource hangs off — the one its reader and its patch are both bound to.
async fn create_resource(
    emulator: &MultiServiceRouter,
    case: &str,
    resource: Resource,
    name: &str,
    member: &str,
    value: &str,
) -> String {
    let request = resource.create_request(name, member, value);
    let (status, created) = call(emulator, "POST", resource.collection(), Some(request)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 creating the resource, got {status} {created}"
    );
    let id = as_str(&created, resource.id_member());
    assert!(
        !id.is_empty(),
        "case {case:?}: expected the create to report a minted id, got {created}"
    );
    format!("{}/{id}", resource.collection())
}

/// Read one resource back, asserting the read succeeded.
async fn get_resource(emulator: &MultiServiceRouter, case: &str, uri: &str) -> Value {
    let (status, fetched) = call(emulator, "GET", uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 reading {uri} back, got {status} {fetched}"
    );
    fetched
}

/// Patch one resource, asserting the patch answered the status its operation answers.
async fn patch_resource(
    emulator: &MultiServiceRouter,
    case: &str,
    resource: Resource,
    uri: &str,
    patch: Value,
) -> Value {
    let (status, patched) = call(emulator, "PATCH", uri, Some(patch)).await;
    assert_eq!(
        status,
        resource.patch_status(),
        "case {case:?}: expected {} patching {uri}, got {status} {patched}",
        resource.patch_status()
    );
    patched
}

#[tokio::test]
async fn an_optional_string_member_carried_empty_is_removed_from_every_reader() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let member = row.member;
        let name = format!("cleared-{index}");
        let uri = create_resource(
            &emulator,
            case,
            row.resource,
            &name,
            member,
            &row.created,
        )
        .await;

        // (1) The resource carries the member, so there is a stored value to clear.
        let created = get_resource(&emulator, case, &uri).await;
        assert_eq!(
            created.get(member).and_then(Value::as_str),
            Some(row.created.as_str()),
            "case {case:?}: the created resource must carry {member}, got {created}"
        );

        // (2) The empty string clears it: the patch's own answer reports no such member.
        let cleared = patch_resource(
            &emulator,
            case,
            row.resource,
            &uri,
            json!({ member: "" }),
        )
        .await;
        assert_eq!(
            cleared.get(member),
            None,
            "case {case:?}: expected the patch to report no {member} member, got {cleared}"
        );

        // (3) And the clear was persisted, not echoed: no later read reports it either.
        let fetched = get_resource(&emulator, case, &uri).await;
        assert_eq!(
            fetched.get(member),
            None,
            "case {case:?}: expected the reader to report no {member} member, got {fetched}"
        );

        // (4) Clearing a member that is already clear is the state the call asks for, so
        // the rule is idempotent and the second answer is the first one again.
        let again = patch_resource(
            &emulator,
            case,
            row.resource,
            &uri,
            json!({ member: "" }),
        )
        .await;
        assert_eq!(
            again.get(member),
            None,
            "case {case:?}: expected a second clear to report no {member} member, got {again}"
        );

        // (5) A value sent afterwards is stored as written, so the clear removed the value
        // rather than the member's ability to hold one.
        let restored = patch_resource(
            &emulator,
            case,
            row.resource,
            &uri,
            json!({ member: row.restored }),
        )
        .await;
        assert_eq!(
            restored.get(member).and_then(Value::as_str),
            Some(row.restored),
            "case {case:?}: expected the patch to report {member}={:?}, got {restored}",
            row.restored
        );
        let read_back = get_resource(&emulator, case, &uri).await;
        assert_eq!(
            read_back.get(member).and_then(Value::as_str),
            Some(row.restored),
            "case {case:?}: expected the reader to report {member}={:?}, got {read_back}",
            row.restored
        );
    }
}

#[tokio::test]
async fn only_the_empty_value_clears_the_member() {
    let emulator = emulator();

    // Values that are TEXT rather than nothing: a single space, a run of whitespace, and a
    // single character. Each is stored as written, so the member is still reported — only
    // the zero-length value asks for the stored one to be removed.
    let values = [
        ("a single space", " "),
        ("a run of whitespace", " \t "),
        ("one character", "x"),
    ];

    for (index, row) in rows().into_iter().enumerate() {
        for (value_index, (value_case, value)) in values.into_iter().enumerate() {
            let case = format!("{} patched to {value_case}", row.case);
            let member = row.member;
            let name = format!("kept-{index}-{value_index}");
            let uri = create_resource(
                &emulator,
                &case,
                row.resource,
                &name,
                member,
                &row.created,
            )
            .await;

            let patched = patch_resource(
                &emulator,
                &case,
                row.resource,
                &uri,
                json!({ member: value }),
            )
            .await;
            assert_eq!(
                patched.get(member).and_then(Value::as_str),
                Some(value),
                "case {case:?}: expected the patch to report {member}={value:?}, got {patched}"
            );
            let fetched = get_resource(&emulator, &case, &uri).await;
            assert_eq!(
                fetched.get(member).and_then(Value::as_str),
                Some(value),
                "case {case:?}: expected the reader to report {member}={value:?}, got {fetched}"
            );
        }
    }
}
