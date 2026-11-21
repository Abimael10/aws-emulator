// SPDX-License-Identifier: CC0-1.0

//! WHICH `ResourceArn` forms the generic tagging surface can address a resource in,
//! driven end to end over HTTP through all three of its operations.
//!
//! The rule under test: `TagResource`, `ListTagsForResource` and `UntagResource` address
//! the resource in their greedy `{ResourceArn+}` label ALONE, and a resource type is
//! addressed only in the ARN namespace that publishes it — a general purpose bucket as the
//! whole resource field of an `s3` ARN with empty Region and account fields, a directory
//! bucket as the `bucket/<name>` form of an `s3express` ARN, an access point in either
//! namespace, and a Storage Lens group or an S3 Access Grants resource in `s3`. A label in
//! any other form leaves the path addressing nothing, so it is not a path this service can
//! parse: it is refused `400 InvalidURI` with the fixed sentence and the path addressing
//! the resource the caller named — relative to the API version segment, so `tags/` and that
//! ARN, with the wire's own escaping resolved — echoed in the `URI` member, whether or not
//! a resource of some other kind answers to that name.
//! An ARN in a form the surface DOES address names a resource, so it is served, or refused
//! as that resource being missing when no store holds it.
//!
//! The rows span what the rule generalises over: the ARN namespace, the resource type, the
//! typed and untyped resource forms, whether the Region and account fields are filled, a
//! value that is no ARN at all, and the wire escaping of the label. One row is the
//! regional bucket-style form over a bucket the bucket store really HOLDS, because the
//! defect this suite pins was that such a request was served a tag set.

use crate::access_point_lifecycle::{Emulator, Reply, ACCOUNT, REGION, XMLNS};
use crate::storage_lens_configuration::{reported_tags, tag, tags_xml};

/// The code, status and fixed sentence this service answers a path it cannot parse with.
/// Spelled out here rather than read from the implementation's own constants, so a change
/// to them fails this suite.
const UNPARSABLE_STATUS: u16 = 400;
const UNPARSABLE_CODE: &str = "InvalidURI";
const UNPARSABLE_MESSAGE: &str = "Couldn't parse the specified URI.";

/// The code and the fixed sentence this surface answers for an ARN it CAN address that
/// names a resource no store holds.
const NO_SUCH_RESOURCE_STATUS: u16 = 404;
const NO_SUCH_RESOURCE_CODE: &str = "NoSuchResource";

/// The bucket the rows that address a bucket name.
const BUCKET: &str = "reports";

/// What this surface does with a row's `ResourceArn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The form addresses a resource, so the call reaches it: there is no such resource in
    /// a fresh store, which is the refusal that says so.
    Addresses,
    /// The form addresses no resource at all, so the path cannot be parsed.
    Unparsable,
}

/// Send `method` to the tagging route for `label`, with the body that method's operation
/// carries.
async fn tagging_call(emulator: &Emulator, method: &str, label: &str) -> Reply {
    let path = match method {
        "DELETE" => format!("/v20180820/tags/{label}?tagKeys=Team"),
        _ => format!("/v20180820/tags/{label}"),
    };
    let body = match method {
        "POST" => format!(
            "<TagResourceRequest xmlns=\"{XMLNS}\">{}</TagResourceRequest>",
            tags_xml(Some(&[tag("Team", "platform")]))
        ),
        _ => String::new(),
    };
    emulator.call(method, &path, ACCOUNT, &body).await
}

/// The three operations of the surface, by the method each is bound as
/// (`smithy_query(query="operation")`: `GET`, `POST` and `DELETE` on
/// `/v20180820/tags/{ResourceArn+}`).
const OPERATIONS: [(&str, &str); 3] = [
    ("ListTagsForResource", "GET"),
    ("TagResource", "POST"),
    ("UntagResource", "DELETE"),
];

/// Assert `reply` is the unparsable-path refusal echoing the resource `arn` addressed
/// under the tagging path.
fn assert_unparsable(case: &str, operation: &str, arn: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (UNPARSABLE_STATUS, UNPARSABLE_CODE),
        "case {case:?} through {operation}: expected {UNPARSABLE_STATUS} {UNPARSABLE_CODE}, \
         got {} with body {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.text("Message").as_deref(),
        Some(UNPARSABLE_MESSAGE),
        "case {case:?} through {operation}: expected the fixed sentence, got body {}",
        reply.body
    );
    assert_eq!(
        reply.text("URI").as_deref(),
        Some(format!("tags/{arn}").as_str()),
        "case {case:?} through {operation}: expected the refusal to echo the path addressing \
         the resource the caller named, got body {}",
        reply.body
    );
}

#[tokio::test]
async fn an_arn_form_this_surface_addresses_no_resource_in_is_a_path_it_cannot_parse() {
    let emulator = Emulator::start();
    // The bucket store holds the bucket every bucket row names, so a refusal below is
    // about the FORM of the ARN rather than about an empty store.
    emulator.provision_bucket("the bucket the rows address", BUCKET);
    let account = ACCOUNT;

    let rows: Vec<(&str, String, Addressing)> = vec![
        // The forms AWS publishes for the resources this surface tags.
        (
            "a general purpose bucket the bucket store does not hold",
            "arn:aws:s3:::absent-bucket".to_string(),
            Addressing::Addresses,
        ),
        (
            "a directory bucket",
            format!("arn:aws:s3express:{REGION}:{account}:bucket/db--usw2-az1--x-s3"),
            Addressing::Addresses,
        ),
        (
            "an access point for a general purpose bucket",
            format!("arn:aws:s3:{REGION}:{account}:accesspoint/reports-ap"),
            Addressing::Addresses,
        ),
        (
            "an access point for a directory bucket",
            format!("arn:aws:s3express:{REGION}:{account}:accesspoint/reports-ap"),
            Addressing::Addresses,
        ),
        (
            "a Storage Lens group",
            format!("arn:aws:s3:{REGION}:{account}:storage-lens-group/dev"),
            Addressing::Addresses,
        ),
        (
            "an S3 Access Grants instance",
            format!("arn:aws:s3:{REGION}:{account}:access-grants/default"),
            Addressing::Addresses,
        ),
        (
            "a registered Access Grants location",
            format!("arn:aws:s3:{REGION}:{account}:access-grants/default/location/loc"),
            Addressing::Addresses,
        ),
        // The forms it addresses no resource in.
        (
            // The recorded defect: the typed bucket form under the plain `s3`
            // namespace, over a bucket the bucket store HOLDS, which used to be
            // served a tag set.
            "the regional bucket-style form of a bucket that is there",
            format!("arn:aws:s3:{REGION}:{account}:bucket/{BUCKET}"),
            Addressing::Unparsable,
        ),
        (
            "the regional bucket-style form of a bucket that is not",
            format!("arn:aws:s3:{REGION}:{account}:bucket/absent"),
            Addressing::Unparsable,
        ),
        (
            "a general purpose bucket ARN carrying a Region and an account",
            format!("arn:aws:s3:{REGION}:{account}:{BUCKET}"),
            Addressing::Unparsable,
        ),
        (
            "a Storage Lens group in the S3 Express namespace",
            format!("arn:aws:s3express:{REGION}:{account}:storage-lens-group/dev"),
            Addressing::Unparsable,
        ),
        (
            "an Access Grants instance in the S3 Express namespace",
            format!("arn:aws:s3express:{REGION}:{account}:access-grants/default"),
            Addressing::Unparsable,
        ),
        (
            "a Storage Lens configuration, a resource type this surface does not tag",
            format!("arn:aws:s3:{REGION}:{account}:storage-lens/default-account-dashboard"),
            Addressing::Unparsable,
        ),
        (
            "a typed resource form carrying no id",
            format!("arn:aws:s3express:{REGION}:{account}:bucket/"),
            Addressing::Unparsable,
        ),
        (
            "another vendor's resource ARN",
            format!("arn:aws:sqs:{REGION}:{account}:queue"),
            Addressing::Unparsable,
        ),
        (
            "a value that is no ARN at all",
            BUCKET.to_string(),
            Addressing::Unparsable,
        ),
    ];

    for (case, label, addressing) in &rows {
        for (operation, method) in OPERATIONS {
            let reply = tagging_call(&emulator, method, label).await;
            match addressing {
                Addressing::Unparsable => assert_unparsable(case, operation, label, &reply),
                Addressing::Addresses => assert_eq!(
                    (reply.status, reply.code().as_str()),
                    (NO_SUCH_RESOURCE_STATUS, NO_SUCH_RESOURCE_CODE),
                    "case {case:?} through {operation}: expected the addressed resource to be \
                     resolved and found missing, got {} with body {}",
                    reply.status,
                    reply.body
                ),
            }
        }
    }

    // The bucket the rows named still reads its own tag set through the form that DOES
    // address it, so none of those refusals wrote or read anything through another name.
    let served = tagging_call(&emulator, "GET", &format!("arn:aws:s3:::{BUCKET}")).await;
    assert_eq!(
        (served.status, reported_tags(&served.body)),
        (200, Vec::new()),
        "expected the bucket's own tag set to be served through the form that addresses it, \
         got {} with body {}",
        served.status,
        served.body
    );
}

#[tokio::test]
async fn the_refusal_echoes_the_resource_the_label_addressed_however_it_was_escaped() {
    let emulator = Emulator::start();
    emulator.provision_bucket("the bucket the labels address", BUCKET);
    let unparsable = format!("arn:aws:s3:{REGION}:{ACCOUNT}:bucket/{BUCKET}");

    // The same unaddressable ARN, written as the wire allows it to be written: plainly, and
    // with the ARN's own separators percent-escaped the way an SDK escapes a path label.
    // The escapes are how the path travelled rather than part of the resource addressed, so
    // the path cannot be parsed either way and the member echoes the ARN itself.
    let rows = [
        ("the label written plainly", unparsable.clone()),
        (
            "the label with its colons escaped",
            unparsable.replace(':', "%3A"),
        ),
        (
            "the label with every separator escaped",
            unparsable.replace(':', "%3A").replace('/', "%2F"),
        ),
    ];

    for (case, label) in rows {
        for (operation, method) in OPERATIONS {
            let reply = tagging_call(&emulator, method, &label).await;
            assert_unparsable(case, operation, &unparsable, &reply);
        }
    }
}
