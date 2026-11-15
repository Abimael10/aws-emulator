// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a tag `TagResource` will not apply earns — end to end over the wire.
//!
//! Two different layers refuse two different conditions, and the rule this suite pins is
//! that they stay apart:
//!
//! * a key or a value outside the LENGTH the model's `Tag` documentation states for the
//!   shapes a tag is built from (`Tag$Key` "The maximum length of a tag key is 128
//!   characters. The minimum length is 1 character", `Tag$Value` "The maximum length of a
//!   tag value is 256 characters") breaks a bound of the SHAPES, so it is refused where a
//!   request is validated against its shapes: `ValidationException`, carrying no message
//!   of this service's at all;
//! * a tag the shapes accept but the service will not store — one carrying the prefix
//!   `Tag` reserves ("The `aws:` prefix is reserved for use by Amazon Web Services; you
//!   can't use it in any tag keys or values that you define") — is refused with the
//!   operation's own modelled input error, `BadRequestException`, worded by the fixed
//!   sentence this surface answers a tagging call it will not apply, with nothing of the
//!   request interpolated into it.
//!
//! Either way the call applies NONE of the batch it carried: the tags the resource already
//! held are exactly what a later caller still reads.
//!
//! The refusal table spans what the rule generalises over: which HALF of the pair is at
//! fault (key, value), WHERE in the batch the offending tag sits (only, first, second,
//! last), how far past the bound the length is (one character, many), and which end of the
//! bound is broken (under the minimum, over the maximum). The accepted table spans the rows
//! that sit just inside each bound and the key that carries the reserved text somewhere
//! other than at its start, so neither rule is read wider than it is.

use serde_json::{json, Value};

use crate::resource_tags::{arn, tag, Ses, ACCOUNT_ID, HOME, SHAPE_BOUND_ERROR, TAG_WRITE_FAILED};

/// The longest a tag key may be, taken from the crate's own constant: `Tag$Key` documents
/// "The maximum length of a tag key is 128 characters. The minimum length is 1 character"
/// (`smithy_query(query="doc",shape="Tag",member="Key")`).
const MAX_TAG_KEY_CHARS: usize =
    service_com_amazonaws_sesv2_simpleemailservice_v2::state::MAX_TAG_KEY_CHARS;

/// The longest a tag value may be, from the same source: `Tag$Value` documents "The maximum
/// length of a tag value is 256 characters. The minimum length is 0 characters".
const MAX_TAG_VALUE_CHARS: usize =
    service_com_amazonaws_sesv2_simpleemailservice_v2::state::MAX_TAG_VALUE_CHARS;

/// The modelled input error of this operation: "The input you provided is invalid"
/// (`BadRequestException`, HTTP 400).
const BAD_REQUEST: &str = "BadRequestException";

/// The configuration set every row below tags, and the one tag it already carries, so each
/// row can prove the refused batch left the stored set exactly as it was.
const HELD_SET: &str = "tag-layers";

/// The tag the held configuration set is created with.
fn baseline() -> Vec<(String, String)> {
    vec![tag("sim2real-owner", "parity")]
}

/// Create the held configuration set with its baseline tag and answer the ARN that
/// addresses it.
async fn held_configuration_set(ses: &Ses) -> String {
    let tags: Vec<Value> = baseline()
        .iter()
        .map(|(key, value)| json!({"Key": key, "Value": value}))
        .collect();
    let created = ses
        .call(
            "POST",
            "/v2/email/configuration-sets",
            Some(json!({"ConfigurationSetName": HELD_SET, "Tags": tags})),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "creating the configuration set {HELD_SET:?} answered {} {:?}",
        created.status,
        created.message()
    );
    arn(HOME, ACCOUNT_ID, "configuration-set", HELD_SET)
}

/// Which layer refuses a row's batch, and so which error the row earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// A bound of the SHAPES a request is built from: refused with no message.
    Shapes,
    /// A restriction the SERVICE enforces on a tag the shapes accept: refused with this
    /// operation's modelled input error and the surface's own fixed sentence.
    Service,
}

impl Layer {
    /// The wire error code a refusal of this layer carries.
    fn error_code(self) -> &'static str {
        match self {
            Self::Shapes => SHAPE_BOUND_ERROR,
            Self::Service => BAD_REQUEST,
        }
    }

    /// The message a refusal of this layer carries.
    fn message(self) -> String {
        match self {
            Self::Shapes => String::new(),
            Self::Service => TAG_WRITE_FAILED.to_string(),
        }
    }
}

/// One refused row: the batch to submit and the layer that refuses it.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    tags: Vec<(String, String)>,
    layer: Layer,
}

/// The refused table.
fn refused_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the only key carries the reserved prefix",
            tags: vec![tag("aws:sim2real", "nope")],
            layer: Layer::Service,
        },
        Row {
            case: "the key is the reserved prefix and nothing else",
            tags: vec![tag("aws:", "nope")],
            layer: Layer::Service,
        },
        Row {
            case: "the VALUE carries the reserved prefix",
            tags: vec![tag("creator", "aws:ses")],
            layer: Layer::Service,
        },
        Row {
            case: "the second key of a two-tag batch carries the reserved prefix",
            tags: vec![tag("env", "prod"), tag("aws:sim2real", "nope")],
            layer: Layer::Service,
        },
        Row {
            case: "the first key of a three-tag batch carries the reserved prefix",
            tags: vec![
                tag("aws:sim2real", "nope"),
                tag("env", "prod"),
                tag("owner", "platform"),
            ],
            layer: Layer::Service,
        },
        Row {
            case: "the last key of a three-tag batch carries the reserved prefix",
            tags: vec![
                tag("env", "prod"),
                tag("owner", "platform"),
                tag("aws:sim2real", "nope"),
            ],
            layer: Layer::Service,
        },
        Row {
            case: "a key one character past the maximum the shapes state",
            tags: vec![tag(&"k".repeat(MAX_TAG_KEY_CHARS + 1), "prod")],
            layer: Layer::Shapes,
        },
        Row {
            case: "a key far past the maximum the shapes state",
            tags: vec![tag(&"k".repeat(MAX_TAG_KEY_CHARS * 3), "prod")],
            layer: Layer::Shapes,
        },
        Row {
            case: "a key under the minimum the shapes state, carrying no character at all",
            tags: vec![tag("", "prod")],
            layer: Layer::Shapes,
        },
        Row {
            case: "a value one character past the maximum the shapes state",
            tags: vec![tag("env", &"v".repeat(MAX_TAG_VALUE_CHARS + 1))],
            layer: Layer::Shapes,
        },
        Row {
            case: "the second key of a batch is past the maximum the shapes state",
            tags: vec![
                tag("env", "prod"),
                tag(&"k".repeat(MAX_TAG_KEY_CHARS + 1), "nope"),
            ],
            layer: Layer::Shapes,
        },
        Row {
            case: "one tag breaks the shapes' bound and another carries the reserved prefix",
            // The shapes are validated before any rule of the operation's own runs, so the
            // outer layer is the one that answers a batch breaking both.
            tags: vec![
                tag(&"k".repeat(MAX_TAG_KEY_CHARS + 1), "nope"),
                tag("aws:sim2real", "nope"),
            ],
            layer: Layer::Shapes,
        },
    ]
}

#[tokio::test]
async fn a_refused_tag_earns_the_refusal_of_the_layer_its_fault_belongs_to() {
    for row in refused_rows() {
        let ses = Ses::new();
        let resource_arn = held_configuration_set(&ses).await;

        let reply = ses.tag_resource(&resource_arn, &row.tags).await;
        assert_eq!(
            reply.status,
            400,
            "case {:?}: TagResource answered {} {:?}, expected 400",
            row.case,
            reply.status,
            reply.message()
        );
        assert_eq!(
            reply.error_code(),
            row.layer.error_code(),
            "case {:?}: expected the {:?} layer's {}, got {:?}",
            row.case,
            row.layer,
            row.layer.error_code(),
            reply.error_code()
        );
        assert_eq!(
            reply.message(),
            row.layer.message(),
            "case {:?}: expected the message {:?}, got {:?}",
            row.case,
            row.layer.message(),
            reply.message()
        );

        // NOTHING of the batch was applied: the resource still carries exactly the tag it
        // was created with, including the acceptable tags of a batch refused for one tag.
        let still_held = ses.list_tags(&resource_arn).await.tags("Tags");
        assert_eq!(
            still_held,
            baseline(),
            "case {:?}: after the refusal the configuration set carried {still_held:?}, \
             expected {:?}",
            row.case,
            baseline()
        );
    }
}

#[tokio::test]
async fn the_refusal_of_a_tag_the_service_will_not_store_states_nothing_of_the_request() {
    for row in refused_rows()
        .into_iter()
        .filter(|row| row.layer == Layer::Service)
    {
        let ses = Ses::new();
        let resource_arn = held_configuration_set(&ses).await;

        let message = ses.tag_resource(&resource_arn, &row.tags).await.message();
        for (key, value) in &row.tags {
            assert!(
                !message.contains(key.as_str()),
                "case {:?}: the refusal states the key {key:?} it was given: {message:?}",
                row.case
            );
            if !value.is_empty() {
                assert!(
                    !message.contains(value.as_str()),
                    "case {:?}: the refusal states the value {value:?} it was given: \
                     {message:?}",
                    row.case
                );
            }
        }
        assert!(
            !message.contains(&resource_arn) && !message.contains(HELD_SET),
            "case {:?}: the refusal states the resource it was given: {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_two_layers_do_not_collapse_into_one_refusal() {
    let ses = Ses::new();
    let resource_arn = held_configuration_set(&ses).await;

    let reserved = ses
        .tag_resource(&resource_arn, &[tag("aws:sim2real", "nope")])
        .await;
    let too_long = ses
        .tag_resource(
            &resource_arn,
            &[tag(&"k".repeat(MAX_TAG_KEY_CHARS + 1), "nope")],
        )
        .await;

    assert_ne!(
        reserved.error_code(),
        too_long.error_code(),
        "the reserved-prefix and the past-the-bound refusals both answered {:?}",
        reserved.error_code()
    );
    assert_ne!(
        reserved.message(),
        too_long.message(),
        "the reserved-prefix and the past-the-bound refusals both answered the message {:?}",
        reserved.message()
    );
}

/// One accepted row: a batch that sits inside every bound and breaks no restriction.
struct AcceptedRow {
    case: &'static str,
    tags: Vec<(String, String)>,
}

#[tokio::test]
async fn a_tag_inside_every_bound_is_stored_and_read_back() {
    let rows = vec![
        AcceptedRow {
            case: "a key at exactly the maximum the shapes state",
            tags: vec![tag(&"k".repeat(MAX_TAG_KEY_CHARS), "prod")],
        },
        AcceptedRow {
            case: "a key at exactly the minimum the shapes state",
            tags: vec![tag("k", "prod")],
        },
        AcceptedRow {
            case: "a value at exactly the maximum the shapes state",
            tags: vec![tag("env", &"v".repeat(MAX_TAG_VALUE_CHARS))],
        },
        AcceptedRow {
            case: "a value of no characters, the minimum the shapes state",
            tags: vec![tag("retired", "")],
        },
        AcceptedRow {
            // The restriction is on the PREFIX, so the same text anywhere else in the key
            // is a key a caller may define.
            case: "a key carrying the reserved text somewhere other than at its start",
            tags: vec![tag("team-aws:ops", "aws-ish")],
        },
    ];

    for row in rows {
        let ses = Ses::new();
        let resource_arn = held_configuration_set(&ses).await;

        let reply = ses.tag_resource(&resource_arn, &row.tags).await;
        assert_eq!(
            reply.status,
            200,
            "case {:?}: TagResource answered {} {:?} ({:?})",
            row.case,
            reply.status,
            reply.error_code(),
            reply.message()
        );

        let mut expected = baseline();
        expected.extend(row.tags.iter().cloned());
        let reported = ses.list_tags(&resource_arn).await.tags("Tags");
        assert_eq!(
            reported, expected,
            "case {:?}: expected the configuration set to carry {expected:?}, got {reported:?}",
            row.case
        );
    }
}
