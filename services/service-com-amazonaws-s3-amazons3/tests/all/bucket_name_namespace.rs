// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the bucket NAMESPACE a general purpose bucket name lives in.
//!
//! The rule, from `CreateBucket`'s own documentation: "General purpose buckets exist in a
//! global namespace, which means that each bucket name must be unique across all Amazon
//! Web Services accounts in all the Amazon Web Services Regions within a partition." So
//! there is no Region left in which a name an account already holds can be minted a second
//! time: such a create is refused with `BucketAlreadyOwnedByYou` (HTTP 409 per the model)
//! and the existing bucket keeps the Region it was created in.
//!
//! The ONE documented exception is a re-create of the bucket as it stands: "For legacy
//! compatibility, if you re-create an existing bucket that you already own in the North
//! Virginia Region, Amazon S3 returns 200 OK"
//! (`smithy_query(query="shape",shape="BucketAlreadyOwnedByYou")`) — the Region the create
//! asks for has to be the one the bucket already lives in, and that Region has to be
//! `us-east-1`.
//!
//! The table below spans both sides of that split along what the rule generalises over:
//! which Region the bucket was created in, which Region the second create asks for, and
//! whether the two are the same. Every constraint value is one of
//! `BucketLocationConstraint`'s enum values
//! (`smithy_query(query="shape",shape="BucketLocationConstraint")`), where `us-east-1` is
//! deliberately absent — an absent constraint is the only way to ask for it — and `EU` is
//! the legacy alias for `eu-west-1`.
//!
//! The global namespace has one SUBDIVISION, which the tables from
//! [`an_account_regional_name_needs_the_namespace_header_and_the_callers_own_account`] on
//! pin: "when you create a general purpose bucket, you can choose to create a bucket in the
//! shared global namespace or you can choose to create a bucket in your account regional
//! namespace. Your account regional namespace is a subdivision of the global namespace that
//! only your account can create buckets in. … These buckets consist of a bucket name prefix
//! that you create, and a suffix that contains your 12-digit Amazon Web Services Account ID,
//! the Amazon Web Services Region code, and ends with `-an`. Bucket names must follow the
//! format `bucket-name-prefix-accountId-region-an`"
//! (`smithy_query(query="doc",shape="CreateBucketRequest",member="BucketNamespace")`).
//!
//! So a requested name of that form addresses ONE account's namespace, and the create is
//! served only when the request declares the namespace in the `x-amz-bucket-namespace` header
//! that member is bound to AND the account id the name states is the caller's own:
//!
//! * no header at all is `MissingNamespaceHeader` (HTTP 400), naming the header the caller
//!   must add in the `<Header>` member of the error document;
//! * an account id that is not the caller's is `InvalidBucketNamespace` (HTTP 400), naming
//!   the requested bucket name in the `<BucketNamespace>` member and both account ids in the
//!   message;
//! * a served one reports the bucket in PATH form in `Location` and the bucket's own ARN in
//!   `x-amz-bucket-arn` (`CreateBucketOutput$Location` / `$BucketArn`).
//!
//! Either refusal records NOTHING, so the name stays free for the request that is formed
//! right, and a name OUTSIDE that form is an ordinary global bucket name, which needs no
//! header at all.

use super::bucket_lifecycle::{create_bucket, head_bucket, router};
use super::bucket_location::{get_bucket_location, root_element};

/// One row: the case name, the constraint the FIRST create carried, the constraint the
/// SECOND create carried, the status the second create must answer, the error code it must
/// carry (none for the legacy 200), and the Region the bucket must still be in afterwards.
type ReuseRow<'a> = (
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    u16,
    Option<&'a str>,
    &'a str,
);

#[tokio::test]
async fn a_name_an_account_holds_cannot_be_minted_in_another_region() {
    let router = router();

    let rows: [ReuseRow<'_>; 7] = [
        (
            "re-creating a us-east-1 bucket in us-east-1 is the documented legacy 200",
            None,
            None,
            200,
            None,
            "us-east-1",
        ),
        (
            "a us-east-1 bucket cannot be re-minted in another Region",
            None,
            Some("us-west-2"),
            409,
            Some("BucketAlreadyOwnedByYou"),
            "us-east-1",
        ),
        (
            "a bucket elsewhere cannot be re-minted in us-east-1 either",
            Some("us-west-2"),
            None,
            409,
            Some("BucketAlreadyOwnedByYou"),
            "us-west-2",
        ),
        (
            "the legacy EU alias bucket cannot be re-minted in a Region the issue never \
             cited",
            Some("EU"),
            Some("ap-southeast-7"),
            409,
            Some("BucketAlreadyOwnedByYou"),
            "eu-west-1",
        ),
        (
            "neither Region being us-east-1 changes nothing: still refused, still where it \
             was",
            Some("ap-northeast-3"),
            Some("us-gov-east-1"),
            409,
            Some("BucketAlreadyOwnedByYou"),
            "ap-northeast-3",
        ),
        (
            "asking for the very Region the bucket is in is refused when it is not \
             us-east-1",
            Some("us-west-1"),
            Some("us-west-1"),
            409,
            Some("BucketAlreadyOwnedByYou"),
            "us-west-1",
        ),
        (
            "the EU alias and eu-west-1 name one Region, and it is not us-east-1",
            Some("EU"),
            Some("eu-west-1"),
            409,
            Some("BucketAlreadyOwnedByYou"),
            "eu-west-1",
        ),
    ];

    for (index, (case, first_constraint, second_constraint, expected_status, expected_code, expected_region)) in
        rows.iter().enumerate()
    {
        let bucket = format!("namespace-bucket-{index}");

        let first = create_bucket(&router, &bucket, *first_constraint, None).await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: the first create of {bucket:?} must succeed, got {} with body \
             {:?}",
            first.status,
            String::from_utf8_lossy(&first.body)
        );

        let second = create_bucket(&router, &bucket, *second_constraint, None).await;
        assert_eq!(
            second.status, *expected_status,
            "case {case:?}: expected HTTP {expected_status} from the second create, got {} \
             with body {:?}",
            second.status,
            String::from_utf8_lossy(&second.body)
        );
        match expected_code {
            Some(code) => assert_eq!(
                second.error_code(),
                *code,
                "case {case:?}: expected error code {code:?}, got {:?}",
                second.error_code()
            ),
            None => assert_eq!(
                second.header("location").as_deref(),
                Some(format!("/{bucket}").as_str()),
                "case {case:?}: the legacy 200 still reports the bucket's Location, got {:?}",
                second.header("location")
            ),
        }

        // The bucket stayed where it was: both readers of a bucket's Region still report
        // the Region of the FIRST create, so nothing moved the bucket and nothing minted a
        // second one somewhere else.
        let head = head_bucket(&router, &bucket).await;
        assert_eq!(
            head.status, 200,
            "case {case:?}: expected HTTP 200 from HeadBucket after the second create, got \
             {}",
            head.status
        );
        assert_eq!(
            head.header("x-amz-bucket-region").as_deref(),
            Some(*expected_region),
            "case {case:?}: expected HeadBucket to still report {expected_region:?}, got \
             {:?}",
            head.header("x-amz-bucket-region")
        );

        let location = get_bucket_location(&router, &bucket).await;
        assert_eq!(
            location.status, 200,
            "case {case:?}: expected HTTP 200 from GetBucketLocation, got {} with body {:?}",
            location.status,
            String::from_utf8_lossy(&location.body)
        );
        // "Buckets in Region us-east-1 have a LocationConstraint of null"
        // (`GetBucketLocationOutput$LocationConstraint`), so that one Region reports no
        // text at all and every other reports its own code.
        let (_, reported) = root_element(&location);
        let expected_constraint = if *expected_region == "us-east-1" {
            ""
        } else {
            expected_region
        };
        assert_eq!(
            reported, expected_constraint,
            "case {case:?}: expected the location document to report \
             {expected_constraint:?}, got {reported:?} in body {:?}",
            String::from_utf8_lossy(&location.body)
        );
    }
}

#[tokio::test]
async fn the_refused_reuse_leaves_the_bucket_and_its_objects_alone() {
    // A refused create writes NOTHING: the bucket it did not re-mint still holds the
    // object that was put in it, so the refusal cannot be a delete-and-recreate.
    let router = router();
    let created = create_bucket(&router, "namespace-objects", Some("us-west-2"), None).await;
    assert_eq!(
        created.status, 200,
        "the first create must succeed, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    let put = super::bucket_lifecycle::send(
        &router,
        "PUT",
        "/namespace-objects/keep.txt",
        &[],
        "payload",
    )
    .await;
    assert_eq!(
        put.status, 200,
        "the object write must succeed, got {} with body {:?}",
        put.status,
        String::from_utf8_lossy(&put.body)
    );

    for (case, constraint) in [
        ("another Region", Some("eu-central-1")),
        ("us-east-1 by omission", None),
        ("the Region it is already in", Some("us-west-2")),
    ] {
        let reused = create_bucket(&router, "namespace-objects", constraint, None).await;
        assert_eq!(
            reused.status, 409,
            "case {case:?}: expected HTTP 409 from the reuse, got {} with body {:?}",
            reused.status,
            String::from_utf8_lossy(&reused.body)
        );
        assert_eq!(
            reused.error_code(),
            "BucketAlreadyOwnedByYou",
            "case {case:?}: expected error code BucketAlreadyOwnedByYou, got {:?}",
            reused.error_code()
        );

        let got = super::bucket_lifecycle::send(
            &router,
            "GET",
            "/namespace-objects/keep.txt",
            &[],
            "",
        )
        .await;
        assert_eq!(
            got.status, 200,
            "case {case:?}: the object must survive the refused reuse, got {} with body {:?}",
            got.status,
            String::from_utf8_lossy(&got.body)
        );
        assert_eq!(
            String::from_utf8_lossy(&got.body),
            "payload",
            "case {case:?}: expected the stored bytes back, got {:?}",
            String::from_utf8_lossy(&got.body)
        );
    }
}

/// The account every caller of this emulator speaks for, which is the account an account
/// regional bucket name has to state to be the caller's own namespace.
fn caller_account() -> &'static str {
    engine::emulator_account_id()
}

/// The fixed sentence S3 refuses a create of an account regional name that declares no
/// namespace with.
const MISSING_NAMESPACE_HEADER_MESSAGE: &str =
    "The requested bucket is an account-regional namespace bucket, but your request is \
     missing the required x-amz-bucket-namespace header.";

/// The sentence S3 refuses a create naming ANOTHER account's regional namespace with: fixed
/// text around the two account ids it is about — the one the requested name states and the
/// caller's own.
fn invalid_bucket_namespace_message(named_account: &str) -> String {
    format!(
        "The requested bucket is an account-regional namespace bucket, but the requested AWS \
         Account ID '{named_account}' does not match the caller's AWS Account ID '{}'. \
         Specify the caller's AWS Account ID in the bucket name.",
        caller_account()
    )
}

/// One row of the namespace refusal table: the case, the requested bucket name, the namespace
/// the request declares (when any), the error code it must be refused with, the element and
/// value the error document must name the refusal by, and the sentence it must carry.
type NamespaceRefusalRow<'a> = (
    &'a str,
    String,
    Option<&'a str>,
    &'a str,
    (&'a str, String),
    String,
);

#[tokio::test]
async fn an_account_regional_name_needs_the_namespace_header_and_the_callers_own_account() {
    let router = router();
    let caller = caller_account();
    // The header the missing-namespace refusal names, which is the one
    // `CreateBucketRequest$BucketNamespace` is bound to.
    let header = ("Header", "x-amz-bucket-namespace".to_string());

    // The rows differ along the account id the name states (the caller's, one digit off it,
    // and two other accounts), the Region code it carries (four Regions across three
    // partitions), the prefix before it (including one carrying twelve digits of its own) and
    // the namespace the header declares (absent, and both values of
    // `smithy_query(query="shape",shape="BucketNamespace")`).
    let rows: [NamespaceRefusalRow<'_>; 8] = [
        (
            "the caller's own account regional name, with no namespace header",
            format!("probe-{caller}-us-west-2-an"),
            None,
            "MissingNamespaceHeader",
            header.clone(),
            MISSING_NAMESPACE_HEADER_MESSAGE.to_string(),
        ),
        (
            "another account's regional name with no header is refused for the header first",
            "probe-999999999999-eu-central-1-an".to_string(),
            None,
            "MissingNamespaceHeader",
            header.clone(),
            MISSING_NAMESPACE_HEADER_MESSAGE.to_string(),
        ),
        (
            "a Region in another partition, still missing the header",
            "probe-123456789012-us-gov-east-1-an".to_string(),
            None,
            "MissingNamespaceHeader",
            header.clone(),
            MISSING_NAMESPACE_HEADER_MESSAGE.to_string(),
        ),
        (
            "another account's namespace, declared account-regional",
            "probe-999999999999-us-west-2-an".to_string(),
            Some("account-regional"),
            "InvalidBucketNamespace",
            (
                "BucketNamespace",
                "probe-999999999999-us-west-2-an".to_string(),
            ),
            invalid_bucket_namespace_message("999999999999"),
        ),
        (
            "an account id one digit off the caller's is another account",
            "probe-000000000001-ap-southeast-7-an".to_string(),
            Some("account-regional"),
            "InvalidBucketNamespace",
            (
                "BucketNamespace",
                "probe-000000000001-ap-southeast-7-an".to_string(),
            ),
            invalid_bucket_namespace_message("000000000001"),
        ),
        (
            "a third account, in a gov-cloud Region",
            "probe-123456789012-us-gov-east-1-an".to_string(),
            Some("account-regional"),
            "InvalidBucketNamespace",
            (
                "BucketNamespace",
                "probe-123456789012-us-gov-east-1-an".to_string(),
            ),
            invalid_bucket_namespace_message("123456789012"),
        ),
        (
            "declaring the global namespace does not make the name the caller's: the NAME \
             states whose namespace it is",
            "probe-999999999999-us-east-1-an".to_string(),
            Some("global"),
            "InvalidBucketNamespace",
            (
                "BucketNamespace",
                "probe-999999999999-us-east-1-an".to_string(),
            ),
            invalid_bucket_namespace_message("999999999999"),
        ),
        (
            "a prefix carrying twelve digits of its own: the id the Region code follows is \
             the namespace's",
            format!("probe-{caller}-999999999999-us-west-2-an"),
            Some("account-regional"),
            "InvalidBucketNamespace",
            (
                "BucketNamespace",
                format!("probe-{caller}-999999999999-us-west-2-an"),
            ),
            invalid_bucket_namespace_message("999999999999"),
        ),
    ];

    for (case, bucket, namespace, expected_code, (element, expected_value), expected_message) in
        rows
    {
        let refused = create_bucket(&router, &bucket, None, namespace).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 from CreateBucket on {bucket:?}, got {} with \
             body {:?}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            expected_code,
            "case {case:?}: expected error code {expected_code:?}, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {case:?}: expected the sentence {expected_message:?}, got {:?}",
            refused.error_message()
        );
        assert_eq!(
            refused.element(element).as_deref(),
            Some(expected_value.as_str()),
            "case {case:?}: expected <{element}>{expected_value}</{element}> in the error \
             document, got {:?} in body {:?}",
            refused.element(element),
            String::from_utf8_lossy(&refused.body)
        );

        // The refused create recorded nothing: the bucket it did not mint is not there, so
        // an existence probe answers the not-found it would have answered before the call.
        let head = head_bucket(&router, &bucket).await;
        assert_eq!(
            head.status, 404,
            "case {case:?}: expected HTTP 404 from HeadBucket on the bucket that was not \
             created, got {}",
            head.status
        );
    }
}

/// One row of the served table: the case, the requested name, the namespace declared, the
/// location constraint the create carries, and the Region the bucket must read back with.
type NamespaceServedRow<'a> = (&'a str, String, Option<&'a str>, Option<&'a str>, &'a str);

#[tokio::test]
async fn the_account_regional_create_reports_the_bucket_path_and_its_arn() {
    let router = router();
    let caller = caller_account();

    let rows: [NamespaceServedRow<'_>; 5] = [
        (
            "the caller's own namespace, declared, in us-west-2",
            format!("probe-{caller}-us-west-2-an"),
            Some("account-regional"),
            Some("us-west-2"),
            "us-west-2",
        ),
        (
            "the same in a Region in another partition",
            format!("probe-{caller}-us-gov-east-1-an"),
            Some("account-regional"),
            Some("us-gov-east-1"),
            "us-gov-east-1",
        ),
        (
            "no location constraint: the Region the unspecified constraint stands for",
            format!("probe-{caller}-us-east-1-an"),
            Some("account-regional"),
            None,
            "us-east-1",
        ),
        (
            "a prefix carrying another account's twelve digits, the caller's id before the \
             Region code",
            format!("probe-999999999999-{caller}-ap-southeast-7-an"),
            Some("account-regional"),
            Some("ap-southeast-7"),
            "ap-southeast-7",
        ),
        (
            "declaring the global namespace over the caller's own name is served too",
            format!("probe-global-{caller}-us-east-1-an"),
            Some("global"),
            None,
            "us-east-1",
        ),
    ];

    for (case, bucket, namespace, constraint, expected_region) in rows {
        let created = create_bucket(&router, &bucket, constraint, namespace).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateBucket on {bucket:?}, got {} with \
             body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        // "A forward slash followed by the name of the bucket for all account regional
        // namespace buckets and all global general purpose buckets created in us-east-1"
        // (`CreateBucketOutput$Location`).
        let expected_location = format!("/{bucket}");
        assert_eq!(
            created.header("location").as_deref(),
            Some(expected_location.as_str()),
            "case {case:?}: expected Location {expected_location:?}, got {:?}",
            created.header("location")
        );
        let expected_arn = format!("arn:aws:s3:::{bucket}");
        assert_eq!(
            created.header("x-amz-bucket-arn").as_deref(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the new bucket's ARN {expected_arn:?}, got {:?}",
            created.header("x-amz-bucket-arn")
        );

        // The bucket is really there, in the Region the create asked for.
        let head = head_bucket(&router, &bucket).await;
        assert_eq!(
            head.status, 200,
            "case {case:?}: expected HTTP 200 from HeadBucket on the bucket just created, \
             got {}",
            head.status
        );
        assert_eq!(
            head.header("x-amz-bucket-region").as_deref(),
            Some(expected_region),
            "case {case:?}: expected HeadBucket to report Region {expected_region:?}, got \
             {:?}",
            head.header("x-amz-bucket-region")
        );
    }
}

#[tokio::test]
async fn a_name_outside_the_account_regional_form_needs_no_namespace_header() {
    let router = router();
    let caller = caller_account();

    // Each name is one way of NOT being `prefix-accountId-region-an`, so each is an ordinary
    // global bucket name and the create is served with no header at all.
    let rows: [(&str, String); 5] = [
        (
            "an account id and a Region code, but not the account regional suffix",
            format!("probe-{caller}-us-west-2"),
        ),
        (
            "the suffix with no account id anywhere in the name",
            "probe-logs-an".to_string(),
        ),
        (
            "the caller's account id immediately before the suffix, with no Region code",
            format!("probe-{caller}-an"),
        ),
        (
            "another account's id immediately before the suffix is a global name too",
            "probe-999999999999-an".to_string(),
        ),
        (
            "the suffix's letters inside a word rather than as a segment",
            format!("probe-{caller}-us-west-2-plan"),
        ),
    ];

    for (case, bucket) in rows {
        let created = create_bucket(&router, &bucket, None, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateBucket on the global name \
             {bucket:?}, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let head = head_bucket(&router, &bucket).await;
        assert_eq!(
            head.status, 200,
            "case {case:?}: expected HTTP 200 from HeadBucket on {bucket:?}, got {}",
            head.status
        );
    }
}

#[tokio::test]
async fn a_refused_account_regional_create_leaves_the_name_free() {
    // The hazard the refusals exist for: a half-specified create must not consume the name,
    // so the correctly formed request that follows gets the bucket.
    let router = router();
    let bucket = format!("probe-free-{}-us-west-2-an", caller_account());

    let refused = create_bucket(&router, &bucket, Some("us-west-2"), None).await;
    assert_eq!(
        refused.status, 400,
        "the create with no namespace header must be refused, got {} with body {:?}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );

    let created = create_bucket(&router, &bucket, Some("us-west-2"), Some("account-regional")).await;
    assert_eq!(
        created.status, 200,
        "the correctly formed create of the same name must succeed, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    assert_eq!(
        created.header("location").as_deref(),
        Some(format!("/{bucket}").as_str()),
        "the served create reports the bucket path, got {:?}",
        created.header("location")
    );
    assert_eq!(
        created.header("x-amz-bucket-arn").as_deref(),
        Some(format!("arn:aws:s3:::{bucket}").as_str()),
        "the served create reports the new bucket's ARN, got {:?}",
        created.header("x-amz-bucket-arn")
    );

    let head = head_bucket(&router, &bucket).await;
    assert_eq!(
        head.status, 200,
        "the bucket must read back after the create that was served, got {}",
        head.status
    );
}
