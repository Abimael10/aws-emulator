// SPDX-License-Identifier: CC0-1.0

//! Probes of the rules `PutBucketInventoryConfiguration` judges an inventory configuration
//! DOCUMENT by, and of the `id` argument that addresses it — every one driven as a real HTTP
//! request through the router, so what is pinned is the wire behaviour (status, code,
//! message) and what the bucket carries afterwards.
//!
//! Declared as `inventory_probe` — the name of the file it lives in, which this crate's
//! build script requires — in this crate's `tests/all.rs`, beside the other `i` suites.
//!
//! The rules under test, with their sources:
//!
//! * A document whose member values are not ones the schema this API publishes for an
//!   `InventoryConfiguration` admits is refused 400 [`MALFORMED_XML_CODE`] with that code's
//!   own fixed sentence: an `Id` outside the id rule every bucket configuration id shares
//!   ("the ID has a 64 character limit and can only contain letters, numbers, periods,
//!   dashes, and underscores"), an `InventoryFilter$Prefix` that states no prefix at all, a
//!   member naming a value outside its enum's published set, and a value the schema cannot
//!   READ as the member's declared type (a boolean outside the forms XML spells one in).
//! * `InventoryS3BucketDestination$Bucket` is "the Amazon Resource Name (ARN) of the bucket
//!   where inventory results will be published", and a bucket ARN carries no Region: a value
//!   that is not one is refused 400 [`INVALID_S3_DESTINATION_BUCKET_CODE`] with the sentence
//!   its own fault publishes ([`BucketArnFault::message`]).
//! * `SSEKMS$KeyId` is "the ID of the Key Management Service (KMS) symmetric encryption
//!   customer managed key to use for encrypting inventory reports", which S3 Inventory takes
//!   as that key's ARN: a value that is not one is refused 400
//!   [`INVALID_KMS_KEY_ID_CODE`]. The SSE-S3 alternative names no key at all and is taken.
//! * The `id` argument and the document's own `Id` name ONE configuration: two ids that are
//!   both stated and differ are refused 400 [`ID_MISMATCH_CODE`], and a request that states
//!   no id for the document it carries — no `id` argument at all, or an empty one — addresses
//!   nothing and is refused 500 [`INTERNAL_ERROR_CODE`].
//! * Every refusal records NOTHING: the configuration is absent from
//!   `ListBucketInventoryConfigurations`, which reports exactly the documents that were
//!   taken, and a document the rules admit round-trips through
//!   `GetBucketInventoryConfiguration` under the id that addresses it.

use service_com_amazonaws_s3_amazons3::ops::put_bucket_inventory_configuration::PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS;
use service_com_amazonaws_s3_amazons3::ops::types::{
    INVENTORY_FORMAT_VALUES, INVENTORY_FREQUENCY_VALUES, INVENTORY_INCLUDED_OBJECT_VERSIONS_VALUES,
};
use service_com_amazonaws_s3_amazons3::state::{
    BucketArnFault, ID_MISMATCH_CODE, ID_MISMATCH_MESSAGE, INTERNAL_ERROR_CODE,
    INTERNAL_ERROR_MESSAGE, INVALID_KMS_KEY_ID_CODE, INVALID_KMS_KEY_ID_MESSAGE,
    INVALID_S3_DESTINATION_BUCKET_CODE, MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE,
    MAX_INVENTORY_ID_CHARS, NO_SUCH_CONFIGURATION_CODE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_inventory_configuration::{
    bucket, get_inventory, put_inventory_payload, reported_configurations,
};
use super::bucket_inventory_configuration_listing::list_inventory;
use super::bucket_lifecycle::{router, send, Answer};

/// The XML namespace a client's writer puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// A destination a put is taken with: "the Amazon Resource Name (ARN) of the bucket where
/// inventory results will be published", which names neither a Region nor an account.
const DESTINATION_ARN: &str = "arn:aws:s3:::inventory-reports";

/// A KMS key ARN, the form `SSEKMS$KeyId` is taken in:
/// `arn:<partition>:kms:<region>:<account-id>:key/<key-id>`.
const KMS_KEY_ARN: &str = "arn:aws:kms:us-east-1:123456789012:key/4f2c1d8e-inventory";

/// One `InventoryConfiguration` document, spelled member by member, so a row can carry text
/// no typed builder can express — an id with a space in it, a boolean outside the forms XML
/// spells one in.
#[derive(Debug, Clone)]
struct Document {
    /// `InventoryConfiguration$Id`.
    id: String,
    /// `InventoryConfiguration$IsEnabled`, as the document SPELLS it.
    is_enabled: String,
    /// `InventoryS3BucketDestination$Bucket`.
    destination_bucket: String,
    /// `InventoryS3BucketDestination$Format`.
    format: String,
    /// The `<Encryption>` container's inner XML, or `None` for a destination that names no
    /// encryption.
    encryption: Option<String>,
    /// `InventoryFilter$Prefix`, or `None` for a configuration that carries no `<Filter>`.
    filter_prefix: Option<String>,
    /// `InventorySchedule$Frequency`.
    frequency: String,
    /// `InventoryConfiguration$IncludedObjectVersions`.
    included_object_versions: String,
}

impl Document {
    /// A document every rule admits, identified by `id`: the first value of each published
    /// catalog, published to [`DESTINATION_ARN`], with no encryption and no filter. The shape
    /// every row below varies ONE member of.
    fn taken(id: &str) -> Self {
        Self {
            id: id.to_string(),
            is_enabled: "true".to_string(),
            destination_bucket: DESTINATION_ARN.to_string(),
            format: INVENTORY_FORMAT_VALUES[0].to_string(),
            encryption: None,
            filter_prefix: None,
            frequency: INVENTORY_FREQUENCY_VALUES[0].to_string(),
            included_object_versions: INVENTORY_INCLUDED_OBJECT_VERSIONS_VALUES[0].to_string(),
        }
    }

    /// The same document, published to `bucket`.
    fn publishing_to(mut self, bucket: &str) -> Self {
        self.destination_bucket = bucket.to_string();
        self
    }

    /// The same document, with its reports encrypted under the SSE-KMS key `key_id`.
    fn encrypted_under(mut self, key_id: &str) -> Self {
        self.encryption = Some(format!("<SSE-KMS><KeyId>{key_id}</KeyId></SSE-KMS>"));
        self
    }

    /// The same document, with its reports encrypted the way S3 manages keys itself — the
    /// `InventoryEncryption$SSES3` container, which carries no member at all.
    fn encrypted_by_s3(mut self) -> Self {
        self.encryption = Some("<SSE-S3></SSE-S3>".to_string());
        self
    }

    /// The same document, inventorying only the objects under `prefix`.
    fn filtered(mut self, prefix: &str) -> Self {
        self.filter_prefix = Some(prefix.to_string());
        self
    }

    /// The same document, with `member` spelled `value` — for a value no typed member can
    /// hold.
    fn spelling(mut self, member: &str, value: &str) -> Self {
        match member {
            "IsEnabled" => self.is_enabled = value.to_string(),
            "Format" => self.format = value.to_string(),
            "Frequency" => self.frequency = value.to_string(),
            "IncludedObjectVersions" => self.included_object_versions = value.to_string(),
            other => panic!("no row spells the member {other:?}"),
        }
        self
    }

    /// The payload, in the member order
    /// `smithy_query(query="shape",shape="InventoryConfiguration")` declares.
    fn xml(&self) -> String {
        let mut body = format!("<InventoryConfiguration xmlns=\"{XMLNS}\">");
        body.push_str("<Destination><S3BucketDestination>");
        body.push_str(&format!("<Bucket>{}</Bucket>", self.destination_bucket));
        body.push_str(&format!("<Format>{}</Format>", self.format));
        if let Some(encryption) = &self.encryption {
            body.push_str(&format!("<Encryption>{encryption}</Encryption>"));
        }
        body.push_str("</S3BucketDestination></Destination>");
        body.push_str(&format!("<IsEnabled>{}</IsEnabled>", self.is_enabled));
        if let Some(prefix) = &self.filter_prefix {
            body.push_str(&format!("<Filter><Prefix>{prefix}</Prefix></Filter>"));
        }
        body.push_str(&format!("<Id>{}</Id>", self.id));
        body.push_str(&format!(
            "<IncludedObjectVersions>{}</IncludedObjectVersions>",
            self.included_object_versions
        ));
        body.push_str(&format!(
            "<Schedule><Frequency>{}</Frequency></Schedule>",
            self.frequency
        ));
        body.push_str("</InventoryConfiguration>");
        body
    }
}

/// One row of the document table: the case, the document, and what the put must answer —
/// the status, the error code (`None` for a document that is taken) and the `<Message>`.
type DocumentRow = (&'static str, Document, u16, Option<&'static str>, String);

/// One row of the addressing table: the case, the `id` argument the request states (`None`
/// for a request that states none at all), the document it carries, and the status, code and
/// message the put must answer.
type AddressingRow = (
    &'static str,
    Option<&'static str>,
    Document,
    u16,
    &'static str,
    &'static str,
);

/// `id` as a query string carries it: a space is the one character the ids below carry that
/// a URI may not hold, which a client percent-encodes.
fn as_query_value(id: &str) -> String {
    id.replace(' ', "%20")
}

/// The ids a listing of `name` reports, in the order its body carries them.
async fn listed_ids(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let listed = list_inventory(router, name, None, &[]).await;
    assert_eq!(
        listed.status,
        200,
        "the listing of {name:?} must be served, got {} ({})",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    reported_configurations(&listed)
        .into_iter()
        .map(|reported| reported.id)
        .collect()
}

/// The answer to one put of `document` under the `id` argument `id`.
async fn put(
    router: &MultiServiceRouter,
    name: &str,
    id: &str,
    document: &Document,
) -> Answer {
    put_inventory_payload(router, name, &as_query_value(id), &document.xml(), &[]).await
}

/// Every document the published schema does not admit is refused with the code and the
/// sentence S3 publishes for the rule it broke, and records nothing; every document it admits
/// is taken and reads back.
#[tokio::test]
async fn a_document_outside_the_published_schema_is_refused_and_records_nothing() {
    let router = router();
    let name = "inventory-document-rules";
    bucket(&router, name).await;

    // The id rule's own bound, and one character past it.
    let at_the_ceiling = "i".repeat(MAX_INVENTORY_ID_CHARS);
    let past_the_ceiling = "i".repeat(MAX_INVENTORY_ID_CHARS + 1);
    let malformed = MALFORMED_XML_MESSAGE.to_string();

    let rows: Vec<DocumentRow> = vec![
        // The id rule. The character class is the one the id members publish: "can only
        // contain letters, numbers, periods, dashes, and underscores".
        (
            "an id one character past the 64-character ceiling",
            Document::taken(&past_the_ceiling),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an id carrying a space",
            Document::taken("Daily Report"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an id carrying a character outside the published class",
            Document::taken("daily/report"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an id exactly at the 64-character ceiling",
            Document::taken(&at_the_ceiling),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
        (
            "an id spelled in every character the class holds",
            Document::taken("Daily.Report-v1_2"),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
        // The destination bucket: an ARN, carrying no Region.
        (
            "a destination named by the bare bucket name",
            Document::taken("bare-destination").publishing_to("inventory-reports"),
            400,
            Some(INVALID_S3_DESTINATION_BUCKET_CODE),
            BucketArnFault::NotAnArn.message().to_string(),
        ),
        (
            "a destination ARN of another service",
            Document::taken("foreign-destination")
                .publishing_to("arn:aws:sqs:us-east-1:123456789012:reports"),
            400,
            Some(INVALID_S3_DESTINATION_BUCKET_CODE),
            BucketArnFault::ForeignVendor.message().to_string(),
        ),
        (
            "a destination ARN qualified by a Region",
            Document::taken("regional-destination")
                .publishing_to("arn:aws:s3:us-east-1::inventory-reports"),
            400,
            Some(INVALID_S3_DESTINATION_BUCKET_CODE),
            BucketArnFault::RegionQualified.message().to_string(),
        ),
        (
            "a destination ARN qualified by a Region and an account",
            Document::taken("regional-owned-destination")
                .publishing_to("arn:aws:s3:us-east-1:123456789012:inventory-reports"),
            400,
            Some(INVALID_S3_DESTINATION_BUCKET_CODE),
            BucketArnFault::RegionQualified.message().to_string(),
        ),
        // The destination encryption key: a KMS key's ARN, or SSE-S3, which names none.
        (
            "an SSE-KMS key named by its bare key id",
            Document::taken("bare-key").encrypted_under("4f2c1d8e-inventory"),
            400,
            Some(INVALID_KMS_KEY_ID_CODE),
            INVALID_KMS_KEY_ID_MESSAGE.to_string(),
        ),
        (
            "an SSE-KMS key named by an alias ARN",
            Document::taken("alias-key")
                .encrypted_under("arn:aws:kms:us-east-1:123456789012:alias/inventory"),
            400,
            Some(INVALID_KMS_KEY_ID_CODE),
            INVALID_KMS_KEY_ID_MESSAGE.to_string(),
        ),
        (
            "an SSE-KMS key ARN naming no Region",
            Document::taken("regionless-key")
                .encrypted_under("arn:aws:kms::123456789012:key/4f2c1d8e-inventory"),
            400,
            Some(INVALID_KMS_KEY_ID_CODE),
            INVALID_KMS_KEY_ID_MESSAGE.to_string(),
        ),
        (
            "an SSE-KMS key named by its ARN",
            Document::taken("kms-key").encrypted_under(KMS_KEY_ARN),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
        (
            "reports encrypted the way S3 manages keys itself",
            Document::taken("s3-managed-key").encrypted_by_s3(),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
        // The filter prefix: a filter states one.
        (
            "a filter stating an empty prefix",
            Document::taken("empty-prefix").filtered(""),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "a filter stating a prefix",
            Document::taken("stated-prefix").filtered("reports/"),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
        // A member naming a value outside its enum's published set, including the published
        // value spelled in another case.
        (
            "a Frequency outside its published set",
            Document::taken("bad-frequency").spelling("Frequency", "Hourly"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "a Frequency published value spelled in lower case",
            Document::taken("lowercase-frequency").spelling(
                "Frequency",
                &INVENTORY_FREQUENCY_VALUES[0].to_lowercase(),
            ),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "a destination Format outside its published set",
            Document::taken("bad-format").spelling("Format", "JSON"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an IncludedObjectVersions outside its published set",
            Document::taken("bad-versions").spelling("IncludedObjectVersions", "Some"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        // A value the schema cannot READ as the member's declared type: `IsEnabled` is a
        // boolean, and XML spells one `true` or `false`.
        (
            "an IsEnabled spelled with a capital",
            Document::taken("capital-boolean").spelling("IsEnabled", "True"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an IsEnabled spelled in upper case",
            Document::taken("upper-boolean").spelling("IsEnabled", "FALSE"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an IsEnabled spelled as a word XML does not spell a boolean with",
            Document::taken("worded-boolean").spelling("IsEnabled", "yes"),
            400,
            Some(MALFORMED_XML_CODE),
            malformed.clone(),
        ),
        (
            "an IsEnabled spelled false",
            Document::taken("disabled").spelling("IsEnabled", "false"),
            PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            None,
            String::new(),
        ),
    ];

    let mut taken: Vec<String> = Vec::new();
    for (case, document, status, code, message) in &rows {
        let answer = put(&router, name, &document.id, document).await;
        assert_eq!(
            answer.status,
            *status,
            "case {case:?}: expected HTTP {status}, got {} ({})",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        match code {
            Some(code) => {
                assert_eq!(
                    answer.error_code(),
                    *code,
                    "case {case:?}: expected the code {code}, got {} ({})",
                    answer.error_code(),
                    String::from_utf8_lossy(&answer.body)
                );
                assert_eq!(
                    answer.error_message(),
                    *message,
                    "case {case:?}: expected the message {message:?}, got {:?}",
                    answer.error_message()
                );
                // A refused put records nothing, so the bucket carries no configuration
                // under the id its document named.
                let read = get_inventory(&router, name, &as_query_value(&document.id)).await;
                assert_eq!(
                    read.error_code(),
                    NO_SUCH_CONFIGURATION_CODE,
                    "case {case:?}: expected a refused put to record nothing, so the read must \
                     answer {NO_SUCH_CONFIGURATION_CODE}, got {} ({})",
                    read.error_code(),
                    read.status
                );
            }
            None => {
                assert!(
                    answer.body.is_empty(),
                    "case {case:?}: expected an empty body, got {:?}",
                    String::from_utf8_lossy(&answer.body)
                );
                // A document the rules admit is stored under the id that addresses it, which
                // is what the read reports back.
                let read = get_inventory(&router, name, &document.id).await;
                let reported = reported_configurations(&read);
                let reported_id = reported.first().map(|one| one.id.clone());
                assert_eq!(
                    reported_id.as_deref(),
                    Some(document.id.as_str()),
                    "case {case:?}: expected the stored configuration to read back under its \
                     own id, got {reported_id:?} ({})",
                    read.status
                );
                taken.push(document.id.clone());
            }
        }
    }

    // The listing reports exactly the documents that were taken: no refused document is in
    // the inventory of inventory configurations.
    let mut listed = listed_ids(&router, name).await;
    listed.sort();
    let mut expected = taken.clone();
    expected.sort();
    assert_eq!(
        listed, expected,
        "case the listing after the table: expected exactly the documents that were taken, \
         got {listed:?}"
    );
}

/// The `id` argument and the document's own `Id` name ONE configuration: a request that
/// states another id is refused for the disagreement, and one that states no id at all is
/// refused as a request the service could not carry out. Neither records anything.
#[tokio::test]
async fn the_id_argument_must_address_the_document_the_put_carries() {
    let router = router();
    let name = "inventory-addressing";
    bucket(&router, name).await;

    let rows: Vec<AddressingRow> = vec![
        (
            "an argument naming another configuration",
            Some("argument-id"),
            Document::taken("document-id"),
            400,
            ID_MISMATCH_CODE,
            ID_MISMATCH_MESSAGE,
        ),
        (
            "an argument naming another configuration, the other way round",
            Some("document-id"),
            Document::taken("argument-id"),
            400,
            ID_MISMATCH_CODE,
            ID_MISMATCH_MESSAGE,
        ),
        (
            "an argument differing from the document only in case",
            Some("Daily"),
            Document::taken("daily"),
            400,
            ID_MISMATCH_CODE,
            ID_MISMATCH_MESSAGE,
        ),
        (
            "an empty argument",
            Some(""),
            Document::taken("unaddressed"),
            500,
            INTERNAL_ERROR_CODE,
            INTERNAL_ERROR_MESSAGE,
        ),
        (
            "no argument at all",
            None,
            Document::taken("also-unaddressed"),
            500,
            INTERNAL_ERROR_CODE,
            INTERNAL_ERROR_MESSAGE,
        ),
    ];

    for (case, argument, document, status, code, message) in &rows {
        let answer = match argument {
            Some(argument) => put(&router, name, argument, document).await,
            // The one request no `id`-bearing helper can send: the subresource with no `id`
            // argument at all.
            None => {
                send(
                    &router,
                    "PUT",
                    &format!("/{name}?inventory"),
                    &[],
                    &document.xml(),
                )
                .await
            }
        };
        assert_eq!(
            answer.status,
            *status,
            "case {case:?}: expected HTTP {status}, got {} ({})",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            *code,
            "case {case:?}: expected the code {code}, got {} ({})",
            answer.error_code(),
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_message(),
            *message,
            "case {case:?}: expected the message {message:?}, got {:?}",
            answer.error_message()
        );
    }

    // Nothing any of those refusals named reached the store, under either id.
    for id in [
        "argument-id",
        "document-id",
        "Daily",
        "daily",
        "unaddressed",
        "also-unaddressed",
    ] {
        let read = get_inventory(&router, name, id).await;
        assert_eq!(
            read.error_code(),
            NO_SUCH_CONFIGURATION_CODE,
            "case {id:?}: expected a refused put to record nothing, so the read must answer \
             {NO_SUCH_CONFIGURATION_CODE}, got {} ({})",
            read.error_code(),
            read.status
        );
    }
    let listed = listed_ids(&router, name).await;
    assert!(
        listed.is_empty(),
        "case the listing after the table: expected a bucket carrying no configuration at \
         all, got {listed:?}"
    );

    // And the same document the disagreements carried IS taken once the argument addresses
    // it, which is what tells the refusals above from a document the rules reject.
    let document = Document::taken("document-id");
    let answer = put(&router, name, &document.id, &document).await;
    assert_eq!(
        answer.status,
        PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
        "case the argument addressing the document: expected HTTP \
         {PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS}, got {} ({})",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        listed_ids(&router, name).await,
        vec![document.id.clone()],
        "case the argument addressing the document: expected the listing to report it"
    );
}
