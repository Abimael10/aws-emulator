// SPDX-License-Identifier: CC0-1.0

//! The hold a tenant's resource associations put on the resource they name, end to end
//! over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — through the shared harness in the sibling
//! [`crate::configuration_sets`] suite, so routing, label decoding, the handler and
//! serialization are all exercised.
//!
//! The rule these cases pin: a resource a tenant still holds an association to cannot be
//! deleted — `CreateTenantResourceAssociation` binds the resource to the tenant and the
//! tenant goes on using it — so the delete of such a resource is refused with the modelled
//! 400 `BadRequestException` naming the resource's ARN, and the resource survives: its own
//! read still reports it. Once the last association is gone — removed by
//! `DeleteTenantResourceAssociation`, or dropped with the tenant that held it, whose
//! "associations with resources are removed" when it is deleted — the same delete is
//! accepted and the resource then reads back as absent.
//!
//! The tables below span what the rule generalises over: the KIND of resource the ARN
//! names (each `ResourceType` the model binds —
//! `smithy_query(query="shape",shape="ResourceType")`: `CONFIGURATION_SET`,
//! `EMAIL_IDENTITY`, `EMAIL_TEMPLATE`), both email-identity spellings, how many tenants
//! hold the resource, which way the last association goes away, and the resource's own
//! read before and after.

use serde_json::{json, Value};

use crate::configuration_sets::{Reply, Ses};
use crate::contact_lists::query_escaped;
use crate::tenant_resource_association_removal::Associations;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state;

/// The Region the shared harness signs its calls for, and so the Region the ARNs below
/// carry.
const REGION: &str = "us-east-1";

/// The subject of one row: the kind of resource the tenant holds, and the name SES holds
/// it under.
enum Subject {
    /// A configuration set, created with `CreateConfigurationSet`.
    ConfigurationSet(&'static str),
    /// An email identity — an address or a domain — created with `CreateEmailIdentity`.
    EmailIdentity(&'static str),
    /// An email template, created with `CreateEmailTemplate`.
    EmailTemplate(&'static str),
}

impl Subject {
    /// The ARN SES addresses this resource by, in the Region the call executes in: the
    /// resource forms this service mints (`crate::state::configuration_set_arn`,
    /// `identity_arn`, `email_template_arn`).
    fn arn(&self) -> String {
        match self {
            Self::ConfigurationSet(name) => state::configuration_set_arn(REGION, name),
            Self::EmailIdentity(identity) => state::identity_arn(REGION, identity),
            Self::EmailTemplate(name) => state::email_template_arn(REGION, name),
        }
    }

    /// Create the resource, failing loudly if the create is refused: a row is about the
    /// delete.
    async fn create(&self, ses: &Ses, case: &str) {
        let created = match self {
            Self::ConfigurationSet(name) => ses.create_configuration_set(name).await,
            Self::EmailIdentity(identity) => ses.create_email_identity(identity).await,
            Self::EmailTemplate(name) => {
                ses.call(
                    "POST",
                    "/v2/email/templates",
                    Some(json!({
                        "TemplateName": name,
                        "TemplateContent": {"Subject": "Hello", "Text": "Hi"},
                    })),
                )
                .await
            }
        };
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 creating the resource, got {} {:?}",
            created.status,
            created.message()
        );
    }

    /// Read the resource through its own getter.
    async fn read(&self, ses: &Ses) -> Reply {
        match self {
            Self::ConfigurationSet(name) => ses.get_set(name).await,
            Self::EmailIdentity(identity) => ses.get_email_identity(identity).await,
            Self::EmailTemplate(name) => {
                ses.call("GET", &format!("/v2/email/templates/{name}"), None)
                    .await
            }
        }
    }

    /// Delete the resource through its own delete operation.
    async fn delete(&self, ses: &Ses) -> Reply {
        let path = match self {
            Self::ConfigurationSet(name) => format!("/v2/email/configuration-sets/{name}"),
            // `EmailIdentity` is an `@httpLabel`, so the identity reaches the service
            // escaped, the way an SDK escapes a label.
            Self::EmailIdentity(identity) => {
                format!("/v2/email/identities/{}", query_escaped(identity))
            }
            Self::EmailTemplate(name) => format!("/v2/email/templates/{name}"),
        };
        ses.call("DELETE", &path, None).await
    }
}

/// How a row lets go of the associations holding its resource.
enum Release {
    /// `DeleteTenantResourceAssociation`, one tenant at a time: "the resource itself is
    /// not deleted, only its association with the specific tenant is removed".
    Disassociate,
    /// `DeleteTenant`: "when you delete a tenant, its associations with resources are
    /// removed, but the resources themselves are not deleted".
    DeleteTenant,
}

/// One row: the resource the tenants hold, which tenants hold it, and how the hold is
/// released.
struct HoldCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The resource the tenants are associated with.
    subject: Subject,
    /// The tenants holding it, each created and associated.
    tenants: &'static [&'static str],
    /// How the row releases the hold once the refusal is shown.
    release: Release,
}

/// The rows of the held-resource table: every resource kind a tenant may hold, both
/// email-identity spellings, resources held by one tenant and by several, and both ways
/// the last association goes away.
fn hold_cases() -> Vec<HoldCase> {
    vec![
        HoldCase {
            case: "a configuration set one tenant holds, released by disassociating",
            subject: Subject::ConfigurationSet("promo_set"),
            tenants: &["nightly-tenant"],
            release: Release::Disassociate,
        },
        HoldCase {
            case: "a configuration set three tenants hold, released by disassociating",
            subject: Subject::ConfigurationSet("shared-set-1"),
            tenants: &["tenant_a", "tenant_b", "tenant-c"],
            release: Release::Disassociate,
        },
        HoldCase {
            case: "a configuration set two tenants hold, released by deleting the tenants",
            subject: Subject::ConfigurationSet("tenant_deleted_set"),
            tenants: &["first-tenant", "second_tenant"],
            release: Release::DeleteTenant,
        },
        HoldCase {
            case: "an email identity that is a domain, released by disassociating",
            subject: Subject::EmailIdentity("example.com"),
            tenants: &["domain_tenant"],
            release: Release::Disassociate,
        },
        HoldCase {
            case: "an email identity that is an address held by two tenants, released by \
                   disassociating",
            subject: Subject::EmailIdentity("sender@example.com"),
            tenants: &["address-tenant", "other_tenant"],
            release: Release::Disassociate,
        },
        HoldCase {
            case: "an email identity that is a domain, released by deleting the tenant",
            subject: Subject::EmailIdentity("mail.example.org"),
            tenants: &["identity-tenant"],
            release: Release::DeleteTenant,
        },
        HoldCase {
            case: "an email template one tenant holds, released by deleting the tenant",
            subject: Subject::EmailTemplate("welcome"),
            tenants: &["template-tenant"],
            release: Release::DeleteTenant,
        },
        HoldCase {
            case: "an email template two tenants hold, released by deleting the tenants",
            subject: Subject::EmailTemplate("receipt_2"),
            tenants: &["billing-tenant", "ops_tenant"],
            release: Release::DeleteTenant,
        },
    ]
}

/// Create the row's resource and tenants, and associate every tenant with it. Answers the
/// resource's ARN.
async fn set_up(ses: &Ses, row: &HoldCase) -> String {
    let case = row.case;
    row.subject.create(ses, case).await;
    let arn = row.subject.arn();
    for tenant in row.tenants {
        let created = ses.create_tenant(tenant).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 creating tenant {tenant:?}, got {} {:?}",
            created.status,
            created.message()
        );
        let associated = ses.associate(tenant, &arn).await;
        assert_eq!(
            associated.status,
            200,
            "case {case:?}: expected HTTP 200 associating {tenant:?}, got {} {:?}",
            associated.status,
            associated.message()
        );
    }
    arn
}

/// Assert the row's delete is refused for the associations it still holds, and that the
/// resource survived the refusal.
async fn assert_delete_is_held_back(ses: &Ses, row: &HoldCase, arn: &str) {
    let case = row.case;
    let refused = row.subject.delete(ses).await;
    assert_eq!(
        refused.status,
        400,
        "case {case:?}: expected HTTP 400 deleting the held resource, got {} {:?}",
        refused.status,
        refused.message()
    );
    assert_eq!(
        refused.error_code(),
        "BadRequestException",
        "case {case:?}: expected BadRequestException, got {:?} {:?}",
        refused.error_code(),
        refused.message()
    );
    assert_eq!(
        refused.message(),
        state::resource_has_tenant_associations(arn),
        "case {case:?}: expected the refusal to name the resource's ARN, got {:?}",
        refused.message()
    );
    let read = row.subject.read(ses).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected the refused delete to leave the resource, got {} {:?}",
        read.status,
        read.message()
    );
}

/// Let go of one tenant's hold on the row's resource, the way the row releases it.
async fn release_one(ses: &Ses, row: &HoldCase, tenant: &str, arn: &str) {
    let case = row.case;
    let released = match row.release {
        Release::Disassociate => ses.disassociate(tenant, arn).await,
        Release::DeleteTenant => {
            ses.call(
                "POST",
                "/v2/email/tenants/delete",
                Some(json!({"TenantName": tenant})),
            )
            .await
        }
    };
    assert_eq!(
        released.status,
        200,
        "case {case:?}: expected HTTP 200 releasing {tenant:?}'s hold, got {} {:?}",
        released.status,
        released.message()
    );
}

#[tokio::test]
async fn a_resource_a_tenant_holds_cannot_be_deleted_until_the_last_association_goes() {
    for row in hold_cases() {
        let ses = Ses::new();
        let arn = set_up(&ses, &row).await;
        let case = row.case;

        // Every association still in place: the delete is refused and the resource stays.
        assert_delete_is_held_back(&ses, &row, &arn).await;

        // Letting go of all but the last hold does not release the resource either.
        let (last, rest) = row
            .tenants
            .split_last()
            .expect("every row holds at least one tenant");
        for tenant in rest {
            release_one(&ses, &row, tenant, &arn).await;
            assert_delete_is_held_back(&ses, &row, &arn).await;
        }

        // The last hold goes, and with it the hold on the resource.
        release_one(&ses, &row, last, &arn).await;
        let deleted = row.subject.delete(&ses).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case:?}: expected the delete to be accepted, got {} {:?}",
            deleted.status,
            deleted.message()
        );
        assert!(
            deleted.is_empty_body(),
            "case {case:?}: expected an empty response body, got {:?}",
            deleted.member("")
        );
        let read = row.subject.read(&ses).await;
        assert_eq!(
            (read.status, read.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: expected the deleted resource to read back as absent, got {} {:?}",
            read.status,
            read.message()
        );
    }
}

#[tokio::test]
async fn an_association_holds_back_only_the_resource_it_names() {
    // The gate is about the resource the ARN names, so a tenant holding one resource does
    // not hold back the delete of another — of the same kind or of any other.
    let case = "one held configuration set beside an unheld set, identity and template";
    let ses = Ses::new();
    let held = Subject::ConfigurationSet("held_set");
    let arn = set_up(
        &ses,
        &HoldCase {
            case,
            subject: Subject::ConfigurationSet("held_set"),
            tenants: &["holding-tenant"],
            release: Release::Disassociate,
        },
    )
    .await;

    for free in [
        Subject::ConfigurationSet("free_set"),
        Subject::EmailIdentity("free.example.com"),
        Subject::EmailTemplate("free-template"),
    ] {
        free.create(&ses, case).await;
        let deleted = free.delete(&ses).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case:?}: expected the unheld resource {} to be deletable, got {} {:?}",
            free.arn(),
            deleted.status,
            deleted.message()
        );
    }

    let refused = held.delete(&ses).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "BadRequestException"),
        "case {case:?}: expected the held set to stay held, got {} {:?}",
        refused.status,
        refused.message()
    );
    assert_eq!(
        refused.message(),
        state::resource_has_tenant_associations(&arn),
        "case {case:?}: expected the refusal to name the held set's ARN, got {:?}",
        refused.message()
    );
    let read = held.read(&ses).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected the held set to survive, got {} {:?}",
        read.status,
        read.message()
    );
    assert_eq!(
        read.member("ConfigurationSetName"),
        Value::String("held_set".to_string()),
        "case {case:?}: expected the held set to read back under its own name, got {:?}",
        read.member("ConfigurationSetName")
    );
}
