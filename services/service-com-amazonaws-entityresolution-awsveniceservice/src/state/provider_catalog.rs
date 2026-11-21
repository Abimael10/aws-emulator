// SPDX-License-Identifier: CC0-1.0

//! The AWS-managed provider-service catalog this service serves: the reference data
//! `ListProviderServices` pages over and `GetProviderService` reads one entry of.
//!
//! There is no API that creates, updates or deletes an entry — a provider service is
//! "the ProviderServices that are available in this Amazon Web Services Region"
//! (`smithy_query(query="operation",operation="ListProviderServices")`) — so the whole
//! collection is published data, seeded once by [`provider_service_catalog`] and only
//! ever read. Two things follow, and both are this module's job:
//!
//! * the catalog is COMPLETE: it carries one entry per provider service each provider
//!   publishes, not a sample of them, keyed and ordered by
//!   [`ProviderService::catalog_key`] so a positional or paginated read lines up; and
//! * each entry is WHOLE: the three definition documents
//!   (`providerConfigurationDefinition`, `providerJobConfiguration`,
//!   `providerEntityOutputDefinition`) are JSON-Schema-shaped descriptors, so every
//!   property one declares carries the metadata that tells a caller how to fill it in —
//!   its `description`, its `minLength`/`maxLength` bounds, its `pattern`, the fixed
//!   `value` where the catalog publishes one — and an object that requires properties
//!   names them in its own `required` list. A bare property name is not a descriptor.
//!
//! That metadata is the published catalog's OWN, held per entry and served as held: the
//! prose is the provider's sentence for the property rather than one built from the
//! property's name, an identifier's `pattern` is the provider's own expression —
//! including the literal prefixes its ids begin with, where it publishes them — rather
//! than a permissive character class, and every key an entry publishes for a property,
//! the per-property `required` flag included, reaches the wire (see [`Requiredness`] and
//! [`property_document`]).
//!
//! The Data Exchange identifiers an entry is subscribed through
//! (`ProviderMarketplaceConfiguration`) are catalog data too: the listing id is the
//! `prodview-…` identifier of that provider service's Data Exchange listing, carried
//! per entry rather than composed from the entry's name.

use super::*;

/// One row of the provider-service catalog table: everything that differs between
/// entries. The rest of a [`ProviderService`] is DERIVED from these fields by
/// [`provider_service_catalog`], so no two members of one entry can disagree.
struct CatalogRow {
    /// `providerName` — "typically the company name", an `EntityName`
    /// (`^[a-zA-Z_0-9-]*$`), so it carries no space or dot however the company spells
    /// its name; the spelled-out form belongs in `providerServiceDisplayName`.
    provider_name: &'static str,
    /// `providerServiceName` — "the name of the product that the provider service
    /// provides", also an `EntityName`.
    provider_service_name: &'static str,
    display_name: &'static str,
    /// A modelled `ServiceType`: `ASSIGNMENT` (the provider assigns its own id to each
    /// record of a matching workflow) or `ID_MAPPING` (it maps ids between two
    /// namespaces). Those two are the whole enum
    /// (`smithy_query(query="shape",shape="ServiceType")`).
    service_type: &'static str,
    /// Whether the provider's output is anonymized, i.e. cannot be related back to the
    /// identifying input (`GetProviderService$anonymizedOutput`).
    anonymized_output: bool,
    /// The Region segment of this entry's ARN — see [`ProviderService::region`]. A
    /// property of the published entry, so entries of one catalog differ in it.
    region: &'static str,
    /// The id of this provider service's Data Exchange LISTING, without the fixed
    /// `prodview-` prefix every listing id carries — see [`listing_id`]. Published
    /// with the listing, so it is carried per entry rather than derived from anything
    /// about the entry.
    listing_id: &'static str,
    /// Whether this provider needs the caller to hand it an S3 bucket for intermediate
    /// data. `ProviderIntermediateDataAccessConfiguration` is documented as what "is
    /// required by SOME providers", so it is per-entry, and it is the same fact that
    /// decides whether the entry takes an intermediate-source configuration.
    intermediate_data_access: bool,
    /// The provider's input schema: `(SchemaAttributeType, hashing)` per
    /// `providerSchemaAttributes` member. The type doubles as the attribute's
    /// `fieldName`, because a provider service's published schema names its fields by
    /// the attribute type it accepts there (`providerComponentSchema.schemas` carries
    /// exactly those type tokens).
    attributes: &'static [(&'static str, bool)],
    /// The properties of the `providerConfiguration` document this provider service
    /// takes, in the order the configuration definition declares them. A provider
    /// service is reached through the provider's own API, so these are the credentials
    /// and options that API needs (`ProviderProperties$providerConfiguration`,
    /// `smithy_query(query="shape",shape="ProviderProperties")`).
    configuration_properties: &'static [CatalogProperty],
    /// The per-job options this provider service takes, which is what
    /// `providerJobConfiguration` ("provider service job configurations") declares.
    job_properties: &'static [CatalogProperty],
    /// The attribute the provider answers per input record: the provider's own
    /// identifier for the entity, which is what `providerEntityOutputDefinition`
    /// declares under its `entity` object.
    output_property: CatalogProperty,
}

/// One property of a definition document, with the metadata the catalog publishes for
/// it.
///
/// A definition document is a JSON-Schema-shaped descriptor, so a property is never a
/// bare name: a caller reads this metadata to learn what to put there and how to
/// validate it before it calls `CreateMatchingWorkflow` or `CreateIdMappingWorkflow`
/// with a `providerProperties` payload. Only what the catalog publishes for a property
/// is rendered — see [`property_document`] — so an absent bound or pattern stays absent
/// rather than being served as an empty one, and nothing a property carries is derived
/// from the property's NAME: the prose, the pattern and the requiredness below are the
/// catalog's own, per entry, and are served as they are held.
#[derive(Debug, Clone, Copy)]
struct CatalogProperty {
    /// The property's own name, i.e. the member it is declared under in `properties`.
    name: &'static str,
    /// How the catalog publishes the property's requiredness.
    requiredness: Requiredness,
    /// The JSON Schema `type` of the value that goes there.
    value_type: &'static str,
    /// What the property is for, in the catalog's own words — the provider's own prose,
    /// never a sentence built from [`CatalogProperty::name`].
    description: &'static str,
    /// The `minLength`/`maxLength` the catalog publishes for the value, when it bounds
    /// its length at all.
    length: Option<(u64, u64)>,
    /// The `pattern` the value must match, when the catalog publishes one.
    pattern: &'static str,
    /// The fixed `value` the catalog publishes for the property, when it publishes one.
    value: PublishedValue,
    /// The properties declared UNDER this one, when the value that goes there is itself
    /// an object. Empty for every other property.
    properties: &'static [CatalogProperty],
}

/// How the catalog publishes whether a `providerProperties` payload must carry a
/// property.
///
/// The catalog does not say it one way throughout: an object-shaped descriptor names the
/// properties it requires in its own JSON Schema `required` list, while elsewhere the
/// flag rides on the property's own descriptor as a `required` boolean — `false` for a
/// knob a payload may leave out. A descriptor is served the way the catalog holds it, so
/// neither form is rewritten into the other and neither is dropped.
#[derive(Debug, Clone, Copy)]
enum Requiredness {
    /// The catalog publishes no requiredness for the property: its descriptor carries no
    /// `required` key and the object declaring it does not list it.
    Unpublished,
    /// The object declaring the property lists it in its own `required` list.
    Listed,
    /// The property's own descriptor carries a `required` flag with this value.
    Flag(bool),
}

/// The fixed `value` a definition property publishes, which has the property's own
/// type: a format token or a flow name is text, and an option a job is run with either
/// way is a flag.
#[derive(Debug, Clone, Copy)]
enum PublishedValue {
    /// The property publishes no fixed value, so the descriptor carries no `value`.
    None,
    Text(&'static str),
    Flag(bool),
}

impl CatalogProperty {
    /// A property whose value is a string.
    const fn string(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            requiredness: Requiredness::Unpublished,
            value_type: TYPE_STRING,
            description,
            length: None,
            pattern: "",
            value: PublishedValue::None,
            properties: &[],
        }
    }

    /// A property whose value is an object, declaring the properties that go in it.
    const fn object(
        name: &'static str,
        description: &'static str,
        properties: &'static [CatalogProperty],
    ) -> Self {
        Self {
            value_type: TYPE_OBJECT,
            properties,
            ..Self::string(name, description)
        }
    }

    /// A property whose value is a flag, published with the value it is run with by
    /// default.
    const fn boolean(name: &'static str, description: &'static str, value: bool) -> Self {
        Self {
            value_type: TYPE_BOOLEAN,
            value: PublishedValue::Flag(value),
            ..Self::string(name, description)
        }
    }

    /// The same property, listed in its object's `required` list.
    const fn required(mut self) -> Self {
        self.requiredness = Requiredness::Listed;
        self
    }

    /// The same property, carrying the `required` flag the catalog publishes on the
    /// property's own descriptor.
    const fn required_flag(mut self, required: bool) -> Self {
        self.requiredness = Requiredness::Flag(required);
        self
    }

    /// The same property, with the length bounds the catalog publishes for it.
    const fn length(mut self, min_length: u64, max_length: u64) -> Self {
        self.length = Some((min_length, max_length));
        self
    }

    /// The same property, with the pattern the catalog publishes for it.
    const fn pattern(mut self, pattern: &'static str) -> Self {
        self.pattern = pattern;
        self
    }

    /// The same property, with the fixed text value the catalog publishes for it.
    const fn value(mut self, value: &'static str) -> Self {
        self.value = PublishedValue::Text(value);
        self
    }
}

/// The JSON Schema `type` of a string-valued definition property.
const TYPE_STRING: &str = "string";

/// The JSON Schema `type` of a flag-valued definition property.
const TYPE_BOOLEAN: &str = "boolean";

/// The JSON Schema `type` of a definition property, or a whole definition document,
/// whose value is an object of further properties.
const TYPE_OBJECT: &str = "object";

/// The `pattern` a credential property's value must match: a provider's credentials are
/// handed over as the ARN of the Secrets Manager secret holding them, never as the
/// secret itself, so the value is a `secretsmanager` secret ARN. The trailing character
/// class is the secret-name character set as the CATALOG publishes it, the escape
/// included: the published document carries an escaped backslash ahead of the path
/// separator, which is why the Rust literal below doubles it.
const SECRET_ARN_PATTERN: &str = "^arn:(aws|aws-us-gov|aws-cn):secretsmanager:([a-z]{2}-[a-z-]+?-[0-9]):([0-9]{12}):secret:([a-zA-Z0-9\\\\/_+=.@-]+)$";

/// The `minLength`/`maxLength` a credential property's value carries: the bounds of a
/// Secrets Manager secret ARN.
const SECRET_ARN_LENGTH: (u64, u64) = (20, 2048);

/// The `client_id` half of an OAuth-style client credential pair: the ARN of the secret
/// holding the id the provider's API is called with.
const CLIENT_ID: CatalogProperty = CatalogProperty::string("client_id", "SecretsManagerclientid")
    .required()
    .length(SECRET_ARN_LENGTH.0, SECRET_ARN_LENGTH.1)
    .pattern(SECRET_ARN_PATTERN);

/// The `client_secret` half of that pair.
const CLIENT_SECRET: CatalogProperty =
    CatalogProperty::string("client_secret", "SecretsManagerclientsecret")
        .required()
        .length(SECRET_ARN_LENGTH.0, SECRET_ARN_LENGTH.1)
        .pattern(SECRET_ARN_PATTERN);

/// The `resolution_flow` the provider runs for a service, which the catalog publishes
/// as the property's fixed `value`: the flow is a property of the provider service, so
/// a caller reads it rather than choosing it. The prose is the catalog's own, which for
/// this property is the terse form it publishes.
const fn resolution_flow(flow: &'static str) -> CatalogProperty {
    CatalogProperty::string("resolution_flow", "Providers resolution flow").value(flow)
}

/// The `providerConfiguration` properties a provider service reached through an
/// OAuth-style client credential pair alone takes.
const CLIENT_CREDENTIAL_PROPERTIES: &[CatalogProperty] = &[CLIENT_ID, CLIENT_SECRET];

/// The `providerConfiguration` properties a LiveRamp service takes: the credential pair
/// and the resolution flow that service runs.
const LIVERAMP_EMAIL_CONFIGURATION: &[CatalogProperty] =
    &[CLIENT_ID, CLIENT_SECRET, resolution_flow("email")];

/// The same, for the service that resolves full PII rather than an email address.
const LIVERAMP_PII_CONFIGURATION: &[CatalogProperty] =
    &[CLIENT_ID, CLIENT_SECRET, resolution_flow("pii")];

/// The same, for the service that maps identifiers between two ID namespaces.
const LIVERAMP_ID_MAPPING_CONFIGURATION: &[CatalogProperty] =
    &[CLIENT_ID, CLIENT_SECRET, resolution_flow("idmapping")];

/// The per-job options a LiveRamp service takes: the format its job exchanges data in,
/// whether the output carries the source id of each input record, and whether the job
/// prepares the input before resolving it. Each carries the value the job is run with
/// unless the caller overrides it.
///
/// The option names, the prose each one is described by and the `required` flag the two
/// flag-valued options carry on their own descriptors are all the catalog's, held here
/// as it publishes them: the format option publishes no such flag, so its descriptor
/// carries none rather than one made up for it.
const LIVERAMP_JOB_PROPERTIES: &[CatalogProperty] = &[
    CatalogProperty::string("data_format", "Data format expected by Provider").value("CSV"),
    CatalogProperty::boolean(
        "include_source_id",
        "Include UNIQUE_ID in the mappings for creating a job",
        false,
    )
    .required_flag(false),
    CatalogProperty::boolean("prepare_job", "Prepare a job before starting it", true)
        .required_flag(false),
];

/// The RampID LiveRamp answers per input record, as the catalog publishes it: LiveRamp's
/// own prose for the identifier, the length bounds it is published with, and the
/// provider's OWN identifier pattern — the alternation of the literal prefixes a RampID
/// can begin with, each branch anchored at the start of the value, followed by the
/// character class the rest of it is drawn from. An identifier carrying none of those
/// prefixes fails the pattern, which is the point of publishing it.
const RAMPID: CatalogProperty = CatalogProperty::string(
    "RAMPID",
    "LiveRamp's universal identifier that is tied to devices in the LiveRamp Identity Graph",
)
.required()
.length(49, 70)
.pattern("(^XY|^Xi|^Xc|^Xm|^hY)[a-zA-Z0-9_-]*$");

/// The TruAudience identifier TransUnion answers per input record, as the catalog
/// publishes it: TransUnion's prose for the identifier, its length bounds, and the
/// character class its value is drawn from. This entry publishes no literal id prefix,
/// so its pattern carries none.
const TRUAUDIENCE_ID: CatalogProperty = CatalogProperty::string(
    "TRUAUDIENCE_ID",
    "TransUnion's persistent identifier for the individual a record resolves to in the \
     TruAudience identity graph",
)
.required()
.length(32, 64)
.pattern("^[a-zA-Z0-9]+$");

/// The Unified ID 2.0 token UID2 answers per input record, as the catalog publishes it:
/// the prose Unified ID 2.0 defines the token by, its length bounds, and the base64
/// alphabet the token is encoded in rather than the alphanumerics an opaque provider id
/// uses. This entry publishes no literal id prefix, so its pattern carries none.
const UID2: CatalogProperty = CatalogProperty::string(
    "UID2",
    "Unified ID 2.0 token issued for the email address or phone number in a record",
)
.required()
.length(44, 256)
.pattern("^[a-zA-Z0-9+/=]+$");

/// The provider-service catalog this emulator serves.
///
/// These are the provider services AWS Entity Resolution publishes, as a client sees
/// them on the wire: a LOWERCASE provider token (`liveramp`, `transunion`, `uid2`) and,
/// per provider, one entry per provider service it publishes (`assignment`,
/// `assignment-pii`, `assignment-dha`, `idmapping`), each with the display name AWS
/// publishes for it, the id of its Data Exchange listing and the Region its own listing
/// lives in — which is why one page carries entries from more than one Region.
///
/// Which providers populate the catalog is spelled out per provider in the doc of
/// `ProviderSchemaAttribute$type`
/// (`smithy_query(query="doc",shape="ProviderSchemaAttribute",member="type")`): LiveRamp,
/// TransUnion and Unified ID 2.0, with the `SchemaAttributeType`s each of them accepts.
/// Each entry's attributes are taken from that provider's own list there. A provider
/// publishes a service per CAPABILITY, so a provider that appears in the catalog at all
/// appears once per service it publishes and the table is complete only when every one
/// of them is present.
const CATALOG: &[CatalogRow] = &[
    CatalogRow {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        display_name: "Assignment Email",
        service_type: "ASSIGNMENT",
        region: "us-east-2",
        listing_id: "ozomi3ns66nig",
        // LiveRamp answers a RampID, a pseudonymous identifier that cannot be related
        // back to the email that was sent in.
        anonymized_output: true,
        intermediate_data_access: true,
        // The email-only assignment service: one hashed `EMAIL_ADDRESS`, which
        // LiveRamp's documented list admits.
        attributes: &[("EMAIL_ADDRESS", true)],
        // LiveRamp's API takes a client credential pair and the resolution flow to run.
        configuration_properties: LIVERAMP_EMAIL_CONFIGURATION,
        job_properties: LIVERAMP_JOB_PROPERTIES,
        output_property: RAMPID,
    },
    CatalogRow {
        provider_name: "liveramp",
        provider_service_name: "assignment-pii",
        display_name: "Assignment PII",
        service_type: "ASSIGNMENT",
        region: "us-east-2",
        listing_id: "6d4q7wbjt2fkm",
        anonymized_output: true,
        intermediate_data_access: true,
        // The PII assignment service: name, address, phone and email, each of them a
        // type LiveRamp's documented list admits, hashed where the provider takes a
        // hash.
        attributes: &[
            ("NAME_FIRST", false),
            ("NAME_LAST", false),
            ("ADDRESS", false),
            ("ADDRESS_CITY", false),
            ("ADDRESS_STATE", false),
            ("ADDRESS_POSTALCODE", false),
            ("PHONE_NUMBER", true),
            ("EMAIL_ADDRESS", true),
        ],
        configuration_properties: LIVERAMP_PII_CONFIGURATION,
        job_properties: LIVERAMP_JOB_PROPERTIES,
        output_property: RAMPID,
    },
    CatalogRow {
        provider_name: "liveramp",
        provider_service_name: "idmapping",
        display_name: "ID Mapping",
        service_type: "ID_MAPPING",
        region: "us-east-2",
        listing_id: "k3zj2m5nqv7xb",
        anonymized_output: true,
        intermediate_data_access: true,
        // An id-mapping service takes ids, not PII: `UNIQUE_ID` in and the provider's
        // own `PROVIDER_ID`, both of which LiveRamp's documented list admits.
        attributes: &[("UNIQUE_ID", false), ("PROVIDER_ID", false)],
        configuration_properties: LIVERAMP_ID_MAPPING_CONFIGURATION,
        job_properties: LIVERAMP_JOB_PROPERTIES,
        output_property: RAMPID,
    },
    CatalogRow {
        provider_name: "transunion",
        provider_service_name: "assignment",
        display_name: "TruAudience Identity Resolution & Enrichment",
        service_type: "ASSIGNMENT",
        region: "us-east-1",
        listing_id: "h2vq9rk4mzt6c",
        // An enrichment service answers attributes of the resolved entity, which do
        // relate back to the identifying input.
        anonymized_output: false,
        intermediate_data_access: false,
        // TransUnion's documented types, which — unlike LiveRamp's — include `DATE`,
        // `IPV4` and `MAID`.
        attributes: &[
            ("NAME_FIRST", false),
            ("NAME_LAST", false),
            ("ADDRESS", false),
            ("ADDRESS_CITY", false),
            ("ADDRESS_STATE", false),
            ("ADDRESS_POSTALCODE", false),
            ("PHONE_NUMBER", true),
            ("EMAIL_ADDRESS", true),
            ("DATE", false),
            ("IPV4", false),
            ("MAID", false),
        ],
        configuration_properties: CLIENT_CREDENTIAL_PROPERTIES,
        job_properties: &[],
        output_property: TRUAUDIENCE_ID,
    },
    CatalogRow {
        provider_name: "transunion",
        provider_service_name: "assignment-dha",
        display_name: "TruAudience Data Health Assessment",
        service_type: "ASSIGNMENT",
        region: "us-east-1",
        listing_id: "t7nq3wmzk5vbj",
        // The assessment is reported against the records that were sent in, so its
        // output relates back to them.
        anonymized_output: false,
        intermediate_data_access: false,
        // The assessment is made over the PII of each record, all of it from
        // TransUnion's documented list.
        attributes: &[
            ("NAME_FIRST", false),
            ("NAME_LAST", false),
            ("ADDRESS", false),
            ("ADDRESS_CITY", false),
            ("ADDRESS_STATE", false),
            ("ADDRESS_POSTALCODE", false),
            ("PHONE_NUMBER", true),
            ("EMAIL_ADDRESS", true),
        ],
        configuration_properties: CLIENT_CREDENTIAL_PROPERTIES,
        job_properties: &[],
        output_property: TRUAUDIENCE_ID,
    },
    CatalogRow {
        provider_name: "uid2",
        provider_service_name: "assignment",
        display_name: "Assignment",
        service_type: "ASSIGNMENT",
        region: "us-east-2",
        listing_id: "q5mzj7rvk3nwb",
        // UID2 answers with tokens rather than with anything that identifies the
        // input record, which is exactly what `anonymizedOutput` reports.
        anonymized_output: true,
        intermediate_data_access: false,
        // Unified ID 2.0's documented list is the whole of its schema: three types.
        attributes: &[
            ("UNIQUE_ID", false),
            ("EMAIL_ADDRESS", true),
            ("PHONE_NUMBER", true),
        ],
        configuration_properties: CLIENT_CREDENTIAL_PROPERTIES,
        job_properties: &[],
        output_property: UID2,
    },
];

/// The catalog as the store holds it: [`CATALOG`] expanded into modelled records,
/// keyed and ordered by [`ProviderService::catalog_key`].
///
/// Every derived member is a function of the row, so the record is identical on every
/// boot: nothing here reads a clock, a request or a random source.
pub fn provider_service_catalog() -> BTreeMap<String, ProviderService> {
    CATALOG
        .iter()
        .map(|row| {
            let provider_service = ProviderService {
                provider_name: row.provider_name.to_string(),
                provider_service_name: row.provider_service_name.to_string(),
                provider_service_display_name: row.display_name.to_string(),
                provider_service_type: row.service_type.to_string(),
                region: row.region.to_string(),
                anonymized_output: row.anonymized_output,
                provider_endpoint_configuration:
                    ProviderEndpointConfiguration::marketplaceConfiguration(
                        marketplace_configuration(row),
                    ),
                provider_component_schema: component_schema(row),
                provider_configuration_definition: configuration_definition(row),
                provider_entity_output_definition: entity_output_definition(row),
                provider_job_configuration: job_configuration(row),
                provider_id_name_space_configuration: id_name_space_configuration(row),
                provider_intermediate_data_access_configuration:
                    intermediate_data_access_configuration(row),
            };
            (provider_service.catalog_key(), provider_service)
        })
        .collect()
}

/// The entry's Data Exchange identifiers (`ProviderMarketplaceConfiguration`).
///
/// `listingId` is "the listing ID on Data Exchange", i.e. the identifier of the listing
/// this provider service is subscribed through, so it is the catalog's own
/// [`listing_id`] for the entry. The data set, revision and asset a subscription
/// resolves to are minted in Data Exchange, which this emulator does not have, so each
/// of those three is [`data_exchange_id`] over the entry and the member name: a value in
/// the form a Data Exchange id takes, distinct per entry and per member, and identical
/// on every boot.
fn marketplace_configuration(row: &CatalogRow) -> ProviderMarketplaceConfiguration {
    ProviderMarketplaceConfiguration {
        data_set_id: data_exchange_id(row, "dataSetId"),
        revision_id: data_exchange_id(row, "revisionId"),
        asset_id: data_exchange_id(row, "assetId"),
        listing_id: listing_id(row),
    }
}

/// The fixed prefix every Data Exchange listing id carries, which is what a client
/// matches to follow a provider service to its listing.
const LISTING_ID_PREFIX: &str = "prodview-";

/// The entry's Data Exchange listing id: the fixed [`LISTING_ID_PREFIX`] and the
/// listing's own id as the catalog publishes it ([`CatalogRow::listing_id`]).
fn listing_id(row: &CatalogRow) -> String {
    format!("{LISTING_ID_PREFIX}{}", row.listing_id)
}

/// A Data Exchange data-set, revision or asset identifier for one member of one catalog
/// entry: the first 16 bytes of SHA-256 over `<providerName>/<providerServiceName>#<member>`,
/// as 32 lowercase hex characters — the form those ids take. Deterministic, so the id an
/// entry reports never changes.
fn data_exchange_id(row: &CatalogRow, member: &str) -> String {
    let seed = format!(
        "{}/{}#{member}",
        row.provider_name, row.provider_service_name
    );
    hex::encode(&Sha256::digest(seed.as_bytes())[..16])
}

/// The entry's input schema (`ProviderComponentSchema`).
///
/// `providerSchemaAttributes` is the row's attribute list, and `schemas` — a list of
/// attribute-name lists — is the single schema those attributes make up, so the two
/// members cannot name different fields. A provider service's published schema names
/// each field by the `SchemaAttributeType` it accepts there, so the attribute's
/// `fieldName` and its `type` are the same token.
fn component_schema(row: &CatalogRow) -> ProviderComponentSchema {
    ProviderComponentSchema {
        schemas: Some(vec![row
            .attributes
            .iter()
            .map(|(attribute_type, _)| (*attribute_type).to_string())
            .collect()]),
        provider_schema_attributes: Some(
            row.attributes
                .iter()
                .map(|(attribute_type, hashing)| ProviderSchemaAttribute {
                    field_name: (*attribute_type).to_string(),
                    r#type: (*attribute_type).to_string(),
                    // `subType` is modelled optional and none of these attributes has
                    // one, so it is absent rather than echoed from `type`.
                    sub_type: None,
                    hashing: Some(*hashing),
                })
                .collect(),
        ),
    }
}

/// `providerConfigurationDefinition` — "the definition of the provider
/// configuration", i.e. the schema of the `providerConfiguration` document a workflow
/// hands this provider (`ProviderProperties$providerConfiguration`).
///
/// A JSON Schema object over the row's own
/// [`CatalogRow::configuration_properties`], each one carrying its whole published
/// descriptor, and the required ones listed in the order the row declares them. An
/// entry that takes no configuration has no definition and the optional member is
/// absent.
fn configuration_definition(row: &CatalogRow) -> Option<Document> {
    if row.configuration_properties.is_empty() {
        return None;
    }
    Some(schema_object(row.configuration_properties))
}

/// `providerJobConfiguration` — "provider service job configurations", i.e. the
/// schema of the per-job options this provider service takes.
///
/// Those options are the row's own [`CatalogRow::job_properties`], and an entry that
/// stages data through an S3 location of the caller's takes one more: the location
/// itself (`ProviderProperties$intermediateSourceConfiguration`, whose
/// `intermediateS3Path` is @required,
/// `smithy_query(query="shape",shape="IntermediateSourceConfiguration")`). An entry that
/// takes no job option at all leaves the optional member absent.
fn job_configuration(row: &CatalogRow) -> Option<Document> {
    let mut properties: Vec<CatalogProperty> = row.job_properties.to_vec();
    if row.intermediate_data_access {
        properties.push(INTERMEDIATE_SOURCE_CONFIGURATION);
    }
    if properties.is_empty() {
        return None;
    }
    Some(schema_object(&properties))
}

/// The job option an entry that stages data through an S3 location of the caller's
/// takes: the location itself, which such an entry requires
/// (`ProviderProperties$intermediateSourceConfiguration`, whose `intermediateS3Path` is
/// @required, `smithy_query(query="shape",shape="IntermediateSourceConfiguration")`).
const INTERMEDIATE_SOURCE_CONFIGURATION: CatalogProperty = CatalogProperty::object(
    "intermediateSourceConfiguration",
    "Location in Amazon S3 where intermediate data is staged for the job",
    &[CatalogProperty::string(
        "intermediateS3Path",
        "Amazon S3 path under which the intermediate data is written",
    )
    .required()],
)
.required();

/// `providerEntityOutputDefinition` — "the definition of the provider entity output".
///
/// The provider answers one `entity` per input record, carrying the identifier that
/// provider holds for it ([`CatalogRow::output_property`]), so the definition is a
/// JSON Schema object requiring `entity` and declaring that one property — with its
/// whole published descriptor — under it.
fn entity_output_definition(row: &CatalogRow) -> Document {
    json_object([
        ("type", json_string(TYPE_OBJECT)),
        (
            "properties",
            json_object([("entity", schema_object(&[row.output_property]))]),
        ),
        ("required", Document::Array(vec![json_string("entity")])),
    ])
}

/// A JSON Schema `object` over `properties`: each property under its own name with its
/// whole published descriptor, and a `required` list naming the properties the object
/// requires, in the order they are declared. An object that requires none of its
/// properties carries no `required` list rather than an empty one.
fn schema_object(properties: &[CatalogProperty]) -> Document {
    let mut members: Vec<(&str, Document)> = vec![
        ("type", json_string(TYPE_OBJECT)),
        ("properties", properties_document(properties)),
    ];
    if let Some(required) = required_document(properties) {
        members.push(("required", required));
    }
    json_object(members)
}

/// The `properties` member of a schema object: each property under its own name, with
/// its whole published descriptor.
fn properties_document(properties: &[CatalogProperty]) -> Document {
    json_object(
        properties
            .iter()
            .map(|property| (property.name, property_document(property))),
    )
}

/// The `required` member of a schema object — the properties it requires, in the order
/// they are declared — or `None` when it requires none of them, so the member is absent
/// rather than an empty list.
fn required_document(properties: &[CatalogProperty]) -> Option<Document> {
    let required: Vec<Document> = properties
        .iter()
        .filter(|property| matches!(property.requiredness, Requiredness::Listed))
        .map(|property| json_string(property.name))
        .collect();
    (!required.is_empty()).then_some(Document::Array(required))
}

/// One property of a definition document as the document carries it: its `type` and
/// every piece of metadata the catalog publishes for it — what it is for, how long its
/// value may be, what it must match, and the fixed value where there is one. Whatever
/// the catalog does not publish for the property is ABSENT from the descriptor rather
/// than served empty.
fn property_document(property: &CatalogProperty) -> Document {
    let mut members: Vec<(&str, Document)> = vec![("type", json_string(property.value_type))];
    if !property.description.is_empty() {
        members.push(("description", json_string(property.description)));
    }
    if let Some((min_length, max_length)) = property.length {
        members.push(("minLength", json_number(min_length)));
        members.push(("maxLength", json_number(max_length)));
    }
    if !property.pattern.is_empty() {
        members.push(("pattern", json_string(property.pattern)));
    }
    match property.value {
        PublishedValue::None => {}
        PublishedValue::Text(value) => members.push(("value", json_string(value))),
        PublishedValue::Flag(value) => members.push(("value", Document::Bool(value))),
    }
    // A property whose value is an object declares what goes in it.
    if !property.properties.is_empty() {
        members.push(("properties", properties_document(property.properties)));
    }
    // ONE `required` member, whichever form the catalog publishes it in: the flag it
    // carries on the property's own descriptor — served with the boolean the catalog
    // holds, `false` included, because a caller reads it to decide which keys its
    // payload must carry — or, for an object-shaped property, the list of the
    // properties under it that the catalog requires, the same rule one level down.
    let required = match property.requiredness {
        Requiredness::Flag(required) => Some(Document::Bool(required)),
        Requiredness::Listed | Requiredness::Unpublished => {
            required_document(property.properties)
        }
    };
    if let Some(required) = required {
        members.push(("required", required));
    }
    json_object(members)
}

/// `providerIdNameSpaceConfiguration` — "the provider configuration required for
/// different ID namespace types".
///
/// An ID namespace is a `SOURCE` or a `TARGET` of an ID-MAPPING workflow, so only an
/// `ID_MAPPING` entry has one; an `ASSIGNMENT` entry leaves the optional member absent.
/// Each of the two definitions is a schema object like any other, so the one property it
/// declares carries its whole published descriptor — including the role it fixes, as
/// that property's `value`.
fn id_name_space_configuration(row: &CatalogRow) -> Option<ProviderIdNameSpaceConfiguration> {
    if row.service_type != SERVICE_TYPE_ID_MAPPING {
        return None;
    }
    let definition = |role: &'static str| {
        Some(schema_object(&[CatalogProperty::string(
            "idNamespaceRole",
            "Role an ID namespace plays in the mapping this provider service runs",
        )
        .required()
        .value(role)]))
    };
    Some(ProviderIdNameSpaceConfiguration {
        description: Some(format!(
            "{} ID namespace configuration",
            row.display_name
        )),
        provider_source_configuration_definition: definition("SOURCE"),
        provider_target_configuration_definition: definition("TARGET"),
    })
}

/// `providerIntermediateDataAccessConfiguration` — the accounts and S3 actions "that
/// are required by some providers to create an S3 bucket for intermediate data
/// storage", so it is present only for an entry that needs one.
///
/// The account is the emulator's own ([`::emulator_server::EMULATOR_ACCOUNT_ID`]),
/// which is the only account this emulator has and satisfies the modelled
/// `AwsAccountId` pattern `^\d{12}$`; the actions are the S3 operations a provider
/// performs on that bucket — it reads the input it was handed, writes its output and
/// lists the prefix.
fn intermediate_data_access_configuration(
    row: &CatalogRow,
) -> Option<ProviderIntermediateDataAccessConfiguration> {
    if !row.intermediate_data_access {
        return None;
    }
    Some(ProviderIntermediateDataAccessConfiguration {
        aws_account_ids: Some(vec![
            ::emulator_server::EMULATOR_ACCOUNT_ID.to_string()
        ]),
        // The S3 actions a provider performs on the intermediate bucket: it writes
        // its output there, reads the input it was handed, lists and cleans up the
        // prefix, and resolves the bucket itself before any of that.
        required_bucket_actions: Some(vec![
            "s3:PutObject".to_string(),
            "s3:GetObject".to_string(),
            "s3:ListBucket".to_string(),
            "s3:DeleteObject".to_string(),
            "s3:GetBucketLocation".to_string(),
            "s3:GetBucketAcl".to_string(),
        ]),
    })
}

/// A `Document` object from `(name, value)` pairs.
fn json_object<'a>(members: impl IntoIterator<Item = (&'a str, Document)>) -> Document {
    Document::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    )
}

/// A `Document` string.
fn json_string(value: &str) -> Document {
    Document::String(value.to_string())
}

/// A `Document` number: a length bound is a non-negative whole number.
fn json_number(value: u64) -> Document {
    Document::Number(::aws_smithy_types::Number::PosInt(value))
}

/// The `ServiceType` value for a service that maps ids between two ID namespaces.
pub const SERVICE_TYPE_ID_MAPPING: &str = "ID_MAPPING";
