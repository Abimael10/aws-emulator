// SPDX-License-Identifier: CC0-1.0

//! The published blueprint catalog: the operating systems and applications an
//! instance can be created from, and the rules a request's blueprint is resolved
//! against.

use super::*;

/// The default user name for connecting to an instance of each blueprint family,
/// from the Lightsail user guide's "Connecting to your instance" tables: the
/// operating-system blueprints connect as `ec2-user` except Debian (`admin`) and
/// Ubuntu (`ubuntu`), Windows connects as `Administrator`, and every
/// Bitnami-packaged application blueprint connects as `bitnami`.
///
/// Matching is by longest id prefix. A blueprint family that is not listed
/// reports NO `username`: the member is optional, and guessing one would be worse
/// than saying nothing.
pub(super) const BLUEPRINT_USERNAMES: &[(&str, &str)] = &[
    ("almalinux", "ec2-user"),
    ("amazon_linux", "ec2-user"),
    ("centos", "ec2-user"),
    ("debian", "admin"),
    ("freebsd", "ec2-user"),
    ("opensuse", "ec2-user"),
    ("ubuntu", "ubuntu"),
    ("windows", "Administrator"),
    ("django", "bitnami"),
    ("drupal", "bitnami"),
    ("ghost", "bitnami"),
    ("gitlab", "bitnami"),
    ("joomla", "bitnami"),
    ("lamp", "bitnami"),
    ("magento", "bitnami"),
    ("mean", "bitnami"),
    ("nginx", "bitnami"),
    ("nodejs", "bitnami"),
    ("prestashop", "bitnami"),
    ("redmine", "bitnami"),
    ("wordpress", "bitnami"),
];

/// The user name an instance of `blueprint_id` connects as, or `None` when the
/// blueprint is not one of the families in [`BLUEPRINT_USERNAMES`].
pub fn blueprint_username(blueprint_id: &str) -> Option<&'static str> {
    BLUEPRINT_USERNAMES
        .iter()
        .filter(|(prefix, _)| blueprint_id.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, username)| *username)
}

/// The friendly name of the blueprint `blueprint_id`, as `Instance$blueprintName`
/// reports it for an instance created from it: the name the catalog publishes for
/// the image ([`BlueprintSpec::name`]) when this service publishes it, and the
/// id's words title-cased ([`blueprint_name`]) for a blueprint the catalog does
/// not hold — one name per blueprint, wherever it is reported.
pub fn blueprint_display_name(blueprint_id: &str) -> String {
    blueprint_spec(blueprint_id)
        .map(|spec| spec.name())
        .unwrap_or_else(|| blueprint_name(blueprint_id))
}

/// Whether `blueprint_id` is a Windows blueprint. Every Lightsail Windows
/// blueprint id begins `windows_` (`windows_server_2022`); every other blueprint
/// is a Linux/Unix one.
pub fn blueprint_is_windows(blueprint_id: &str) -> bool {
    blueprint_id.starts_with("windows")
}

/// The friendly blueprint name, rendered from the blueprint id by title-casing
/// its underscore-separated words.
///
/// That is the derivation the modelled `Instance$blueprintName` documentation
/// shows: blueprint id `amazon_linux_2023` is reported as `Amazon Linux 2023`.
pub fn blueprint_name(blueprint_id: &str) -> String {
    blueprint_id
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One entry of the instance blueprint catalog: a virtual private server image
/// an instance can be created from.
///
/// This is a PUBLISHED catalog, not per-account state — every account sees the
/// same entries — which is why it is a `const` table rather than a field on the
/// store, exactly like the bundle and Region catalogs above.
#[derive(Debug, Clone, Copy)]
pub struct BlueprintSpec {
    /// `Blueprint$blueprintId`: the id a `CreateInstances` request names
    /// (`amazon_linux_2023`, `wordpress`).
    pub blueprint_id: &'static str,
    /// `Blueprint$name` where the catalog's own name for the image is the name of
    /// the PRODUCT it installs rather than the words of its id (`SQL Server 2022
    /// Express` for `windows_server_2022_sql_2022_express`), and `None` where the
    /// catalog's name is the id's words title-cased — the derivation
    /// `Instance$blueprintName` reports ([`blueprint_name`]).
    pub published_name: Option<&'static str>,
    /// `Blueprint$minPower` where the image requires a LARGER plan than its
    /// platform's own floor, and `None` where the floor is the platform's
    /// ([`BlueprintSpec::min_power`]).
    ///
    /// The minimum is the image's own requirement, which is why the catalog
    /// publishes it per entry: an application image that installs a database
    /// engine beside the operating system does not run on the plans the bare
    /// operating system runs on.
    pub published_min_power: Option<i32>,
    /// `Blueprint$group` — "The group name of the blueprint (`amazon-linux`)".
    ///
    /// The group is PUBLISHED per entry rather than derived from the id: the
    /// catalog groups an image family under the name the service gives that
    /// family, which is not always the id's own words — the recorded
    /// `windows_server_2025` entry is grouped `windows_2025`, where its family
    /// carries the release and drops the `server` word.
    pub group: &'static str,
    /// `Blueprint$type`, one of the modelled `BlueprintType` enum's two values
    /// (`smithy_query(query="shape",shape="BlueprintType")`): `os` for an
    /// operating-system image, `app` for an application stack.
    pub blueprint_type: &'static str,
    /// `Blueprint$isActive`. From that member's own documentation: "Inactive
    /// blueprints are listed to support customers with existing instances but
    /// are not necessarily available for launch of new instances" — and
    /// `CreateInstances$blueprintId` says "Use active blueprints when creating
    /// new instances", so only an active entry can be created from.
    pub is_active: bool,
    /// `Blueprint$description` — "The description of the blueprint": the catalog
    /// text this service publishes for the image, carried PER ENTRY.
    ///
    /// It is the image's own catalog prose — what the image is and what it is
    /// for — not a sentence derived from the entry's id, name or `type`: those
    /// say nothing a caller could not already read off the other members, while
    /// a client that renders this text to a user is showing it in place of the
    /// catalog's.
    ///
    /// The column carries AWS's OWN catalog copy for the image wherever this
    /// catalog knows it — the Windows family's paragraph, which names the release
    /// the image installs, and the Amazon Linux images' — and carries it WHOLE:
    /// AWS's paragraph with a clause of it dropped is no longer the catalog's
    /// text, so a recorded paragraph is reproduced sentence for sentence. Where
    /// AWS's copy is not known this catalog's own prose about that image stands,
    /// since AWS publishes no machine-readable index of these paragraphs. What the
    /// column owes is the published prose about THAT image, which is what is
    /// asserted of it.
    pub description: &'static str,
    /// `Blueprint$version` — "The version number of the operating system,
    /// application, or stack (`2016.03.0`)" — of THIS image.
    ///
    /// Carried PER ENTRY, because that is what the member names: the release of
    /// the software the image installs. It is not one figure shared by the
    /// listing — two entries of one listing differ wherever the software they
    /// install does, which is why an image of an operating-system release reports
    /// that release while an image rebuilt on a schedule reports its build.
    ///
    /// Which string an entry carries is what the catalog publishes for it: an
    /// operating system whose release the catalog names carries that release
    /// (`24.04`, `12`), an Amazon Linux image carries the release string Amazon
    /// Linux publishes for it (`2023.<minor>.<build>.<revision>`), and an image
    /// the catalog publishes by BUILD DATE — the Windows family and the
    /// preinstalled application stacks — carries that date
    /// ([`BLUEPRINT_IMAGE_BUILD_VERSION`]).
    pub version: &'static str,
    /// The page AWS publishes about this image, as a PATH under
    /// [`AWS_PRODUCT_PAGE_URL_PREFIX`] — what [`BlueprintSpec::product_url`]
    /// builds `Blueprint$productUrl` from — or empty where the catalog publishes
    /// no page for the entry and the member is reported absent.
    ///
    /// `Blueprint$productUrl` is "The product URL to learn more about the image
    /// or blueprint", and the page is one AWS serves rather than the page the
    /// image's upstream vendor keeps: what the link has to describe is the image
    /// as it is offered HERE, including the terms the next member names. Which
    /// AWS page that is depends on the image: one offered through an AWS
    /// Marketplace listing links that listing ([`MARKETPLACE_PRODUCT_VIEW_PATH`],
    /// at the `prodview-<id>` product view or the `<product-code>` a recorded
    /// answer reports for an older listing), while an image AWS publishes a
    /// product page of its own for — its own operating systems — links that page.
    ///
    /// The HOST and the path are the published ones. The path is the recorded one
    /// on the rows a recorded answer names it for, and CHOSEN on the rest, since
    /// AWS publishes no index of its listings.
    pub product_page: &'static str,
    /// Where AWS publishes the end-user licence agreement for this image, which
    /// [`BlueprintSpec::license_url`] builds `Blueprint$licenseUrl` from.
    ///
    /// `Blueprint$licenseUrl` is "The end-user license agreement URL for the
    /// image or blueprint". The agreement is the one accepted by launching the
    /// image HERE, so the link leads to AWS's own copy of it — and it is
    /// published for every image the catalog offers, an openly licensed
    /// operating system included: a recorded answer carries one on an Amazon
    /// Linux entry, whose software is offered under its own open terms. That is
    /// why every entry of [`BLUEPRINTS`] publishes one.
    ///
    /// WHICH form the link takes is the catalog's, and both forms a recorded
    /// answer reports are [`BlueprintLicense`]'s: an agreement document on AWS's
    /// licence host, or the usage terms carried on the image's own Marketplace
    /// listing. As with [`BlueprintSpec::product_page`], the identifiers in an
    /// agreement path are the recorded ones on the rows a recorded answer names
    /// them for and CHOSEN on the rest.
    pub license: BlueprintLicense,
}

/// Where AWS publishes the end-user licence agreement of one catalog entry.
///
/// A recorded `GetBlueprints` answer reports two forms, and which one an entry
/// carries is what AWS publishes for that image: an image whose agreement AWS
/// serves as a document of its own links that document on the licence host
/// ([`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`]), while an image offered through a
/// Marketplace listing that carries its terms links the usage section of THAT
/// listing ([`MARKETPLACE_USAGE_TERMS_FRAGMENT`]).
///
/// Either way the link is the ENTRY's own: it leads to the agreement accepted by
/// launching this image, never to another image's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlueprintLicense {
    /// AWS publishes no agreement for the image, so the optional member is
    /// reported absent.
    NotPublished,
    /// The `<product>/<agreement>.txt` document AWS serves under
    /// [`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`], named by the path it is served
    /// at.
    Agreement(&'static str),
    /// The usage terms published on the image's own Marketplace listing: the
    /// entry's [`BlueprintSpec::product_page`] with
    /// [`MARKETPLACE_USAGE_TERMS_FRAGMENT`].
    ListingUsageTerms,
}

/// One value of the modelled `BlueprintType` enum: an operating-system image.
pub const BLUEPRINT_TYPE_OS: &str = "os";

/// One value of the modelled `BlueprintType` enum: an application stack.
pub const BLUEPRINT_TYPE_APP: &str = "app";

/// Where this service publishes what a blueprint's `productUrl` leads to: the
/// AWS site, which `Blueprint$productUrl` is the entry's page path
/// ([`BlueprintSpec::product_page`]) appended to.
///
/// Every entry's product link is under this one host: the catalog points a caller
/// at the page AWS serves about the image, not at the page the image's upstream
/// vendor keeps about the software.
pub const AWS_PRODUCT_PAGE_URL_PREFIX: &str = "https://aws.amazon.com/";

/// The path an image offered through an AWS Marketplace listing is published at,
/// which its [`BlueprintSpec::product_page`] opens with: the listing's
/// `prodview-<id>` product view under this path of the AWS site.
///
/// An image AWS publishes a product page of its own for carries that page's path
/// instead, which is why the Marketplace path is a prefix of SOME entries rather
/// than of the whole column.
pub const MARKETPLACE_PRODUCT_VIEW_PATH: &str = "marketplace/pp/";

/// Where this service publishes a blueprint's end-user licence agreement as a
/// document of its own: the AWS licence host the agreement path of
/// [`BlueprintLicense::Agreement`] is appended to.
///
/// The agreement a caller accepts by launching an image here is the one AWS
/// serves for it, which is why an agreement document is served from this host
/// rather than from the upstream vendor's licensing pages.
pub const BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX: &str =
    "https://d7umqicpi7263.cloudfront.net/eula/product/";

/// The section of an AWS Marketplace listing that carries the image's usage
/// terms, which `Blueprint$licenseUrl` names where AWS publishes those terms on
/// the listing itself rather than as an agreement document of its own.
///
/// SOURCE: the recorded `GetBlueprints` answer, which reports a Windows SQL
/// Server application image's licence as that image's own Marketplace listing
/// with this fragment.
pub const MARKETPLACE_USAGE_TERMS_FRAGMENT: &str = "#pdp-usage";

/// The end-user licence agreement AWS publishes for the Windows Server
/// operating-system images, as the `<product>/<agreement>.txt` path it is served
/// at under [`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`].
///
/// SOURCE: the recorded `GetBlueprints` answer, which reports this one agreement
/// on more than one Windows Server entry: the agreement is the Windows Server
/// FAMILY's — one document covering the releases of it this catalog offers — so
/// every release links that document rather than one agreement per row.
pub const WINDOWS_SERVER_LICENSE_AGREEMENT: &str =
    "b0e459f5-2c38-4939-ad2a-d9784ecab690/0e7c7387-f24a-45b5-9206-e5ab5ff5fd9c.txt";

/// `Blueprint$versionCode` — "The version code" — of every entry of
/// [`BLUEPRINTS`]: Lightsail's own revision counter for the image it publishes
/// at that version, which starts at the first revision and is the value the
/// recorded entries carry.
pub const BLUEPRINT_VERSION_CODE: &str = "1";

/// `Blueprint$version` of an image this catalog publishes by BUILD DATE rather
/// than by the release of the software it installs: the date that build was
/// published, in the `YYYY.MM.DD` form the member's own example (`2016.03.0`)
/// shows.
///
/// The Windows family and the preinstalled application stacks are rebuilt as a
/// set and are published by that build's date, which is why a recorded answer
/// reports one and the same version on a Windows Server operating-system image
/// and on a Windows SQL Server application image. An operating system whose own
/// release the catalog names carries that release instead — a recorded Amazon
/// Linux entry carries the Amazon Linux release string — so this is the version
/// of SOME entries, never of the whole listing (see [`BlueprintSpec::version`]).
pub const BLUEPRINT_IMAGE_BUILD_VERSION: &str = "2026.09.17";

/// `Blueprint$minPower` of an application image that installs the Windows SQL
/// Server database engine beside the operating system: it needs the 4 GB plan,
/// so its floor is that plan's [`BundleSpec::power`].
pub const WINDOWS_SQL_BLUEPRINT_MIN_POWER: i32 = 2_000;

/// `Blueprint$minPower` of a Windows image — "The minimum bundle power required
/// to run this blueprint".
///
/// A Windows image does not run on the smallest instance plan, so its floor is
/// the next plan's power: the `micro` plan's [`BundleSpec::power`], which is the
/// value the recorded Windows entries report. A Linux/Unix image this catalog
/// publishes runs on every plan, which `0` states — "`0` indicates that the
/// blueprint runs on all instance sizes".
pub const WINDOWS_BLUEPRINT_MIN_POWER: i32 = 500;

/// `Blueprint$minPower` of an image that runs on every plan this service
/// publishes. See [`WINDOWS_BLUEPRINT_MIN_POWER`].
pub const BLUEPRINT_MIN_POWER_ANY_SIZE: i32 = 0;

/// The instance blueprints this service publishes — the catalog `GetBlueprints`
/// reports and `CreateInstances` resolves `blueprintId` against.
///
/// SOURCE: the blueprint ids Lightsail's instance-image list publishes (the
/// Lightsail console's "Select a blueprint" catalog, which `GetBlueprints`
/// answers from): the operating-system families `windows_server`,
/// `amazon_linux`, `ubuntu`, `debian`, `almalinux`, `centos_stream`, `freebsd`
/// and `opensuse`, each at the releases Lightsail offers, the Windows SQL Server
/// application images, plus the Bitnami-packaged and hosting-panel application
/// stacks. An entry whose upstream release has reached end of life stays in the
/// catalog but is marked inactive, which is the state Lightsail keeps a retired
/// image in so existing instances still resolve it while a new instance cannot
/// be launched from it.
///
/// Each entry carries its OWN catalog text, version and links: the description
/// AWS publishes for the image — the paragraph in full, since an abridged one is
/// no longer the catalog's text — the release of the software it installs, the
/// path of the AWS page the image is described at and the agreement AWS publishes
/// for it (see [`BlueprintSpec::description`], [`BlueprintSpec::version`],
/// [`BlueprintSpec::product_page`] and [`BlueprintSpec::license`], and
/// [`AWS_PRODUCT_PAGE_URL_PREFIX`] / [`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`]
/// for the URL forms). Where a recorded answer quotes a row's paragraph, version,
/// page path or agreement, that row carries the recorded value; the other rows'
/// text and paths are this catalog's own, as each of those members records.
///
/// The ORDER is the catalog's published one, which `GetBlueprints` walks: the
/// Windows images first — the operating-system releases newest first, then the
/// application images that preinstall a SQL Server edition on one — then the
/// Linux/Unix operating systems and last the application stacks. That is the
/// order the recorded answer reports, whose first two entries are
/// `windows_server_2025` and `windows_server_2022` and whose Windows SQL entries
/// precede every Linux/Unix one.
pub const BLUEPRINTS: &[BlueprintSpec] = &[
    // Windows Server operating systems, newest release first.
    BlueprintSpec {
        blueprint_id: "windows_server_2025",
        published_name: None,
        published_min_power: None,
        group: "windows_2025",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Lightsail helps you build, deploy, scale, and manage Microsoft \
                      applications quickly, easily, and cost effectively with Windows Server \
                      2025. For business IT applications, Lightsail runs \
                      Windows-based solutions in a secure, easily managed, and performant \
                      cloud environment. Common Windows use cases include \
                      Enterprise Windows-based application hosting, website and web-service \
                      hosting, data processing, distributed testing, ASP.NET application \
                      hosting, and running any other application requiring Windows software.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-g76csvka4r3by",
        license: BlueprintLicense::Agreement(WINDOWS_SERVER_LICENSE_AGREEMENT),
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2022",
        published_name: None,
        published_min_power: None,
        group: "windows_2022",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Lightsail helps you build, deploy, scale, and manage Microsoft \
                      applications quickly, easily, and cost effectively with Windows Server \
                      2022. For business IT applications, Lightsail runs \
                      Windows-based solutions in a secure, easily managed, and performant \
                      cloud environment. Common Windows use cases include \
                      Enterprise Windows-based application hosting, website and web-service \
                      hosting, data processing, distributed testing, ASP.NET application \
                      hosting, and running any other application requiring Windows software.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        // The recorded answer reports this release's listing and the Windows
        // Server family's one agreement.
        product_page: "marketplace/pp/prodview-dq4sxno5vuy7m",
        license: BlueprintLicense::Agreement(WINDOWS_SERVER_LICENSE_AGREEMENT),
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2019",
        published_name: None,
        published_min_power: None,
        group: "windows_2019",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Lightsail helps you build, deploy, scale, and manage Microsoft \
                      applications quickly, easily, and cost effectively with Windows Server \
                      2019. For business IT applications, Lightsail runs \
                      Windows-based solutions in a secure, easily managed, and performant \
                      cloud environment. Common Windows use cases include \
                      Enterprise Windows-based application hosting, website and web-service \
                      hosting, data processing, distributed testing, ASP.NET application \
                      hosting, and running any other application requiring Windows software.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        // A listing AWS publishes at its Marketplace PRODUCT CODE rather than a
        // `prodview-` product view: the page path is the listing's own, so the
        // column carries whichever form AWS serves for the image.
        product_page: "marketplace/pp/B07QZ4XZ8F",
        license: BlueprintLicense::Agreement(WINDOWS_SERVER_LICENSE_AGREEMENT),
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2016",
        published_name: None,
        published_min_power: None,
        group: "windows_2016",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Lightsail helps you build, deploy, scale, and manage Microsoft \
                      applications quickly, easily, and cost effectively with Windows Server \
                      2016. For business IT applications, Lightsail runs \
                      Windows-based solutions in a secure, easily managed, and performant \
                      cloud environment. Common Windows use cases include \
                      Enterprise Windows-based application hosting, website and web-service \
                      hosting, data processing, distributed testing, ASP.NET application \
                      hosting, and running any other application requiring Windows software.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/B01M7SJEU7",
        license: BlueprintLicense::Agreement(WINDOWS_SERVER_LICENSE_AGREEMENT),
    },
    // The Windows SQL Server application images: an edition of the database
    // engine preinstalled on a Windows Server image, which the catalog publishes
    // after the bare Windows images and before the first Linux/Unix one.
    BlueprintSpec {
        blueprint_id: "windows_server_2022_sql_2022_express",
        published_name: Some("SQL Server 2022 Express"),
        published_min_power: Some(WINDOWS_SQL_BLUEPRINT_MIN_POWER),
        group: "windows_2022_sql_exp_2022",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Microsoft SQL Server 2022 Express Edition, preinstalled on Windows \
                      Server 2022. SQL Server Express is the free, entry-level edition of the \
                      database engine, for small databases and for developing and testing \
                      data-driven applications, and this image runs a Windows application and \
                      its database on a single Lightsail instance. Learn more about Microsoft \
                      SQL Server 2022 Express on the AWS Marketplace.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-c2jz4lr4h2yc6",
        // AWS publishes this image's terms on the listing itself, so the licence
        // link is that listing's usage section.
        license: BlueprintLicense::ListingUsageTerms,
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2022_sql_2019_express",
        published_name: Some("SQL Server 2019 Express"),
        published_min_power: Some(WINDOWS_SQL_BLUEPRINT_MIN_POWER),
        group: "windows_2022_sql_exp_2019",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Microsoft SQL Server 2019 Express Edition, preinstalled on Windows \
                      Server 2022. SQL Server Express is the free, entry-level edition of the \
                      database engine, and this image carries the 2019 release for an \
                      application qualified against it, with the application and its database \
                      on a single Lightsail instance. Learn more about Microsoft SQL Server \
                      2019 Express on the AWS Marketplace.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-xbikutlmywslu",
        license: BlueprintLicense::ListingUsageTerms,
    },
    // Linux/Unix operating systems.
    BlueprintSpec {
        blueprint_id: "amazon_linux_2023",
        published_name: None,
        published_min_power: None,
        group: "amazon_linux_2023",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Linux 2023 is a modern, general purpose Linux-based OS that comes \
                      with 5 years of long term support. It is optimized for AWS and designed \
                      to provide a secure, stable and high-performance execution environment to \
                      develop and run your cloud applications.",
        version: "2023.12.20260930.0",
        product_page: "linux/amazon-linux-2023",
        license: BlueprintLicense::Agreement("f37c8255-1ff9-48bd-b5da-b5046f4fee68/\
                                              17864b80-f5b6-4f0a-aa26-7346ef135ebc.txt"),
    },
    BlueprintSpec {
        blueprint_id: "amazon_linux_2",
        published_name: None,
        published_min_power: None,
        group: "amazon_linux_2",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Linux 2 is the previous generation Amazon Linux operating system \
                      that provides high performance, stability and security for your cloud \
                      workloads. With Amazon Linux 2, you get an application environment with \
                      long term support and the AWS CLI, cloud-init and the AWS SDKs \
                      preinstalled.",
        version: "2.0.20260930.0",
        product_page: "amazon-linux-2",
        license: BlueprintLicense::Agreement("c4d8e2a1-3f57-4b60-9d82-16ea70cb4f93/\
                                              5a71b0c6-9d34-4e18-87af-20cb63de5917.txt"),
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_24_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu_24_04",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 24.04 LTS from Canonical, a long-term support release \
                      maintained with security updates for five years. Its large package \
                      archive and current language runtimes make it a general-purpose base for \
                      web applications, APIs and development environments on a Lightsail \
                      instance.",
        version: "24.04",
        product_page: "marketplace/pp/prodview-2jmwz6bqxkd4t",
        license: BlueprintLicense::Agreement("e81c5a3f-24d9-4b76-95e0-7ad31cf6082b/\
                                              3b92d4e7-50a1-4c68-8f73-19ca05be7d42.txt"),
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_22_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu_22_04",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 22.04 LTS from Canonical, a long-term support release \
                      maintained with security updates for five years. Choose it where an \
                      application is already qualified against the 22.04 package set and its \
                      language runtimes.",
        version: "22.04",
        product_page: "marketplace/pp/prodview-kd8vqz3mwbtx5",
        license: BlueprintLicense::Agreement("7a24f9b1-6c80-4d35-92ea-58bd03cf1e76/\
                                              9d01c7a5-2b63-4f94-85d8-47ea16bc0938.txt"),
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_20_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu_20_04",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 20.04 LTS from Canonical, carried for applications pinned \
                      to its package set and runtimes. Newer long-term support releases are \
                      published in this catalog for a new deployment.",
        version: "20.04",
        product_page: "marketplace/pp/prodview-9wmtb4kzqvxd2",
        license: BlueprintLicense::Agreement("2f6b81d4-9a07-4e52-83c6-71da45bf0e29/\
                                              6c38a2f9-0d71-4b85-94e3-52ac18db6704.txt"),
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_18_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu_18_04",
        blueprint_type: BLUEPRINT_TYPE_OS,
        // Ubuntu 18.04 LTS is past its upstream end of standard support.
        is_active: false,
        description: "Ubuntu Server 18.04 LTS from Canonical. The release is past the end of \
                      its standard support period and is listed so an instance already running \
                      it still resolves its image; choose a current long-term support release \
                      for a new instance.",
        version: "18.04",
        product_page: "marketplace/pp/prodview-bq6zktmwx3vd8",
        license: BlueprintLicense::Agreement("5d09c4b7-1e38-4a96-87f2-40cb92de5107/\
                                              8a67f3d2-5c94-4e01-93b8-16de70ca4f85.txt"),
    },
    BlueprintSpec {
        blueprint_id: "debian_12",
        published_name: None,
        published_min_power: None,
        group: "debian_12",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Debian 12 is the stable release of the Debian GNU/Linux distribution, \
                      maintained by the Debian project with security updates. Its conservative \
                      package set and small default install make it a predictable base for \
                      servers and self-managed application stacks.",
        version: "12",
        product_page: "marketplace/pp/prodview-x4ktqzm9wbvd3",
        license: BlueprintLicense::Agreement("9b45e0c8-7d12-4f63-95a7-28cb41df6e03/\
                                              4e18a7b3-0f52-4c96-8d71-63ca29be5017.txt"),
    },
    BlueprintSpec {
        blueprint_id: "debian_11",
        published_name: None,
        published_min_power: None,
        group: "debian_11",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Debian 11 is carried as the previous Debian stable release, for \
                      applications pinned to its package set and library versions, and is \
                      maintained with long-term security updates by the Debian project.",
        version: "11",
        product_page: "marketplace/pp/prodview-m2wvqz7ktbxd5",
        license: BlueprintLicense::Agreement("3c71d9a6-4b58-4e02-96f8-15da87cb0e34/\
                                              7f02b6d1-8c43-4a95-92e7-50bd31ca6f08.txt"),
    },
    BlueprintSpec {
        blueprint_id: "debian_10",
        published_name: None,
        published_min_power: None,
        group: "debian_10",
        blueprint_type: BLUEPRINT_TYPE_OS,
        // Debian 10 is past its upstream end of life.
        is_active: false,
        description: "Debian 10 has reached the end of its life upstream and is listed so an \
                      instance already running it still resolves its image; choose a current \
                      Debian stable release for a new instance.",
        version: "10",
        product_page: "marketplace/pp/prodview-7bkzqvm4twxd9",
        license: BlueprintLicense::Agreement("6e83f1c4-2d97-4b50-85a9-71cb04de3f26/\
                                              1a54c8b7-6f03-4d92-97e1-28da60bc5917.txt"),
    },
    BlueprintSpec {
        blueprint_id: "almalinux_9",
        published_name: None,
        published_min_power: None,
        group: "almalinux_9",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "AlmaLinux OS 9 is a community-owned, production-grade Linux \
                      distribution that is binary compatible with Red Hat Enterprise Linux 9. \
                      It is a drop-in base for RPM-based enterprise workloads, with security \
                      and maintenance updates from the AlmaLinux OS Foundation.",
        version: "9",
        product_page: "marketplace/pp/prodview-vz3mqkt8wbxd6",
        license: BlueprintLicense::Agreement("8f27b3d0-5a61-4c94-93e2-07da18cb6f45/\
                                              2d60a9f4-7b18-4e53-86c1-49bd03ca7e26.txt"),
    },
    BlueprintSpec {
        blueprint_id: "centos_stream_9",
        published_name: None,
        published_min_power: None,
        group: "centos_stream_9",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "CentOS Stream 9 is the continuously delivered distribution that tracks \
                      just ahead of Red Hat Enterprise Linux 9, for developers who build and \
                      test RPM-based software against the next minor release.",
        version: "9",
        product_page: "marketplace/pp/prodview-q8wmzkt2vbxd4",
        license: BlueprintLicense::Agreement("4a96c7f2-0b53-4d18-95e7-62da31bc0948/\
                                              9e13b5a8-4c70-4f26-83d9-50ca17de6b02.txt"),
    },
    BlueprintSpec {
        blueprint_id: "freebsd_14",
        published_name: None,
        published_min_power: None,
        group: "freebsd_14",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "FreeBSD 14 is an advanced open-source operating system from the FreeBSD \
                      Project, with the ZFS file system, jails and the pkg package collection. \
                      Choose it for network services and storage workloads that rely on the \
                      FreeBSD stack.",
        version: "14",
        product_page: "marketplace/pp/prodview-t5zqmkw9vbxd7",
        license: BlueprintLicense::Agreement("1b58d0a7-9f42-4e63-87c5-30da26bc5917/\
                                              6f94c2b1-3d07-4a85-92e6-18db40ca7f23.txt"),
    },
    BlueprintSpec {
        blueprint_id: "opensuse_15",
        published_name: None,
        published_min_power: None,
        group: "opensuse_15",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "openSUSE Leap 15 is a stable Linux distribution built from SUSE Linux \
                      Enterprise sources, with the YaST administration tools and the zypper \
                      package manager, for administrators who want an enterprise-derived base \
                      under community maintenance.",
        version: "15",
        product_page: "marketplace/pp/prodview-z9qmkt3wvbxd8",
        license: BlueprintLicense::Agreement("7d02a9c5-6b31-4f84-93e7-45ca18bd0e26/\
                                              3a71f6d9-0c54-4b92-85d8-27de61ca4f03.txt"),
    },
    // Application stacks.
    BlueprintSpec {
        blueprint_id: "wordpress",
        published_name: None,
        published_min_power: None,
        group: "wordpress",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "WordPress packaged by Bitnami is the most widely used content \
                      management and blogging platform on the web, and this image has it \
                      serving as soon as the instance starts. The stack installs WordPress on \
                      Apache, PHP and MySQL, with the Bitnami tooling that keeps the \
                      components up to date.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-c3thaabbmnmfy",
        license: BlueprintLicense::Agreement("2a7f85c1-4b63-4d09-9e87-06c15df2b3a8/\
                                              4d19c7e6-85b2-4f71-9a03-27be6cd81f54.txt"),
    },
    BlueprintSpec {
        blueprint_id: "wordpress_multisite",
        published_name: None,
        published_min_power: None,
        group: "wordpress-multisite",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "WordPress Multisite packaged by Bitnami runs a network of WordPress \
                      sites from one installation, with a single dashboard for themes, plugins \
                      and users. The image installs WordPress with the multisite network \
                      enabled on Apache, PHP and MySQL.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-g4jkpwqz2mvxb",
        license: BlueprintLicense::Agreement("6b3d09f5-7c12-4a84-95e0-3f87ba6d41c2/\
                                              8f27a1b4-0d96-4e53-87ca-5b19de402f76.txt"),
    },
    BlueprintSpec {
        blueprint_id: "lamp_8_bitnami",
        published_name: None,
        published_min_power: None,
        group: "lamp-bitnami",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "LAMP packaged by Bitnami is a PHP 8 development and hosting stack: \
                      Apache, MySQL and PHP with the extensions, Composer and command-line \
                      tools a PHP application expects, ready to take your own code.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-n7qzmkw4tvbx9",
        license: BlueprintLicense::Agreement("0c54e8a7-3b19-4f62-8d05-9ae71c24b6f3/\
                                              2b96d7f0-5c84-4a19-93e7-61fd08ac527b.txt"),
    },
    BlueprintSpec {
        blueprint_id: "nodejs",
        published_name: None,
        published_min_power: None,
        group: "nodejs",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Node.js packaged by Bitnami is a production-ready Node.js runtime with \
                      npm, Apache as a reverse proxy and MongoDB, so a JavaScript application \
                      can be deployed to a Lightsail instance without assembling the stack \
                      first.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-d2kmqzw8tvbx5",
        license: BlueprintLicense::Agreement("7e28b1d4-6f05-4c93-85a1-2d9b70fe4a86/\
                                              9a51c3e8-7b26-4d80-96f4-38ce15b7024d.txt"),
    },
    BlueprintSpec {
        blueprint_id: "joomla",
        published_name: None,
        published_min_power: None,
        group: "joomla",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Joomla! packaged by Bitnami is an open-source content management system \
                      for building web sites and online applications, preinstalled on Apache, \
                      PHP and MySQL and configured to serve as soon as the instance starts.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-j6wzqmk3tvbx7",
        license: BlueprintLicense::Agreement("4f91a6c3-0e78-4b25-9d6f-83a21cb7e504/\
                                              6c38f9a1-2d47-4e96-8b50-71de94a2c3f8.txt"),
    },
    BlueprintSpec {
        blueprint_id: "drupal",
        published_name: None,
        published_min_power: None,
        group: "drupal",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Drupal packaged by Bitnami is an open-source content management \
                      platform for content-rich sites, with a module ecosystem for structured \
                      content and multilingual publishing, preinstalled on Apache, PHP and \
                      MySQL.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-r8kzqmw2tvbx4",
        license: BlueprintLicense::Agreement("1d86c4f9-5a03-4e71-92b8-4f7c06ad3e25/\
                                              3f70b2d5-8c91-4a67-85de-029ab6f41c73.txt"),
    },
    BlueprintSpec {
        blueprint_id: "magento",
        published_name: None,
        published_min_power: None,
        group: "magento",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Magento packaged by Bitnami is an open-source e-commerce platform for \
                      catalogues, carts and checkout, preinstalled with Apache, PHP, MySQL and \
                      Elasticsearch and configured for the catalogue indexing a store needs.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-y3mzqkw7tvbx6",
        license: BlueprintLicense::Agreement("8a23f7b6-1c94-4d58-90e2-6b5d31ca847f/\
                                              5d81e6c0-4a37-4b92-87f1-93ce20b74da6.txt"),
    },
    BlueprintSpec {
        blueprint_id: "mean",
        published_name: None,
        published_min_power: None,
        group: "mean",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "MEAN packaged by Bitnami is a full-stack JavaScript development \
                      environment — MongoDB, Express, Angular and Node.js — with the build \
                      tooling to develop and run a single-page application on one instance.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-e5kzqmw9tvbx2",
        license: BlueprintLicense::Agreement("3c67d0a8-9b41-4f26-85d7-1e2fa9c70b53/\
                                              7b14a8f2-0d63-4c95-9e28-46bd17ca3e80.txt"),
    },
    BlueprintSpec {
        blueprint_id: "ghost_bitnami",
        published_name: None,
        published_min_power: None,
        group: "ghost-bitnami",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Ghost packaged by Bitnami is a modern publishing platform for \
                      professional blogs and newsletters, preinstalled with Node.js, MySQL and \
                      Apache and configured for the Ghost admin client.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-h4mzqkw5tvbx8",
        license: BlueprintLicense::Agreement("9f05b8e2-6d37-4a14-93c8-57ba02fd6c91/\
                                              1c48d7b9-3f20-4e86-85a7-62fd90ce413b.txt"),
    },
    BlueprintSpec {
        blueprint_id: "redmine",
        published_name: None,
        published_min_power: None,
        group: "redmine",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Redmine packaged by Bitnami is an open-source project management and \
                      issue-tracking application with wikis, time tracking and repository \
                      integration, preinstalled with Ruby, Apache and MySQL.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-u9kzqmw6tvbx3",
        license: BlueprintLicense::Agreement("5b94c1f7-8a26-4d03-97e5-30bc74af2d68/\
                                              8e26f1a3-5b47-4c09-92d8-74ac03be5f17.txt"),
    },
    BlueprintSpec {
        blueprint_id: "nginx",
        published_name: None,
        published_min_power: None,
        group: "nginx",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "NGINX Open Source packaged by Bitnami is a high-performance web server, \
                      reverse proxy and load balancer, preinstalled with PHP-FPM and the \
                      configuration layout to serve a static site or front your own \
                      application.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-k2mzqwt7vbxd4",
        license: BlueprintLicense::Agreement("2e71a4d8-0b95-4f37-86c1-49fd25ba7e03/\
                                              4a09c6b5-7d18-4e42-95f3-80be61da2c97.txt"),
    },
    BlueprintSpec {
        blueprint_id: "django",
        published_name: None,
        published_min_power: None,
        group: "django",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Django packaged by Bitnami is a Python web development stack with \
                      Django, the Apache web server and the PostgreSQL and MySQL clients, so a \
                      Django project can be deployed to a Lightsail instance without building \
                      the environment by hand.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-b6kzqmw8tvbx5",
        license: BlueprintLicense::Agreement("7d38b0c5-4f61-4a29-98e7-15ca82bd6f40/\
                                              9b57e2d1-6c03-4f84-87a9-23df05bc714e.txt"),
    },
    BlueprintSpec {
        blueprint_id: "gitlab",
        published_name: None,
        published_min_power: None,
        group: "gitlab",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "GitLab CE packaged by Bitnami is a self-hosted Git repository manager \
                      with code review, issue tracking and continuous integration, \
                      preinstalled with its PostgreSQL database, Redis and web server.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-w3mzqkt9vbxd7",
        license: BlueprintLicense::Agreement("0a92f5b3-7c48-4d16-95e0-68ba13cf7d24/\
                                              6f81b3a7-2e95-4c60-89d4-37ca58e012bf.txt"),
    },
    BlueprintSpec {
        blueprint_id: "prestashop",
        published_name: None,
        published_min_power: None,
        group: "prestashop",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "PrestaShop packaged by Bitnami is an open-source e-commerce platform \
                      for building an online store, preinstalled on Apache, PHP and MySQL with \
                      the store's back office ready to use.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-p7kzqmw2tvbx6",
        license: BlueprintLicense::Agreement("4c18d9e6-3b70-4a52-96f8-27bd50ac1e93/\
                                              2d95f7c0-8a14-4b63-97e1-50bc39da648f.txt"),
    },
    BlueprintSpec {
        blueprint_id: "cpanel_whm_almalinux",
        published_name: None,
        published_min_power: None,
        group: "cpanel-whm-almalinux",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "cPanel & WHM for AlmaLinux is a web hosting control panel for \
                      provisioning and managing hosting accounts, domains, mail and databases \
                      from a browser. The image installs cPanel & WHM on AlmaLinux; a cPanel \
                      licence is required once the trial period ends.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-a8mzqkw3tvbx9",
        license: BlueprintLicense::Agreement("8b60a3f1-5d29-4e74-93c6-01fa78bd5e42/\
                                              3a74c8d2-9f51-4b06-85e7-16bd42af903c.txt"),
    },
    BlueprintSpec {
        blueprint_id: "plesk_ubuntu_18_0_35",
        published_name: None,
        published_min_power: None,
        group: "plesk-ubuntu",
        blueprint_type: BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "Plesk Hosting Stack on Ubuntu is a web hosting control panel for \
                      running and securing sites, domains, mail and databases, with the \
                      WordPress Toolkit and Let's Encrypt integration. The image installs the \
                      Plesk hosting stack on Ubuntu; a Plesk licence is required once the \
                      trial period ends.",
        version: BLUEPRINT_IMAGE_BUILD_VERSION,
        product_page: "marketplace/pp/prodview-l5kzqmw7tvbx2",
        license: BlueprintLicense::Agreement("1f47e8b0-2a63-4c95-87d1-54ca09bf6e37/\
                                              7e03d5a9-6b18-4f72-94c0-29bd81ea453f.txt"),
    },
];

/// The blueprint `blueprint_id` names — active or not, since an instance already
/// running a retired image still has to resolve it — or `None` when this service
/// publishes no such blueprint.
pub fn blueprint_spec(blueprint_id: &str) -> Option<BlueprintSpec> {
    BLUEPRINTS
        .iter()
        .find(|spec| spec.blueprint_id == blueprint_id)
        .copied()
}

/// Whether a new instance can be created from `blueprint_id`: the catalog holds
/// it AND the entry is still active.
pub fn blueprint_is_creatable(blueprint_id: &str) -> bool {
    blueprint_spec(blueprint_id).is_some_and(|spec| spec.is_active)
}

/// One value of the modelled `InstancePlatform` enum
/// (`smithy_query(query="shape",shape="InstancePlatform")`): a Linux/Unix image.
pub const PLATFORM_LINUX_UNIX: &str = "LINUX_UNIX";

/// The other value of the modelled `InstancePlatform` enum: a Windows
/// Server-based image.
pub const PLATFORM_WINDOWS: &str = "WINDOWS";

/// The Lightsail for Research virtual-computer blueprints this service publishes.
///
/// A SEPARATE catalog from [`BLUEPRINTS`]: `GetBlueprints$appCategory` is
/// documented as "Returns a list of blueprints that are specific to Lightsail for
/// Research. You must use this parameter to view Lightsail for Research
/// blueprints", so the two catalogs are disjoint and the request's category is
/// what selects between them (see [`blueprint_catalog`]).
///
/// This emulator publishes no Lightsail for Research blueprint, so the catalog is
/// EMPTY and a request naming a category reports no blueprint. An entry here
/// would have to name a real Research image; inventing one would make
/// `GetBlueprints` report a blueprint `CreateInstances` cannot create from.
pub const LFR_BLUEPRINTS: &[BlueprintSpec] = &[];

/// How many blueprints one `GetBlueprints` page carries.
///
/// CHOSEN: Lightsail publishes no page size for this operation, only that the
/// response carries a `nextPageToken` when more blueprints remain. It is above the
/// published catalog's size, so the whole catalog is answered in one page — as AWS
/// answers it — and the paging walk still holds for a catalog that outgrows it.
pub const BLUEPRINT_PAGE_SIZE: usize = 50;

impl BlueprintSpec {
    /// `Blueprint$name` — "The friendly name of the blueprint": the name the
    /// catalog publishes for the image ([`Self::published_name`]), which for an
    /// image the catalog names after its id's own words is those words
    /// title-cased ([`blueprint_name`]) — the derivation
    /// `Instance$blueprintName` reports, so the catalog's name for a blueprint is
    /// the name an instance created from it carries.
    pub fn name(&self) -> String {
        match self.published_name {
            Some(name) => name.to_string(),
            None => blueprint_name(self.blueprint_id),
        }
    }

    /// `Blueprint$platform` — "The operating system platform (either
    /// Linux/Unix-based or Windows Server-based) of the blueprint", one of the
    /// modelled `InstancePlatform` enum's two values. Windows is the platform of
    /// exactly the Windows blueprints ([`blueprint_is_windows`]), the same
    /// classification that decides which management port a new instance opens.
    pub fn platform(&self) -> &'static str {
        if blueprint_is_windows(self.blueprint_id) {
            PLATFORM_WINDOWS
        } else {
            PLATFORM_LINUX_UNIX
        }
    }

    /// `Blueprint$description` — "The description of the blueprint": the catalog
    /// text this service publishes for the image ([`Self::description`]), the
    /// entry's own and never derived from its id, name or `type`.
    pub fn description(&self) -> String {
        self.description.to_string()
    }

    /// `Blueprint$productUrl` — "The product URL to learn more about the image
    /// or blueprint": the page AWS publishes about this image
    /// ([`AWS_PRODUCT_PAGE_URL_PREFIX`] and [`Self::product_page`]), and `None`
    /// where the catalog publishes no page for the entry, since the member is
    /// optional and an empty URL leads nowhere.
    pub fn product_url(&self) -> Option<String> {
        (!self.product_page.is_empty())
            .then(|| format!("{AWS_PRODUCT_PAGE_URL_PREFIX}{}", self.product_page))
    }

    /// `Blueprint$licenseUrl` — "The end-user license agreement URL for the
    /// image or blueprint": the agreement AWS publishes for THIS image, in
    /// whichever form it publishes it ([`BlueprintLicense`]) — the agreement
    /// document on AWS's licence host, or the usage terms on the image's own
    /// Marketplace listing — and `None` where the image is offered under no
    /// agreement of its own.
    pub fn license_url(&self) -> Option<String> {
        match self.license {
            BlueprintLicense::NotPublished => None,
            BlueprintLicense::Agreement(agreement) => Some(format!(
                "{BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX}{agreement}"
            )),
            // The terms are a section of THIS entry's listing, so the link is
            // built from the entry's own page: an entry the catalog publishes no
            // page for has no listing to carry them.
            BlueprintLicense::ListingUsageTerms => Some(format!(
                "{}{MARKETPLACE_USAGE_TERMS_FRAGMENT}",
                self.product_url()?
            )),
        }
    }

    /// `Blueprint$minPower` — "The minimum bundle power required to run this
    /// blueprint".
    ///
    /// The power of the smallest instance plan the image runs on: the floor the
    /// catalog publishes for the image where it publishes one
    /// ([`Self::published_min_power`]), otherwise its platform's — a Windows
    /// image does not run on the smallest plan
    /// ([`WINDOWS_BLUEPRINT_MIN_POWER`]), and a Linux/Unix image this catalog
    /// publishes runs on every plan, which `0` states
    /// ([`BLUEPRINT_MIN_POWER_ANY_SIZE`]).
    pub fn min_power(&self) -> i32 {
        if let Some(min_power) = self.published_min_power {
            return min_power;
        }
        if self.platform() == PLATFORM_WINDOWS {
            WINDOWS_BLUEPRINT_MIN_POWER
        } else {
            BLUEPRINT_MIN_POWER_ANY_SIZE
        }
    }

    /// This entry as the modelled `Blueprint`.
    ///
    /// `name` is the catalog's own name for the image ([`Self::name`]), and
    /// `version` the entry's own ([`Self::version`]): the release of the software
    /// THIS image installs, which two entries of one listing share only where
    /// their software does.
    pub fn view(&self) -> Blueprint {
        Blueprint {
            blueprint_id: Some(self.blueprint_id.to_string()),
            name: Some(self.name()),
            group: Some(self.group.to_string()),
            r#type: Some(self.blueprint_type.to_string()),
            description: Some(self.description()),
            is_active: Some(self.is_active),
            min_power: Some(self.min_power()),
            version: Some(self.version.to_string()),
            version_code: Some(BLUEPRINT_VERSION_CODE.to_string()),
            // `productUrl` and `licenseUrl` are optional, and an entry the
            // catalog publishes no listing or agreement for reports the ABSENT
            // member rather than an empty string.
            product_url: self.product_url(),
            license_url: self.license_url(),
            platform: Some(self.platform().to_string()),
            // Only a Lightsail for Research blueprint carries a category, and it
            // is the category of the catalog the entry came from, so
            // [`blueprints_from`] stamps it rather than the entry.
            app_category: None,
        }
    }
}

/// The catalog a `GetBlueprints` request naming `app_category` reports from: the
/// Lightsail for Research blueprints when it names a category, the instance
/// blueprints when it names none.
///
/// From `GetBlueprints$appCategory`: "You must use this parameter to view
/// Lightsail for Research blueprints" — so the standard catalog answers a request
/// that names no category, and only a request that names one sees the Research
/// blueprints.
///
/// The VALUE is not re-checked here: `appCategory` targets the modelled
/// `AppCategory` enum, whose value set the centralized constraint walk enforces
/// before any handler runs (a category the model does not name is already a 400
/// `ValidationException`), so every category that reaches this function is one the
/// model admits.
pub fn blueprint_catalog(app_category: Option<&str>) -> &'static [BlueprintSpec] {
    match app_category {
        Some(_) => LFR_BLUEPRINTS,
        None => BLUEPRINTS,
    }
}

/// The blueprints a `GetBlueprints` call reports out of `specs`: every active
/// entry, plus the inactive (retired) ones when `include_inactive` is set, each
/// stamped with `app_category` — the category of the catalog it came from.
///
/// The order is `specs`' own, so two reads answer identically. Taking the catalog
/// as an argument is what lets the slice rule be exercised against a catalog
/// whose shape the published one does not have yet.
pub fn blueprints_from(
    specs: &[BlueprintSpec],
    include_inactive: bool,
    app_category: Option<&str>,
) -> Vec<Blueprint> {
    specs
        .iter()
        .filter(|spec| include_inactive || spec.is_active)
        .map(|spec| Blueprint {
            app_category: app_category.map(str::to_string),
            ..spec.view()
        })
        .collect()
}

/// The blueprints a `GetBlueprints` call reports: [`blueprints_from`] over the
/// catalog [`blueprint_catalog`] selects.
pub fn blueprints(include_inactive: bool, app_category: Option<&str>) -> Vec<Blueprint> {
    blueprints_from(
        blueprint_catalog(app_category),
        include_inactive,
        app_category,
    )
}

/// One page of `reported`: at most `page_size` blueprints from the entry
/// `page_token` names onward, and the id of the first entry of the page after it
/// — present only while entries remain.
///
/// A page token NAMES a blueprint rather than carrying an index, so a catalog that
/// gained or lost an entry between two pages cannot make the walk skip or repeat
/// another one. `None` is the answer when the token names no blueprint of
/// `reported`: there is no page to report, which the caller turns into a refusal.
/// An absent token asks for the first page.
///
/// The walk itself is [`catalog_page`], shared with this service's other paged
/// catalogs; only the member a token names is this catalog's own.
pub fn blueprint_page(
    reported: &[Blueprint],
    page_size: usize,
    page_token: Option<&str>,
) -> Option<(Vec<Blueprint>, Option<String>)> {
    catalog_page(reported, page_size, page_token, |blueprint| {
        blueprint.blueprint_id.as_deref().map(std::borrow::Cow::Borrowed)
    })
}

/// The blueprint ids `reported` holds, for a refusal that has to tell the caller
/// what a page token may name.
pub fn reported_blueprint_ids(reported: &[Blueprint]) -> String {
    reported
        .iter()
        .filter_map(|blueprint| blueprint.blueprint_id.clone())
        .collect::<Vec<_>>()
        .join(", ")
}
