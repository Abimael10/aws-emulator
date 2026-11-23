// SPDX-License-Identifier: CC0-1.0

//! The LENGTH rule an `Attribute`'s text members state, driven end to end over the shared
//! wire harness in [`crate::ecs_wire`] through `PutAttributes`.
//!
//! The rule under test: `Attribute$name` and `Attribute$value` each state two rules — a
//! length ("must contain between 1 and 128 characters") and a character set — and a write
//! whose text falls outside the LENGTH is refused for its length, under the member's own
//! sentence, while a text of an admissible length is then judged by the characters in it and
//! refused, when it fails, under the character-set sentence instead. Either refusal applies
//! nothing, so what the resource already carried is left as it was.
//!
//! The docs that rule comes from:
//!
//! * `Attribute$name`: "The `name` must contain between 1 and 128 characters. The name may
//!   contain letters (uppercase and lowercase), numbers, hyphens (-), underscores (_),
//!   forward slashes (/), back slashes (\), or periods (.)."
//! * `Attribute$value`: "The `value` must contain between 1 and 128 characters. It can
//!   contain letters (uppercase and lowercase), numbers, hyphens (-), underscores (_),
//!   periods (.), at signs (@), forward slashes (/), back slashes (\), colons (:), or
//!   spaces. The value can't start or end with a space."
//! * `PutAttributes`: "Create or update an attribute on an Amazon ECS resource. If the
//!   attribute doesn't exist, it's created. If the attribute exists, its value is replaced
//!   with the specified value."
//! * `InvalidParameterException`: "The specified parameter isn't valid."

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::delete_attributes::{NAME_PATTERN_MESSAGE, VALUE_PATTERN_MESSAGE};
use crate::ecs_wire::{create_cluster, ok, refusal, service};
use crate::list_attributes::{list_attributes, register_with_attributes};

/// The cluster every case works in.
const CLUSTER: &str = "attribute-text";

/// The EC2 instance the fixture registers its container instance for.
const INSTANCE_ID: &str = "i-0bbb0000000000001";

/// The sentence a `name` whose LENGTH is outside the one `Attribute$name` documents is
/// refused with: the length is the member's own rule, so the refusal names the member whose
/// text was read and nothing else — the same sentence whichever end of the bound the text
/// fell outside.
pub(crate) const NAME_LENGTH_MESSAGE: &str = "Invalid length for attribute name.";

/// The same for a `value` outside the length `Attribute$value` documents.
pub(crate) const VALUE_LENGTH_MESSAGE: &str = "Invalid length for attribute value.";

/// The length bounds both members publish, read from the crate's own reading of them rather
/// than from a second copy of the number.
const MAX_TEXT: usize =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::MAX_ATTRIBUTE_TEXT_LENGTH;

/// One row of the attribute-text table: the case name, the `name` and the `value` the single
/// entry carries, and the sentence the write must be refused with — `None` for a text both
/// of the member's rules admit.
type TextRow = (String, String, String, Option<&'static str>);

/// A cluster holding one registered container instance, and that instance's ARN.
async fn fixture(case: &str) -> (MultiServiceRouter, String) {
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    let arn = register_with_attributes(&router, CLUSTER, INSTANCE_ID, &json!([]), case).await;
    (router, arn)
}

/// The attributes the resource at `arn` carries, as name and value, walked page by page so a
/// listing longer than one page cannot hide one.
async fn stored(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<(String, String)> {
    let mut carried: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..50 {
        let mut request = json!({ "cluster": CLUSTER, "targetType": "container-instance" });
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let (listed, next) = list_attributes(router, request, case).await;
        carried.extend(
            listed
                .into_iter()
                .filter(|(target_id, _, _)| target_id == arn)
                .map(|(_, name, value)| (name, value)),
        );
        match next {
            Some(next) => token = Some(next),
            None => {
                carried.sort();
                return carried;
            }
        }
    }
    panic!("case {case:?}: the listing of {CLUSTER:?} did not terminate");
}

/// One `PutAttributes` carrying one entry against `arn`.
fn write(arn: &str, name: &str, value: &str) -> Value {
    json!({
        "cluster": CLUSTER,
        "attributes": [{ "name": name, "value": value, "targetId": arn }],
    })
}

/// The rows differ along what the rule generalises over: WHICH member carries the text, how
/// its length misses the bound (no character at all, one character past the maximum, far
/// past it), the published bounds themselves, and a text of an admissible length that the
/// member's OTHER rule — its character set — refuses instead.
#[tokio::test]
async fn an_attribute_text_outside_its_documented_length_is_refused_for_its_length() {
    let (router, arn) = fixture("setup").await;
    let admitted_name = "tier";
    let admitted_value = "gold";

    let rows: Vec<TextRow> = vec![
        (
            "a name of no characters at all".to_string(),
            String::new(),
            admitted_value.to_string(),
            Some(NAME_LENGTH_MESSAGE),
        ),
        (
            "a name one character past the documented maximum".to_string(),
            "n".repeat(MAX_TEXT + 1),
            admitted_value.to_string(),
            Some(NAME_LENGTH_MESSAGE),
        ),
        (
            "a name far past the documented maximum".to_string(),
            "n".repeat(MAX_TEXT * 4),
            admitted_value.to_string(),
            Some(NAME_LENGTH_MESSAGE),
        ),
        (
            "a name of the documented minimum length".to_string(),
            "n".to_string(),
            admitted_value.to_string(),
            None,
        ),
        (
            "a name of the documented maximum length".to_string(),
            "n".repeat(MAX_TEXT),
            admitted_value.to_string(),
            None,
        ),
        (
            "a value of no characters at all".to_string(),
            admitted_name.to_string(),
            String::new(),
            Some(VALUE_LENGTH_MESSAGE),
        ),
        (
            "a value one character past the documented maximum".to_string(),
            admitted_name.to_string(),
            "v".repeat(MAX_TEXT + 1),
            Some(VALUE_LENGTH_MESSAGE),
        ),
        (
            "a value far past the documented maximum".to_string(),
            admitted_name.to_string(),
            "v".repeat(MAX_TEXT * 4),
            Some(VALUE_LENGTH_MESSAGE),
        ),
        (
            "a value of the documented minimum length".to_string(),
            admitted_name.to_string(),
            "v".to_string(),
            None,
        ),
        (
            "a value of the documented maximum length".to_string(),
            admitted_name.to_string(),
            "v".repeat(MAX_TEXT),
            None,
        ),
        (
            "a name of an admitted length carrying a character the member does not admit"
                .to_string(),
            "stack tier".to_string(),
            admitted_value.to_string(),
            Some(NAME_PATTERN_MESSAGE),
        ),
        (
            "a value of an admitted length that starts with a space".to_string(),
            admitted_name.to_string(),
            " gold".to_string(),
            Some(VALUE_PATTERN_MESSAGE),
        ),
        (
            "a value of an admitted length carrying a character the member does not admit"
                .to_string(),
            admitted_name.to_string(),
            "gold,silver".to_string(),
            Some(VALUE_PATTERN_MESSAGE),
        ),
    ];

    for (case, name, value, expected) in rows {
        match expected {
            Some(expected_message) => {
                let before = stored(&router, &arn, &case).await;
                let (code, message) =
                    refusal(&router, "PutAttributes", write(&arn, &name, &value), &case).await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    ("InvalidParameterException", expected_message),
                    "case {case:?}: expected the refusal this rule states, \
                     got {code} with {message:?}"
                );
                let after = stored(&router, &arn, &case).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: expected a refused write to apply nothing, got {after:?}"
                );
            }
            None => {
                ok(
                    &router,
                    "PutAttributes",
                    write(&arn, &name, &value),
                    &case,
                )
                .await;
                let carried = stored(&router, &arn, &case).await;
                assert!(
                    carried.contains(&(name.clone(), value.clone())),
                    "case {case:?}: expected the written attribute in the listing, \
                     got {carried:?}"
                );
            }
        }
    }
}

/// A write refused for the length of its text applies nothing, so the attribute the resource
/// already carried keeps the value it had — and the same name written with a text of an
/// admissible length replaces it.
#[tokio::test]
async fn a_write_refused_for_its_text_length_leaves_the_stored_value_as_it_was() {
    let (router, arn) = fixture("setup").await;
    let name = "stack";
    ok(
        &router,
        "PutAttributes",
        write(&arn, name, "production"),
        "the first write",
    )
    .await;

    // The rows differ along which end of the documented length the replacement's text misses.
    let refused: [(&str, String); 2] = [
        ("a replacement value of no characters at all", String::new()),
        (
            "a replacement value past the documented maximum",
            "v".repeat(MAX_TEXT + 1),
        ),
    ];
    for (case, value) in refused {
        let (code, message) =
            refusal(&router, "PutAttributes", write(&arn, name, &value), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("InvalidParameterException", VALUE_LENGTH_MESSAGE),
            "case {case:?}: expected the length refusal, got {code} with {message:?}"
        );
        let carried = stored(&router, &arn, case).await;
        assert!(
            carried.contains(&(name.to_string(), "production".to_string())),
            "case {case:?}: expected the stored value to be left as it was, got {carried:?}"
        );
    }

    let case = "a replacement value of an admitted length";
    ok(
        &router,
        "PutAttributes",
        write(&arn, name, "staging"),
        case,
    )
    .await;
    let carried = stored(&router, &arn, case).await;
    assert!(
        carried.contains(&(name.to_string(), "staging".to_string())),
        "case {case:?}: expected the replacement to be stored, got {carried:?}"
    );
}
