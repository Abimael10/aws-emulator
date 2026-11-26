// SPDX-License-Identifier: CC0-1.0

//! What a submitted `DistributionConfig`'s `TenantConfig` is admitted by, end to end over
//! HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! THREE rules, each about the submitted configuration alone, so each holds on the create and
//! on the update alike:
//!
//! * `TenantConfig` "only supports multi-tenant distributions. You can't specify this field
//!   for standard distributions"
//!   (`smithy_query(query="shape",shape="TenantConfig")`), and what makes a configuration
//!   multi-tenant is the `ConnectionMode` member it states — "whether the connection mode is
//!   through a standard distribution (direct) or a multi-tenant distribution with distribution
//!   tenants (tenant-only)" — so a configuration that states `direct`, states another mode or
//!   states none at all may carry no `TenantConfig`, however its definitions read;
//! * a definition's `Definition` is "the value that you assigned to the parameter" and
//!   `StringSchemaConfig` is the schema that value is admitted by, with `Required` "whether the
//!   defined parameter is required"
//!   (`smithy_query(query="shape",shape="ParameterDefinition,ParameterDefinitionSchema,StringSchemaConfig")`)
//!   — so a definition states a schema at all, and one that marks the parameter required states
//!   no `DefaultValue`, which would supply the value every tenant is required to supply itself;
//! * a member of the configuration that addresses the origin may reference at most two of the
//!   parameters its definitions declare: a field past that ceiling is refused with the sentence
//!   that states it.
//!
//! The rows span what each rule generalises over: every arrangement of the `ConnectionMode`
//! member and `TenantConfig`; a schema that is required with and without a default, optional
//! with and without one, absent entirely, and a list where only one entry of several is
//! unusable; and a field referencing none, one, two, three and four parameters, in each of the
//! two templated members, beside a configuration whose two members each reference the ceiling
//! (the bound is per FIELD, not per configuration). Every refusal is asserted to have left
//! nothing behind, and every acceptance to round-trip the members it submitted.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, reused rather than copied.

use crate::distribution_lifecycle::{
    create_distribution, emulator, get_distribution, list_distributions, update_distribution,
    Answer, API_VERSION,
};

/// `ConnectionMode`'s two values, from the model's enum
/// (`smithy_query(query="shape",shape="ConnectionMode")`).
const DIRECT: &str = "direct";
const TENANT_ONLY: &str = "tenant-only";

/// The code every refusal here travels under, and the status the model gives it
/// (`smithy_query(query="operation",operation="CreateDistribution")` -> `InvalidArgument`,
/// httpError 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The sentence a `TenantConfig` on a configuration that does not declare the tenant-only
/// connection mode is refused with, written out as the LITERAL the service answers so a
/// reworded refusal fails this suite.
const TENANT_CONFIG_ON_DIRECT: &str =
    "The parameter TenantConfig cannot be set for a direct distribution.";

/// The sentence a parameter definition list carrying an unusable schema is refused with.
const INVALID_SCHEMAS: &str = "One or more parameter definition schemas aren't valid. Review \
     the parameters for the multi-tenant distribution and try again.";

/// The sentence a member referencing more parameters than the ceiling is refused with.
const TOO_MANY_PARAMETERS: &str = "Your request exceeds the maximum number of parameters per \
     field. Specify fewer parameters and try again.";

/// The most parameters one member of a configuration may reference, which the rows sit on
/// both sides of.
const MAX_PARAMETERS_PER_FIELD: usize = 2;

/// The parameter names the definitions below declare, in the order a reference row uses them.
const DECLARED: &[&str] = &["tenantName", "stage", "regionLabel", "slot"];

/// One `TenantConfig$ParameterDefinitions` entry a row declares.
#[derive(Debug, Clone, Copy)]
struct Definition {
    name: &'static str,
    /// The `StringSchema` the definition states, or `None` for a `Definition` that states no
    /// schema at all.
    schema: Option<Schema>,
}

/// The `StringSchemaConfig` one definition states.
#[derive(Debug, Clone, Copy)]
struct Schema {
    required: bool,
    default_value: Option<&'static str>,
}

impl Definition {
    /// A definition of `name` whose schema requires a value and supplies no default.
    fn required(name: &'static str) -> Self {
        Self {
            name,
            schema: Some(Schema {
                required: true,
                default_value: None,
            }),
        }
    }

    /// A definition of `name` whose schema requires nothing and supplies no default.
    fn optional(name: &'static str) -> Self {
        Self {
            name,
            schema: Some(Schema {
                required: false,
                default_value: None,
            }),
        }
    }

    /// The `member` element this definition travels in. `ParameterDefinitions` is a list whose
    /// member carries no `xmlName`
    /// (`smithy_query(query="shape",shape="ParameterDefinitions")`), so each entry travels in
    /// the restXml default `<member>` element.
    fn xml(&self) -> String {
        let mut out = format!("<member><Name>{}</Name><Definition>", self.name);
        if let Some(schema) = self.schema {
            out.push_str("<StringSchema>");
            if let Some(default_value) = schema.default_value {
                out.push_str(&format!("<DefaultValue>{default_value}</DefaultValue>"));
            }
            out.push_str(&format!("<Required>{}</Required>", schema.required));
            out.push_str("</StringSchema>");
        }
        out.push_str("</Definition></member>");
        out
    }
}

/// One configuration a row submits.
#[derive(Debug, Clone)]
struct Document {
    /// The `ConnectionMode` member, or `None` to leave it out entirely.
    connection_mode: Option<&'static str>,
    /// `TenantConfig$ParameterDefinitions`, or `None` for no `TenantConfig` at all.
    definitions: Option<Vec<Definition>>,
    /// `Origin$DomainName` of the single origin.
    domain_name: String,
    /// `Origin$OriginPath`, or `None` to leave the member out.
    origin_path: Option<String>,
}

impl Default for Document {
    /// A multi-tenant configuration with one declared parameter and no reference to it: the
    /// document every row below varies in one place.
    fn default() -> Self {
        Self {
            connection_mode: Some(TENANT_ONLY),
            definitions: Some(vec![Definition::required(DECLARED[0])]),
            domain_name: "origin.example.com".to_string(),
            origin_path: None,
        }
    }
}

impl Document {
    /// The `DistributionConfig` XML a client sends for this document under `reference`.
    ///
    /// `PriceClass` is stated on a STANDARD document only: the member "only supports standard
    /// distributions", so a multi-tenant configuration that states it is refused, while an
    /// update of a standard distribution that leaves it out is refused as incomplete.
    fn xml(&self, reference: &str) -> String {
        let mut out = format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
             <DomainName>{}</DomainName>",
            self.domain_name
        );
        if let Some(origin_path) = &self.origin_path {
            out.push_str(&format!("<OriginPath>{origin_path}</OriginPath>"));
        }
        out.push_str(
            "</Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
             <Comment>tenant config admission</Comment>",
        );
        if self.connection_mode != Some(TENANT_ONLY) {
            out.push_str("<PriceClass>PriceClass_All</PriceClass>");
        }
        out.push_str("<Enabled>true</Enabled>");
        if let Some(definitions) = &self.definitions {
            out.push_str("<TenantConfig><ParameterDefinitions>");
            for definition in definitions {
                out.push_str(&definition.xml());
            }
            out.push_str("</ParameterDefinitions></TenantConfig>");
        }
        if let Some(connection_mode) = self.connection_mode {
            out.push_str(&format!("<ConnectionMode>{connection_mode}</ConnectionMode>"));
        }
        out.push_str("</DistributionConfig>");
        out
    }

    /// Whether this document declares the tenant-only connection mode, which is what decides
    /// which stored distribution an update of it can land on.
    fn declares_tenant_only(&self) -> bool {
        self.connection_mode == Some(TENANT_ONLY)
    }
}

/// What the emulator has to answer a row with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Accepted,
    Refused(&'static str),
}

/// One row: a document, and what the emulator has to answer it with.
struct Case {
    case: String,
    document: Document,
    expect: Expect,
}

/// `count` references to distinct declared parameters, joined into one path.
fn path_referencing(count: usize) -> String {
    let mut out = String::new();
    for name in DECLARED.iter().take(count) {
        out.push_str(&format!("/{{{{{name}}}}}"));
    }
    out
}

/// Every row the three rules are asserted over.
fn cases() -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();

    // The member rule: every arrangement of `ConnectionMode` and `TenantConfig`.
    for (mode_case, connection_mode) in [
        ("the member stating tenant-only", Some(TENANT_ONLY)),
        ("the member stating direct", Some(DIRECT)),
        ("no connection mode member at all", None),
    ] {
        for (tenancy_case, definitions) in [
            (
                "a TenantConfig",
                Some(vec![Definition::required(DECLARED[0])]),
            ),
            ("an empty TenantConfig", Some(Vec::new())),
            ("no TenantConfig", None),
        ] {
            let states_tenant_config = definitions.is_some();
            let tenant_only = connection_mode == Some(TENANT_ONLY);
            cases.push(Case {
                case: format!("{mode_case} with {tenancy_case}"),
                document: Document {
                    connection_mode,
                    definitions,
                    ..Document::default()
                },
                expect: if states_tenant_config && !tenant_only {
                    Expect::Refused(TENANT_CONFIG_ON_DIRECT)
                } else {
                    Expect::Accepted
                },
            });
        }
    }

    // The schema rule: what one definition may state, and a list where one entry of several
    // is unusable.
    let unusable_default = Definition {
        name: DECLARED[1],
        schema: Some(Schema {
            required: true,
            default_value: Some("prod"),
        }),
    };
    let no_schema = Definition {
        name: DECLARED[2],
        schema: None,
    };
    let optional_default = Definition {
        name: DECLARED[3],
        schema: Some(Schema {
            required: false,
            default_value: Some("eu"),
        }),
    };
    for (schema_case, definitions, expect) in [
        (
            "a required definition with no default",
            vec![Definition::required(DECLARED[0])],
            Expect::Accepted,
        ),
        (
            "an optional definition with no default",
            vec![Definition::optional(DECLARED[0])],
            Expect::Accepted,
        ),
        (
            "an optional definition carrying a default",
            vec![optional_default],
            Expect::Accepted,
        ),
        (
            "every usable schema at once",
            vec![
                Definition::required(DECLARED[0]),
                Definition::optional(DECLARED[1]),
                optional_default,
            ],
            Expect::Accepted,
        ),
        (
            "a required definition carrying a default",
            vec![unusable_default],
            Expect::Refused(INVALID_SCHEMAS),
        ),
        (
            "a definition stating no schema at all",
            vec![no_schema],
            Expect::Refused(INVALID_SCHEMAS),
        ),
        (
            "one unusable schema among several usable ones",
            vec![
                Definition::required(DECLARED[0]),
                optional_default,
                unusable_default,
            ],
            Expect::Refused(INVALID_SCHEMAS),
        ),
    ] {
        cases.push(Case {
            case: format!("tenant-only declaring {schema_case}"),
            document: Document {
                definitions: Some(definitions),
                ..Document::default()
            },
            expect,
        });
    }

    // The per-field ceiling: each templated member referencing none up to one past the
    // ceiling, with every referenced name declared.
    let declared: Vec<Definition> = DECLARED.iter().copied().map(Definition::optional).collect();
    for count in 0..=MAX_PARAMETERS_PER_FIELD + 2 {
        let expect = if count > MAX_PARAMETERS_PER_FIELD {
            Expect::Refused(TOO_MANY_PARAMETERS)
        } else {
            Expect::Accepted
        };
        cases.push(Case {
            case: format!("an origin path referencing {count} parameters"),
            document: Document {
                definitions: Some(declared.clone()),
                origin_path: Some(format!("{}/assets", path_referencing(count))),
                ..Document::default()
            },
            expect,
        });
        let domain_name = DECLARED
            .iter()
            .take(count)
            .map(|name| format!("{{{{{name}}}}}."))
            .collect::<String>();
        cases.push(Case {
            case: format!("an origin domain name referencing {count} parameters"),
            document: Document {
                definitions: Some(declared.clone()),
                domain_name: format!("{domain_name}example.com"),
                ..Document::default()
            },
            expect,
        });
    }
    // The ceiling is per FIELD: a configuration whose two templated members each reference it
    // is admitted, and one whose SECOND member passes it is refused.
    cases.push(Case {
        case: "both templated members referencing the ceiling".to_string(),
        document: Document {
            definitions: Some(declared.clone()),
            domain_name: "{{tenantName}}.{{stage}}.example.com".to_string(),
            origin_path: Some("/{{regionLabel}}/{{slot}}".to_string()),
            ..Document::default()
        },
        expect: Expect::Accepted,
    });
    cases.push(Case {
        case: "the domain name at the ceiling and the origin path past it".to_string(),
        document: Document {
            definitions: Some(declared),
            domain_name: "{{tenantName}}.{{stage}}.example.com".to_string(),
            origin_path: Some(path_referencing(MAX_PARAMETERS_PER_FIELD + 1)),
            ..Document::default()
        },
        expect: Expect::Refused(TOO_MANY_PARAMETERS),
    });

    cases
}

/// Assert that `answer` is what `expect` says, and that a refusal announced no resource.
fn assert_answer(case: &str, answer: &Answer, expect: Expect, context: &str, success: u16) {
    match expect {
        Expect::Accepted => assert_eq!(
            answer.status, success,
            "case {case} ({context}): expected the configuration to be stored, got {} with {}",
            answer.status, answer.raw
        ),
        Expect::Refused(message) => {
            assert_eq!(
                answer.status, 400,
                "case {case} ({context}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                INVALID_ARGUMENT,
                "case {case} ({context}): expected {INVALID_ARGUMENT}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let answered = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                answered, message,
                "case {case} ({context}): expected the message {message:?}, got {answered:?}"
            );
            assert_eq!(
                answer.header("location"),
                "",
                "case {case} ({context}): expected no Location header on a refusal, got {:?}",
                answer.header("location")
            );
        }
    }
}

/// How many distributions the account holds.
async fn stored_count(router: &smithy_server_runtime::MultiServiceRouter) -> usize {
    list_distributions(router, "")
        .await
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default()
}

/// A create stores a configuration only where its `TenantConfig` is one CloudFront admits,
/// and an admitted document really is persisted.
#[tokio::test]
async fn a_create_admits_only_a_tenant_config_cloudfront_would_store() {
    let router = emulator();
    let mut stored = 0usize;

    for (index, case) in cases().into_iter().enumerate() {
        let reference = format!("tenant-config-{index}");
        let answer = create_distribution(&router, &case.document.xml(&reference)).await;
        assert_answer(&case.case, &answer, case.expect, "CreateDistribution", 201);

        if case.expect == Expect::Accepted {
            stored += 1;
            // The configuration the create stored is the one a later read answers with.
            let id = answer.root().text_at("Id").unwrap_or_default().to_string();
            let read = get_distribution(&router, &id).await;
            assert_eq!(
                read.status, 200,
                "case {}: expected the stored distribution to be readable, got {} with {}",
                case.case, read.status, read.raw
            );
            let config = read
                .root()
                .at("DistributionConfig")
                .unwrap_or_else(|| panic!("case {}: the read carries no configuration", case.case));
            assert_eq!(
                config.at("TenantConfig").is_some(),
                case.document.definitions.is_some(),
                "case {}: expected TenantConfig present={}, got {}",
                case.case,
                case.document.definitions.is_some(),
                read.raw
            );
            let origin = config
                .at("Origins/Items/Origin")
                .unwrap_or_else(|| panic!("case {}: the read carries no origin", case.case));
            assert_eq!(
                origin.text_at("DomainName"),
                Some(case.document.domain_name.as_str()),
                "case {}: expected the submitted DomainName, got {}",
                case.case,
                read.raw
            );
            if let Some(origin_path) = &case.document.origin_path {
                assert_eq!(
                    origin.text_at("OriginPath"),
                    Some(origin_path.as_str()),
                    "case {}: expected the submitted OriginPath, got {}",
                    case.case,
                    read.raw
                );
            }
        }

        // A refused create claims nothing.
        let count = stored_count(&router).await;
        assert_eq!(
            count, stored,
            "case {}: expected the account to hold {stored} distributions, got {count}",
            case.case
        );
    }
}

/// The same rules on the UPDATE path: a configuration a create would refuse is refused there
/// too, and the refusal leaves the stored configuration and its version exactly as they were.
///
/// Two subjects, because `ConnectionMode` is immutable: a row that declares the tenant-only
/// mode lands on the multi-tenant distribution, and every other row on the standard one.
#[tokio::test]
async fn an_update_admits_only_a_tenant_config_cloudfront_would_store() {
    let router = emulator();
    let mut tenant_subject = subject(&router, Document::default(), "tenant-config-update").await;
    let mut direct_subject = subject(
        &router,
        Document {
            connection_mode: Some(DIRECT),
            definitions: None,
            ..Document::default()
        },
        "tenant-config-update-direct",
    )
    .await;

    for case in cases() {
        let subject = if case.document.declares_tenant_only() {
            &mut tenant_subject
        } else {
            &mut direct_subject
        };
        // An update submits the configuration whole under the distribution's OWN caller
        // reference, which no update may rename.
        let document = case.document.xml(&subject.reference);
        let answer = update_distribution(&router, &subject.id, &document, Some(&subject.etag)).await;
        assert_answer(&case.case, &answer, case.expect, "UpdateDistribution", 200);
        if case.expect == Expect::Accepted {
            subject.etag = answer.header("etag").to_string();
            continue;
        }
        let read = get_distribution(&router, &subject.id).await;
        assert_eq!(
            read.header("etag"),
            subject.etag,
            "case {}: expected the refused update to leave the version at {:?}, got {:?}",
            case.case,
            subject.etag,
            read.header("etag")
        );
    }
}

/// One stored distribution an update row lands on.
struct Subject {
    id: String,
    etag: String,
    reference: String,
}

/// Create the distribution `document` describes and report it as an update subject.
async fn subject(
    router: &smithy_server_runtime::MultiServiceRouter,
    document: Document,
    reference: &str,
) -> Subject {
    let created = create_distribution(router, &document.xml(reference)).await;
    assert_eq!(
        created.status, 201,
        "case the {reference} subject: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    Subject {
        id: created.root().text_at("Id").unwrap_or_default().to_string(),
        etag: created.header("etag").to_string(),
        reference: reference.to_string(),
    }
}
