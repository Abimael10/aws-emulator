// SPDX-License-Identifier: CC0-1.0

//! What `UpdateOpsMetadata` does to the Application Manager object an `OpsMetadataArn` names.
//!
//! RULE: an update is addressed by ARN alone and never registers an object, so what it writes
//! to is the object a create minted that ARN for: each `MetadataToUpdate` entry is written
//! over the key it names, each `KeysToDelete` key is dropped, every other entry is left as
//! the last write persisted it, and the object is recorded as last written by the principal
//! that made the call. An ARN of this account naming no object is `OpsMetadataNotFoundException`
//! and one naming another ACCOUNT is `OpsMetadataInvalidArgumentException`, the two classes
//! the single-object reads answer and in the same order; a request naming no change at all is
//! that invalid-argument refusal too, and a write that would leave the object holding more
//! keys than this emulator keeps per object is `OpsMetadataKeyLimitExceededException`
//! (`smithy_query(query="operation", operation="UpdateOpsMetadata")` for every class).
//!
//! A metadata KEY is held to the modelled `MetadataKey` by the OPERATION
//! (`smithy_query(query="shape", shape="MetadataMap")`: the key targets `MetadataKey`,
//! `length 1..256`, `pattern ^(?!\s*$).+$`): a map's key is a path segment to the framework's
//! constraint walk rather than a constrained leaf, and that expression is a negative
//! lookahead no regular-expression engine of the walk compiles, so a key outside the shape
//! reaches the handler and is refused there — with the framework's own `ValidationException`
//! clause, wherever a key is carried: the update's map, the update's delete list and the
//! create's map.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the
//! multi-service router — and every entry an update writes is read back through
//! `GetOpsMetadata`, so a response that is not computed from persisted state cannot pass.
//!
//! The rows span what the rule generalises over: how many entries one update writes and at
//! the published ceiling of the map, how many keys it drops and at the published ceiling of
//! the list, whether a key it names is one the object holds, a key carried by both members,
//! the Region and account an ARN names, and where a key sits against the published bounds of
//! the key shape. What the create, the single-object reads and the delete do is
//! `all/ops_metadata.rs`'s subject, and what a listing reports is
//! `all/ops_metadata_listing.rs`'s.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router, ACCOUNT, REGION,
};

/// The `(key, value)` pairs of a `MetadataMap`.
type Entries<'a> = &'a [(&'a str, &'a str)];

/// A second Region, to pin that an update reaches the object held at the Region its create was
/// signed for. The first is the harness's own [`REGION`].
const OTHER_REGION: &str = "us-east-1";

/// An account other than the one this emulator serves, for the ARNs that name one.
const OTHER_ACCOUNT: &str = "123456789012";

/// The sentence an ARN of this account that names no object is refused with, written out here
/// independently of the service, as `all/ops_metadata.rs` writes it.
const NOT_FOUND: &str =
    "OpsMetadata object not found. Verify the Amazon Resource Name (ARN) and try again.";

/// The sentence an ARN naming another account is refused with, on the same terms.
const NOT_THE_OWNER: &str = "Invalid OpsMetadataArn. Requester is not the owner of the \
                             resource. Verify the Amazon Resource Name (ARN) and try again.";

/// The sentence an update naming neither metadata to write nor keys to drop is refused with:
/// such a request states no change for the object to take.
const NO_CHANGE: &str =
    "Invalid request. Specify MetadataToUpdate, KeysToDelete, or both, and try again.";

/// The published ceiling of `MetadataMap` (`smithy_query(query="operation",
/// operation="UpdateOpsMetadata")`: `MetadataToUpdate` has `length 1..5`), which is how many
/// entries ONE update may carry.
const METADATA_MAX: usize = 5;

/// The published ceiling of `MetadataKeysToDeleteList` (the same call: `KeysToDelete` has
/// `length 1..10`), which is how many keys ONE update may drop.
const KEYS_TO_DELETE_MAX: usize = 10;

/// The published ceiling of `MetadataKey`'s own `@length`
/// (`smithy_query(query="shape", shape="MetadataMap")`: the key targets `MetadataKey`,
/// `length 1..256`).
const METADATA_KEY_MAX_CHARACTERS: usize = 256;

/// How many metadata keys one OpsMetadata object holds in this emulator: its own ceiling,
/// past which a write is refused with the modelled key-limit refusal.
const KEYS_PER_OBJECT: usize = 20;

/// The sentence a write past [`KEYS_PER_OBJECT`] is refused with.
fn key_ceiling_message() -> String {
    format!("This emulator holds at most {KEYS_PER_OBJECT} metadata keys per OpsMetadata object")
}

/// The ARN an object registered for the application `name` in `region` is reached by, written
/// out here independently of the service, as `all/ops_metadata.rs` writes it.
fn ops_metadata_arn(region: &str, name: &str) -> String {
    format!("arn:aws:ssm:{region}:{ACCOUNT}:opsmetadata/aws/ssm/{name}/opsmetadata")
}

/// A `MetadataMap` body carrying one entry per `(key, value)` pair.
fn metadata(entries: Entries) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert((*key).to_string(), json!({ "Value": value }));
    }
    Value::Object(map)
}

/// The requirements a `MetadataKey` has to satisfy, as a refusal lists a constraint set:
/// written out here from the model's own declaration for that shape, longest bound first,
/// with the expression quoted the way this service publishes one for that shape — the `$`
/// the model closes the whole-value match with dropped and the `^` it opens with kept, which
/// is how the framework's own refusal of an element of `KeysToDelete` outside that shape's
/// length quotes it too.
fn metadata_key_requirements() -> String {
    format!(
        "[Member must have length less than or equal to {METADATA_KEY_MAX_CHARACTERS}, Member \
         must have length greater than or equal to 1, Member must satisfy regular expression \
         pattern: ^(?!\\s*$).+]"
    )
}

/// The whole message a MAP carrying a key outside `MetadataKey` is refused with: the
/// framework's envelope over one clause, stated at the map member, quoting the map the
/// request carried, over the key shape's whole set.
fn map_key_refusal(member_path: &str, entries: Entries) -> String {
    let mut rendered: Vec<(&str, &str)> = entries.to_vec();
    rendered.sort_unstable_by_key(|(key, _)| *key);
    let quoted: Vec<String> = rendered
        .iter()
        .map(|(key, value)| format!("{key}=MetadataValue(value={value})"))
        .collect();
    format!(
        "1 validation error detected: Value '{{{}}}' at '{member_path}' failed to satisfy \
         constraint: Map keys must satisfy constraint: {}",
        quoted.join(", "),
        metadata_key_requirements()
    )
}

/// The whole message a LIST carrying an element outside `MetadataKey` is refused with: the
/// same envelope, stated at the list member, quoting the list the request carried, over that
/// same set.
fn list_element_refusal(member_path: &str, keys: &[&str]) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at '{member_path}' failed to satisfy \
         constraint: Member must satisfy constraint: {}",
        keys.join(", "),
        metadata_key_requirements()
    )
}

/// `CreateOpsMetadata` for the application `name` at `region`'s endpoint carrying `entries`,
/// asserting it was served and returning the ARN it minted.
async fn create_in_region(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    entries: Entries<'_>,
) -> String {
    let mut request = json!({ "ResourceId": name });
    if !entries.is_empty() {
        request["Metadata"] = metadata(entries);
    }
    let (status, created) = call_in_region(router, region, "CreateOpsMetadata", &request).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the create at {region} must be served, got {status} with {created}"
    );
    created["OpsMetadataArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: a create reports an OpsMetadataArn, got {created}"))
        .to_string()
}

/// `CreateOpsMetadata` at [`REGION`]'s endpoint.
async fn create(router: &MultiServiceRouter, name: &str, entries: Entries<'_>) -> String {
    create_in_region(router, REGION, name, entries).await
}

/// `UpdateOpsMetadata` with `body` at [`REGION`]'s endpoint.
async fn update(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "UpdateOpsMetadata", body).await
}

/// The metadata entries `GetOpsMetadata` reports for `arn`, every page of them, asserting
/// each read was served: one page carries at most the published maximum of entries
/// (`GetOpsMetadataMaxResults` has `range 1..10`), so an object holding more is read by
/// resuming at the key each page hands back.
async fn entries_of(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let mut seen = serde_json::Map::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "OpsMetadataArn": arn });
        if let Some(token) = token.as_deref() {
            request["NextToken"] = json!(token);
        }
        let (status, read) = call(router, "GetOpsMetadata", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        let page = read["Metadata"]
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: a page carries Metadata, got {read}"))
            .clone();
        for (key, value) in page {
            assert!(
                seen.insert(key.clone(), value).is_none(),
                "case {case:?}: {key} is reported twice, got {read}"
            );
        }
        match read["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return Value::Object(seen),
        }
    }
}

/// The one object a listing of [`REGION`] reports for `arn`.
async fn summary_of(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, listing) = call(router, "ListOpsMetadata", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing must be served, got {status} with {listing}"
    );
    listing["OpsMetadataList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a listing reports an OpsMetadataList, got {listing}"))
        .iter()
        .find(|object| object["OpsMetadataArn"].as_str() == Some(arn))
        .unwrap_or_else(|| panic!("case {case:?}: the listing reports {arn}, got {listing}"))
        .clone()
}

/// RULE: what the update carries is what the object holds afterwards — each entry written
/// over the key it names, each named key dropped, everything else left as it was — and the
/// read reports exactly that.
#[tokio::test]
async fn an_update_writes_the_entries_it_carries_and_drops_the_keys_it_names() {
    /// One row: the entries the create persisted, what the update writes and drops, and the
    /// entries the object holds afterwards.
    struct Row {
        case: &'static str,
        created: Entries<'static>,
        to_update: Entries<'static>,
        to_delete: &'static [&'static str],
        held: Entries<'static>,
    }

    let rows = [
        Row {
            case: "an entry under a key the object does not hold is added",
            created: &[("owner", "retail")],
            to_update: &[("application-type", "Custom")],
            to_delete: &[],
            held: &[("owner", "retail"), ("application-type", "Custom")],
        },
        Row {
            case: "an entry under a key it holds replaces that entry",
            created: &[("owner", "retail"), ("application-type", "Custom")],
            to_update: &[("owner", "payments")],
            to_delete: &[],
            held: &[("owner", "payments"), ("application-type", "Custom")],
        },
        Row {
            case: "the published ceiling of entries in one update",
            created: &[("owner", "retail")],
            to_update: &[
                ("k0", "v0"),
                ("k1", "v1"),
                ("k2", "v2"),
                ("k3", "v3"),
                ("k4", "v4"),
            ],
            to_delete: &[],
            held: &[
                ("owner", "retail"),
                ("k0", "v0"),
                ("k1", "v1"),
                ("k2", "v2"),
                ("k3", "v3"),
                ("k4", "v4"),
            ],
        },
        Row {
            case: "a key the object holds is dropped",
            created: &[("owner", "retail"), ("application-type", "Custom")],
            to_update: &[],
            to_delete: &["owner"],
            held: &[("application-type", "Custom")],
        },
        Row {
            case: "several keys are dropped at once",
            created: &[("a", "1"), ("b", "2"), ("c", "3")],
            to_update: &[],
            to_delete: &["a", "c"],
            held: &[("b", "2")],
        },
        Row {
            case: "a key the object does not hold is nothing to drop",
            created: &[("a", "1")],
            to_update: &[],
            to_delete: &["nobody-wrote-this"],
            held: &[("a", "1")],
        },
        Row {
            case: "every entry of the object is dropped",
            created: &[("a", "1"), ("b", "2")],
            to_update: &[],
            to_delete: &["a", "b"],
            held: &[],
        },
        Row {
            case: "one update writes and drops at once",
            created: &[("a", "1"), ("b", "2")],
            to_update: &[("c", "3")],
            to_delete: &["a"],
            held: &[("b", "2"), ("c", "3")],
        },
        Row {
            case: "a key carried by both members is left holding the value written",
            created: &[("a", "1")],
            to_update: &[("a", "2")],
            to_delete: &["a"],
            held: &[("a", "2")],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let arn = create(&router, "MyApplication", row.created).await;

        let mut request = json!({ "OpsMetadataArn": arn });
        if !row.to_update.is_empty() {
            request["MetadataToUpdate"] = metadata(row.to_update);
        }
        if !row.to_delete.is_empty() {
            request["KeysToDelete"] = json!(row.to_delete);
        }
        let (status, updated) = update(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update must be served, got {status} with {updated}"
        );
        assert_eq!(
            updated["OpsMetadataArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: the update reports the object it wrote to, got {updated}"
        );

        let held = entries_of(&router, case, &arn).await;
        assert_eq!(
            held,
            metadata(row.held),
            "case {case:?}: expected {:?}, got {held}",
            row.held
        );
    }
}

/// RULE: the published ceiling of `KeysToDelete` is a list the update takes in full — every
/// key it names is dropped, and the keys it does not name are left.
#[tokio::test]
async fn the_published_ceiling_of_keys_to_delete_is_taken_in_full() {
    let router = router();
    // Ten keys to drop and one to keep, written five at a time (the published ceiling of
    // `MetadataToUpdate`) so the object holds more keys than one update can carry.
    let kept = ("kept", "value");
    let arn = create(&router, "MyApplication", &[kept]).await;
    let keys: Vec<String> = (0..KEYS_TO_DELETE_MAX).map(|n| format!("k{n}")).collect();
    for chunk in keys.chunks(METADATA_MAX) {
        let entries: Vec<(&str, &str)> = chunk.iter().map(|key| (key.as_str(), "v")).collect();
        let (status, written) = update(
            &router,
            &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(&entries) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the write of {chunk:?} must be served, got {status} with {written}"
        );
    }

    let (status, dropped) = update(
        &router,
        &json!({ "OpsMetadataArn": arn, "KeysToDelete": keys }),
    )
    .await;
    assert_eq!(
        status, 200,
        "dropping {KEYS_TO_DELETE_MAX} keys must be served, got {status} with {dropped}"
    );
    let held = entries_of(&router, "the published ceiling of keys to delete", &arn).await;
    assert_eq!(
        held,
        metadata(&[kept]),
        "every key named must be dropped and the one not named kept, got {held}"
    );
}

/// RULE: a served update is the object's last write — the listing reports it as written by
/// the principal that made the call, not before the create.
#[tokio::test]
async fn a_served_update_is_recorded_as_the_objects_last_write() {
    let router = router();
    let arn = create(&router, "MyApplication", &[("a", "1")]).await;
    let before = summary_of(&router, "before the update", &arn).await;
    let created = before["CreationDate"]
        .as_f64()
        .unwrap_or_else(|| panic!("a summary reports a CreationDate, got {before}"));

    let (status, updated) = update(
        &router,
        &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(&[("b", "2")]) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the update must be served, got {status} with {updated}"
    );

    let after = summary_of(&router, "after the update", &arn).await;
    let modified = after["LastModifiedDate"]
        .as_f64()
        .unwrap_or_else(|| panic!("a summary reports a LastModifiedDate, got {after}"));
    assert!(
        modified >= created,
        "expected the update's instant at or after the create's, got {modified} and {created}"
    );
    assert_eq!(
        after["CreationDate"].as_f64(),
        Some(created),
        "an update does not move the instant the object was created, got {after}"
    );
    assert!(
        !after["LastModifiedUser"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "expected the principal the update spoke for, got {after}"
    );
}

/// RULE: an ARN that names no object of this requester's is classified by WHOSE it is — the
/// two classes the single-object reads answer, and in the same order — and the refused update
/// writes nothing.
#[tokio::test]
async fn an_update_of_an_arn_naming_no_object_of_this_requester_is_classified_by_whose_it_is() {
    /// One row: the ARN the update names, the code it must be refused with and the sentence
    /// that refusal carries.
    struct Row {
        case: &'static str,
        arn: String,
        code: &'static str,
        message: &'static str,
    }

    let rows = [
        Row {
            case: "an ARN of this account and Region that names no object",
            arn: ops_metadata_arn(REGION, "NobodyCreatedThis"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN of the object another Region holds",
            arn: ops_metadata_arn(OTHER_REGION, "MyApplication"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN carrying no account at all",
            arn: format!("arn:aws:ssm:{REGION}::opsmetadata/aws/ssm/Nothing/opsmetadata"),
            code: "OpsMetadataNotFoundException",
            message: NOT_FOUND,
        },
        Row {
            case: "an ARN naming another account",
            arn: format!(
                "arn:aws:ssm:{REGION}:{OTHER_ACCOUNT}:opsmetadata/aws/ssm/Theirs/opsmetadata"
            ),
            code: "OpsMetadataInvalidArgumentException",
            message: NOT_THE_OWNER,
        },
        Row {
            case: "an ARN naming another account in another Region",
            arn: format!(
                "arn:aws:ssm:{OTHER_REGION}:{OTHER_ACCOUNT}:opsmetadata/aws/ssm/T/opsmetadata"
            ),
            code: "OpsMetadataInvalidArgumentException",
            message: NOT_THE_OWNER,
        },
    ];

    let router = router();
    // One object each Region holds, so a refusal is about the ARN rather than about an empty
    // store — and the other Region's object is reachable only through its own endpoint.
    let here = create(&router, "MyApplication", &[("a", "1")]).await;
    let there = create_in_region(&router, OTHER_REGION, "MyApplication", &[("b", "2")]).await;

    for row in rows {
        let case = row.case;
        let (status, body) = update(
            &router,
            &json!({
                "OpsMetadataArn": row.arn,
                "MetadataToUpdate": metadata(&[("written", "no")]),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected {}, got {body}",
            row.code
        );
        assert_eq!(
            error_message(&body),
            row.message,
            "case {case:?}: expected {:?}, got {:?}",
            row.message,
            error_message(&body)
        );
    }

    // Neither object was written by any refusal above.
    let held = entries_of(&router, "the object this Region holds", &here).await;
    assert_eq!(
        held,
        metadata(&[("a", "1")]),
        "a refused update writes nothing, got {held}"
    );
    let (status, read) = call_in_region(
        &router,
        OTHER_REGION,
        "GetOpsMetadata",
        &json!({ "OpsMetadataArn": there }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the other Region's object must still be served, got {status} with {read}"
    );
    assert_eq!(
        read["Metadata"],
        metadata(&[("b", "2")]),
        "an ARN of another Region names nothing here, so that object is untouched, got {read}"
    );
}

/// RULE: an update naming neither metadata to write nor keys to drop states no change for the
/// object to take, and is refused as an invalid argument without writing anything.
#[tokio::test]
async fn an_update_naming_no_change_at_all_is_refused() {
    let router = router();
    let arn = create(&router, "MyApplication", &[("a", "1")]).await;

    // (case, the ARN the update names)
    let rows: [(&str, String); 2] = [
        ("an object this account holds", arn.clone()),
        (
            "an ARN that names no object",
            ops_metadata_arn(REGION, "NobodyCreatedThis"),
        ),
    ];
    for (case, named) in rows {
        let (status, body) = update(&router, &json!({ "OpsMetadataArn": named })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "OpsMetadataInvalidArgumentException",
            "case {case:?}: expected the invalid-argument refusal, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NO_CHANGE,
            "case {case:?}: expected {NO_CHANGE:?}, got {:?}",
            error_message(&body)
        );
    }

    let held = entries_of(&router, "an update naming no change", &arn).await;
    assert_eq!(
        held,
        metadata(&[("a", "1")]),
        "a refused update writes nothing, got {held}"
    );
}

/// RULE: a metadata key the modelled `MetadataKey` does not admit is refused as a
/// constraint violation — whichever member carries it, and whether it is written, dropped or
/// created — and the object keeps the entries the last served write persisted.
#[tokio::test]
async fn a_metadata_key_outside_the_modelled_shape_is_refused() {
    // The published ceiling of the key shape's length, and one character past it.
    let longest = "k".repeat(METADATA_KEY_MAX_CHARACTERS);
    let too_long = "k".repeat(METADATA_KEY_MAX_CHARACTERS + 1);

    /// One row: the key a request carries and whether the shape admits it.
    struct Row {
        case: &'static str,
        key: String,
        admitted: bool,
    }

    let rows = [
        Row {
            case: "a key carrying something other than whitespace",
            key: "owner".to_string(),
            admitted: true,
        },
        Row {
            case: "a key at the published length ceiling",
            key: longest,
            admitted: true,
        },
        Row {
            case: "a key of one character",
            key: "k".to_string(),
            admitted: true,
        },
        Row {
            case: "a key carrying something and whitespace",
            key: " owner ".to_string(),
            admitted: true,
        },
        Row {
            case: "a key that is one space",
            key: " ".to_string(),
            admitted: false,
        },
        Row {
            case: "a key that is several spaces",
            key: "   ".to_string(),
            admitted: false,
        },
        Row {
            case: "a key that is a tab",
            key: "\t".to_string(),
            admitted: false,
        },
        Row {
            case: "a key that is a newline",
            key: "\n".to_string(),
            admitted: false,
        },
        Row {
            case: "a key with no characters at all",
            key: String::new(),
            admitted: false,
        },
        Row {
            case: "a key one character past the published length ceiling",
            key: too_long,
            admitted: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let key = row.key.as_str();
        let router = router();
        let arn = create(&router, "MyApplication", &[("held", "value")]).await;

        // The key carried as a map key the update WRITES.
        let entries: Entries = &[(key, "bad")];
        let (status, body) = update(
            &router,
            &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(entries) }),
        )
        .await;
        if row.admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: a key the shape admits must be written, got {status} with {body}"
            );
            let held = entries_of(&router, case, &arn).await;
            assert_eq!(
                held,
                metadata(&[("held", "value"), (key, "bad")]),
                "case {case:?}: the entry the update wrote must be read back, got {held}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: a key outside the shape must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the constraint refusal, got {body}"
        );
        assert_eq!(
            error_message(&body),
            map_key_refusal("metadataToUpdate", entries),
            "case {case:?}: expected {:?}, got {:?}",
            map_key_refusal("metadataToUpdate", entries),
            error_message(&body)
        );

        // The same key carried as an element of the list the update DROPS.
        let (status, body) = update(
            &router,
            &json!({ "OpsMetadataArn": arn, "KeysToDelete": [key] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a key to delete outside the shape must be refused, got {status} \
             with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the constraint refusal for the delete list, got {body}"
        );
        assert_eq!(
            error_message(&body),
            list_element_refusal("keysToDelete", &[key]),
            "case {case:?}: expected {:?}, got {:?}",
            list_element_refusal("keysToDelete", &[key]),
            error_message(&body)
        );

        // And the same key carried by the CREATE, which takes the same shape.
        let (status, body) = call(
            &router,
            "CreateOpsMetadata",
            &json!({ "ResourceId": "AnotherApplication", "Metadata": metadata(entries) }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the create must refuse the same key, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the constraint refusal from the create, got {body}"
        );
        assert_eq!(
            error_message(&body),
            map_key_refusal("metadata", entries),
            "case {case:?}: expected {:?}, got {:?}",
            map_key_refusal("metadata", entries),
            error_message(&body)
        );

        // Nothing any of the three refusals carried was written.
        let held = entries_of(&router, case, &arn).await;
        assert_eq!(
            held,
            metadata(&[("held", "value")]),
            "case {case:?}: a refused request writes nothing, got {held}"
        );
    }
}

/// RULE: the entries ONE object holds are bounded — a write that would leave it holding more
/// keys than this emulator keeps per object is refused and writes nothing, while a write that
/// does not add a key is served at the ceiling.
#[tokio::test]
async fn the_entries_one_object_holds_are_bounded() {
    let router = router();
    let keys: Vec<String> = (0..KEYS_PER_OBJECT).map(|n| format!("k{n}")).collect();
    // The create takes the published maximum of entries, and each update another five, up to
    // the ceiling.
    let first: Vec<(&str, &str)> = keys[..METADATA_MAX]
        .iter()
        .map(|key| (key.as_str(), "v"))
        .collect();
    let arn = create(&router, "MyApplication", &first).await;
    for chunk in keys[METADATA_MAX..].chunks(METADATA_MAX) {
        let entries: Vec<(&str, &str)> = chunk.iter().map(|key| (key.as_str(), "v")).collect();
        let (status, written) = update(
            &router,
            &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(&entries) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the write of {chunk:?} must be served up to the ceiling, got {status} with {written}"
        );
    }

    let at_the_ceiling: Vec<(&str, &str)> = keys.iter().map(|key| (key.as_str(), "v")).collect();
    let held = entries_of(&router, "at the ceiling", &arn).await;
    assert_eq!(
        held,
        metadata(&at_the_ceiling),
        "the object holds every entry the writes persisted, got {held}"
    );

    // One key past the ceiling is refused, and writes nothing.
    let (status, body) = update(
        &router,
        &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(&[("one-too-many", "v")]) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a key past the ceiling must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "OpsMetadataKeyLimitExceededException",
        "expected the key-limit refusal, got {body}"
    );
    assert_eq!(
        error_message(&body),
        key_ceiling_message(),
        "expected {:?}, got {:?}",
        key_ceiling_message(),
        error_message(&body)
    );
    let held = entries_of(&router, "after the refusal", &arn).await;
    assert_eq!(
        held,
        metadata(&at_the_ceiling),
        "a refused update writes nothing, got {held}"
    );

    // A write that adds no key is served at the ceiling: replacing an entry, and dropping one
    // to make room for another.
    let (status, replaced) = update(
        &router,
        &json!({ "OpsMetadataArn": arn, "MetadataToUpdate": metadata(&[("k0", "rewritten")]) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "replacing an entry at the ceiling must be served, got {status} with {replaced}"
    );
    let (status, traded) = update(
        &router,
        &json!({
            "OpsMetadataArn": arn,
            "MetadataToUpdate": metadata(&[("traded-in", "v")]),
            "KeysToDelete": ["k1"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "trading one key for another at the ceiling must be served, got {status} with {traded}"
    );

    let mut expected: Vec<(&str, &str)> = at_the_ceiling
        .iter()
        .copied()
        .filter(|(key, _)| *key != "k0" && *key != "k1")
        .collect();
    expected.push(("k0", "rewritten"));
    expected.push(("traded-in", "v"));
    let held = entries_of(&router, "after the writes at the ceiling", &arn).await;
    assert_eq!(
        held,
        metadata(&expected),
        "the object holds what the writes at the ceiling persisted, got {held}"
    );
}
