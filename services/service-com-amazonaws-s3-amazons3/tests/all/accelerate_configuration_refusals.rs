// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the refusals of the Transfer Acceleration subresource: what S3
//! answers a bucket the feature does not exist for, and what it answers a bucket whose NAME
//! the feature cannot be used with.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (status, error code, message, recorded state) rather than any handler's
//! internals.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! 1. Transfer Acceleration does not exist for a DIRECTORY bucket — S3 Express One Zone
//!    serves a narrower API, and this operation carries the model's "this operation is not
//!    supported for directory buckets" note — so a put against one is refused for the
//!    DOCUMENT it submits (a directory bucket's endpoint parses one bucket-configuration
//!    document, `CreateBucketConfiguration`) and the read of the same subresource is refused
//!    for the METHOD, which that endpoint serves at no such address. Neither records
//!    anything.
//! 2. "The name of the bucket used for Transfer Acceleration must be DNS-compliant and must
//!    not contain periods (".")" (this operation's own documentation), so a put against a
//!    general purpose bucket whose name carries one is refused `InvalidRequest` with the
//!    sentence that names the FEATURE and the rule — not the generic sentence that code
//!    states for any invalid parameter, which names no rule a caller could act on.
//!
//! The rows differ along what the rules generalise over: the state the put asks for (each
//! value `BucketAccelerateStatus` publishes), WHERE the period sits in the name and how many
//! there are, and the kind of bucket addressed (a directory bucket in an Availability Zone, a
//! general purpose bucket with a period, and one with none as the control).

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::object_and_tagging_roundtrip::text;

/// The two states a put may ask for: "the Transfer Acceleration state of a bucket can be set
/// to one of the following two values: Enabled … Suspended" (this operation's own
/// documentation), which is the whole of `BucketAccelerateStatus`.
const STATUSES: [&str; 2] = ["Enabled", "Suspended"];

/// The code S3 answers "a parameter or header in your request isn't valid" with, at HTTP 400.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code S3 answers a request whose shape it cannot use with, at HTTP 400.
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The code S3 answers a method the addressed resource does not serve with, and its status
/// (Amazon S3 *Error responses*, list of error codes: "MethodNotAllowed — The specified method
/// is not allowed against this resource", HTTP 405).
const METHOD_NOT_ALLOWED: &str = "MethodNotAllowed";
/// See [`METHOD_NOT_ALLOWED`].
const METHOD_NOT_ALLOWED_STATUS: u16 = 405;
/// See [`METHOD_NOT_ALLOWED`] — the fixed sentence that code publishes.
const METHOD_NOT_ALLOWED_MESSAGE: &str = "The specified method is not allowed against this resource.";

/// The sentence S3 refuses a put against a bucket whose name carries a period with. Written
/// out here from the published wording rather than imported, so a wording regression fails
/// this suite.
const PERIODS_IN_NAME_MESSAGE: &str =
    "S3 Transfer Acceleration is not supported for buckets with periods (.) in their names";

/// The sentence a directory bucket's endpoint refuses a bucket-configuration document it does
/// not parse with: the document the request carried, beside the one that endpoint does parse.
const NOT_A_CREATE_BUCKET_CONFIGURATION_MESSAGE: &str =
    "AccelerateConfiguration is not valid, expected CreateBucketConfiguration";

/// The generic sentence `InvalidRequest` states for any invalid parameter, which neither
/// refusal here may carry: it names no rule, so a caller cannot tell which one it broke.
const GENERIC_INVALID_REQUEST_MESSAGE: &str =
    "A parameter or header in your request isn't valid. For details, see the description of \
     this API operation.";

/// The `AccelerateConfiguration` document asking for `status`.
fn payload(status: &str) -> String {
    format!(
        "<AccelerateConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Status>{status}</Status></AccelerateConfiguration>"
    )
}

/// `PUT /<bucket>?accelerate` — `PutBucketAccelerateConfiguration`.
async fn put_accelerate(router: &MultiServiceRouter, name: &str, status: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{name}?accelerate"),
        &[],
        &payload(status),
    )
    .await
}

/// `GET /<bucket>?accelerate` — `GetBucketAccelerateConfiguration`.
async fn get_accelerate(router: &MultiServiceRouter, name: &str) -> Answer {
    send(router, "GET", &format!("/{name}?accelerate"), &[], "").await
}

/// RULE: a put against a general purpose bucket whose name carries a period is refused
/// `InvalidRequest` naming the feature and the rule the name broke, and records nothing.
#[tokio::test]
async fn a_dotted_bucket_name_is_refused_with_the_sentence_that_names_the_rule() {
    let router = router();
    // Names differing in WHERE the period sits and how many there are, beside one with none
    // at all as the control. Each is a name S3 holds a bucket under — the published naming
    // rules allow a period between two letters — so the refusal is about the FEATURE's own
    // narrower rule and not about addressing.
    let dotted: [&str; 3] = ["ledger.archive", "ledger.archive.cold", "l.edger-archive"];
    for name in dotted {
        let created = create_bucket(&router, name, None, None).await;
        assert_eq!(
            created.status, 200,
            "the bucket {name:?} must be created first, got {} with body {:?}",
            created.status,
            text(&created)
        );
    }

    for name in dotted {
        for status in STATUSES {
            let case = format!("{name:?} set to {status}");
            let answered = put_accelerate(&router, name, status).await;
            let body = text(&answered);
            assert_eq!(
                answered.status, 400,
                "case {case}: expected HTTP 400, got {} with body {body:?}",
                answered.status
            );
            assert_eq!(
                answered.error_code(),
                INVALID_REQUEST,
                "case {case}: expected the code {INVALID_REQUEST}, got {:?} in body {body:?}",
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                PERIODS_IN_NAME_MESSAGE,
                "case {case}: expected the sentence that names the rule rather than \
                 {GENERIC_INVALID_REQUEST_MESSAGE:?}, got {:?}",
                answered.error_message()
            );
        }

        // Nothing was recorded, and the read of the subresource is still served: only the put
        // is refused.
        let read = get_accelerate(&router, name).await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: the read must still be served, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.element("Status"),
            None,
            "case {name:?}: a refused put must record no state, got body {:?}",
            text(&read)
        );
    }

    // The control: the same put against a name with no period is served, so the refusals
    // above are about the period and not about the request.
    let plain = "ledger-archive";
    let created = create_bucket(&router, plain, None, None).await;
    assert_eq!(
        created.status, 200,
        "the control bucket must be created, got {} with body {:?}",
        created.status,
        text(&created)
    );
    for status in STATUSES {
        let answered = put_accelerate(&router, plain, status).await;
        assert_eq!(
            answered.status, 200,
            "case \"a name with no period set to {status}\": expected HTTP 200, got {} with body \
             {:?}",
            answered.status,
            text(&answered)
        );
        let read = get_accelerate(&router, plain).await;
        assert_eq!(
            read.element("Status").as_deref(),
            Some(status),
            "case \"a name with no period set to {status}\": expected the state to be recorded, \
             got body {:?}",
            text(&read)
        );
    }
}

/// RULE: Transfer Acceleration does not exist for a directory bucket — the put is refused for
/// the document it submits, the read for the method, and neither records anything.
#[tokio::test]
async fn a_directory_bucket_serves_no_transfer_acceleration() {
    let router = router();
    // Two zones, so the refusal is about the KIND of bucket rather than about one zone.
    let buckets: [(&str, &str); 2] = [
        ("accelerate-zonal--use1-az4--x-s3", "use1-az4"),
        ("accelerate-zonal--use1-az5--x-s3", "use1-az5"),
    ];
    for (name, zone) in buckets {
        directory_bucket(&router, name, zone).await;
    }

    for (name, _) in buckets {
        for status in STATUSES {
            let case = format!("{name:?} set to {status}");
            let answered = put_accelerate(&router, name, status).await;
            let body = text(&answered);
            assert_eq!(
                answered.status, 400,
                "case {case}: expected HTTP 400, got {} with body {body:?}",
                answered.status
            );
            assert_eq!(
                answered.error_code(),
                INVALID_ARGUMENT,
                "case {case}: expected the code {INVALID_ARGUMENT}, got {:?} in body {body:?}",
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                NOT_A_CREATE_BUCKET_CONFIGURATION_MESSAGE,
                "case {case}: expected the sentence naming the document, got {:?}",
                answered.error_message()
            );
        }

        // The READ of the same subresource is refused for the method that endpoint serves at
        // no such address, which is a different class of answer from the put's.
        let read = get_accelerate(&router, name).await;
        let body = text(&read);
        assert_eq!(
            read.status, METHOD_NOT_ALLOWED_STATUS,
            "case {name:?}: expected HTTP {METHOD_NOT_ALLOWED_STATUS} for the read, got {} with \
             body {body:?}",
            read.status
        );
        assert_eq!(
            read.error_code(),
            METHOD_NOT_ALLOWED,
            "case {name:?}: expected the code {METHOD_NOT_ALLOWED}, got {:?} in body {body:?}",
            read.error_code()
        );
        assert_eq!(
            read.error_message(),
            METHOD_NOT_ALLOWED_MESSAGE,
            "case {name:?}: expected the fixed sentence that code publishes, got {:?}",
            read.error_message()
        );
    }
}
