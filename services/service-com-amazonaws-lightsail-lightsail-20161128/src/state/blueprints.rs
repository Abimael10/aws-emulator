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
    /// CHOSEN per entry: this service publishes no machine-readable catalog of
    /// these paragraphs, and a recorded answer quotes the text of two rows only.
    /// Those two carry the recorded wording; every other row carries this
    /// catalog's own copy for its image. What the column owes is per-entry prose
    /// about THAT image, which is what is asserted of it.
    pub description: &'static str,
    /// The AWS Marketplace product view this image's listing is published at —
    /// the `prodview-…` id [`BlueprintSpec::product_url`] builds
    /// `Blueprint$productUrl` from — or empty where the catalog publishes no
    /// listing for the entry and the member is reported absent.
    ///
    /// `Blueprint$productUrl` is "The product URL to learn more about the image
    /// or blueprint", and the catalog publishes the image's listing on AWS's own
    /// Marketplace rather than the page the image's upstream vendor keeps: the
    /// listing is what carries the image as this service offers it, including
    /// the terms the next member names.
    ///
    /// The URL FORM is the published one
    /// ([`MARKETPLACE_PRODUCT_VIEW_URL_PREFIX`] and the `prodview-<id>` token),
    /// and that is what this column owes. The id is the recorded one on the rows
    /// a recorded answer names it for, and CHOSEN on the rest, since this
    /// service publishes no index of its listings.
    pub product_view: &'static str,
    /// The end-user licence agreement AWS publishes for this image — the
    /// `<product>/<agreement>.txt` path under
    /// [`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`] that
    /// [`BlueprintSpec::license_url`] builds `Blueprint$licenseUrl` from — or
    /// empty where the image is offered under no agreement of its own and the
    /// member is reported absent.
    ///
    /// `Blueprint$licenseUrl` is "The end-user license agreement URL for the
    /// image or blueprint". The agreement is the one accepted by launching the
    /// image HERE, so the catalog serves it from AWS's own licence host; a
    /// licensed operating system and a packaged application image each carry
    /// one, while an image offered under its upstream open-source terms alone
    /// carries none.
    ///
    /// As with [`BlueprintSpec::product_view`], the path FORM is the published
    /// one, and the two identifiers in it are the recorded ones on the row a
    /// recorded answer names them for and CHOSEN on the rest.
    pub license_agreement: &'static str,
}

/// One value of the modelled `BlueprintType` enum: an operating-system image.
pub const BLUEPRINT_TYPE_OS: &str = "os";

/// One value of the modelled `BlueprintType` enum: an application stack.
pub const BLUEPRINT_TYPE_APP: &str = "app";

/// Where this service publishes a blueprint's listing: the AWS Marketplace
/// product view the image is offered at, which `Blueprint$productUrl` is the
/// product view id ([`BlueprintSpec::product_view`]) appended to.
///
/// Every entry's product link is under this one host and path: the catalog
/// points a caller at the listing AWS serves the image from, not at the page the
/// image's upstream vendor keeps about the software. The host and the
/// `prodview-<id>` form are what this column owes; see
/// [`BlueprintSpec::product_view`] for which rows' ids are recorded and which
/// are chosen.
pub const MARKETPLACE_PRODUCT_VIEW_URL_PREFIX: &str = "https://aws.amazon.com/marketplace/pp/";

/// Where this service publishes a blueprint's end-user licence agreement: the
/// AWS licence host `Blueprint$licenseUrl` is the agreement path
/// ([`BlueprintSpec::license_agreement`]) appended to.
///
/// The agreement a caller accepts by launching an image here is the one AWS
/// serves for it, which is why the catalog's licence links are all under this
/// host rather than on the upstream vendor's licensing pages.
pub const BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX: &str =
    "https://d7umqicpi7263.cloudfront.net/eula/product/";

/// `Blueprint$versionCode` — "The version code" — of every entry of
/// [`BLUEPRINTS`]: Lightsail's own revision counter for the image it publishes
/// at that version, which starts at the first revision and is the value the
/// recorded entries carry.
pub const BLUEPRINT_VERSION_CODE: &str = "1";

/// `Blueprint$version` — "The version number of the operating system,
/// application, or stack (`2016.03.0`)" — of every entry of [`BLUEPRINTS`]: the
/// publication date of the catalog generation these images belong to, in the
/// `YYYY.MM.DD` form that example shows.
///
/// It is the CATALOG's version rather than the image's own upstream release: the
/// recorded answer reports ONE version across entries of different releases — a
/// Windows Server 2025 operating-system image and a SQL Server 2022 application
/// image carry the same one — so the member pins the generation of the catalog a
/// client read, not the year its image's software was published. Re-publishing
/// the images moves this date, and moves it for every row at once.
pub const BLUEPRINT_CATALOG_VERSION: &str = "2026.09.17";

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
/// Each entry carries its OWN catalog text and links: the description this
/// service publishes for the image, the `prodview-…` id of the AWS Marketplace
/// listing it is offered at, and the path of the end-user licence agreement AWS
/// publishes for it where the image is offered under one (see
/// [`BlueprintSpec::description`], [`BlueprintSpec::product_view`] and
/// [`BlueprintSpec::license_agreement`], and
/// [`MARKETPLACE_PRODUCT_VIEW_URL_PREFIX`] /
/// [`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`] for the two URL forms). Where a
/// recorded answer quotes a row's paragraph, listing id or agreement path, that
/// row carries the recorded value; the other rows' text and ids are this
/// catalog's own, as each of those three members records.
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
                      2025. Use a Windows Server 2025 instance for .NET and ASP.NET web \
                      applications and services, Microsoft SQL Server workloads, Active \
                      Directory, file and print services, and running any other application \
                      requiring Windows software.",
        product_view: "prodview-g76csvka4r3by",
        license_agreement: "b0e459f5-2c38-4939-ad2a-d9784ecab690/\
                            0e7c7387-f24a-45b5-9206-e5ab5ff5fd9c.txt",
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2022",
        published_name: None,
        published_min_power: None,
        group: "windows_2022",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Windows Server is an environment for deploying applications using the \
                      Microsoft Web Platform. Windows Server 2022 instances suit .NET and \
                      ASP.NET web applications and services, Microsoft SQL Server workloads, \
                      Active Directory and file services, and developers who build and test on \
                      Windows. Learn more about Windows Server 2022 on the AWS Marketplace.",
        product_view: "prodview-tn7bk2vqwm5zd",
        license_agreement: "5f3a1b72-7c84-4f2d-8ba6-91d0c4e7af35/\
                            8c1d60ab-33f7-4b9e-9c21-5de84a7b0f16.txt",
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2019",
        published_name: None,
        published_min_power: None,
        group: "windows_2019",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Windows Server 2019 is an environment for deploying applications using \
                      the Microsoft Web Platform, with Internet Information Services, ASP.NET \
                      and Windows PowerShell preinstalled, and for running Active Directory, \
                      file services and the Windows desktop applications a business depends \
                      on. Learn more about Windows Server 2019 on the AWS Marketplace.",
        product_view: "prodview-rj4ye8qxzcm2v",
        license_agreement: "a47c9d10-5b26-4e83-9f4a-2c8071ed5b94/\
                            1f9b4c27-6d58-42ea-8b73-0ac915d6e482.txt",
    },
    BlueprintSpec {
        blueprint_id: "windows_server_2016",
        published_name: None,
        published_min_power: None,
        group: "windows_2016",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Windows Server 2016 is an environment for deploying applications using \
                      the Microsoft Web Platform, with Internet Information Services, ASP.NET \
                      and Windows PowerShell preinstalled. Use it to keep an application or \
                      workload running on the Windows Server release it was qualified on. \
                      Learn more about Windows Server 2016 on the AWS Marketplace.",
        product_view: "prodview-hq9mzv3btkw6s",
        license_agreement: "c82f6b31-0d47-4a95-9e26-7b418cf3d05a/\
                            3e7a28d9-14b6-4c7f-85d2-69fb0a41c738.txt",
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
        product_view: "prodview-c2jz4lr4h2yc6",
        license_agreement: "9d41e7c6-8a35-4b02-97fe-4d6b1a85c2e7/\
                            5b82f0a4-7e19-4d36-9af1-82c7e504b6d1.txt",
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
        product_view: "prodview-xbikutlmywslu",
        license_agreement: "e16b4a78-2f90-4d51-8c37-05ba9e728d14/\
                            7c04d9b2-6a81-4e35-93fd-1b58acf20e69.txt",
    },
    // Linux/Unix operating systems.
    BlueprintSpec {
        blueprint_id: "amazon_linux_2023",
        published_name: None,
        published_min_power: None,
        group: "amazon-linux",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Linux 2023 is a Linux-based operating system from Amazon Web \
                      Services, tuned for running cloud and enterprise applications and \
                      supported with security and maintenance updates. It ships with the AWS \
                      CLI, cloud-init and the AWS SDKs, and is the starting point for building \
                      your own application stack on a Lightsail instance.",
        product_view: "prodview-5xk8dwqmy2vzb",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "amazon_linux_2",
        published_name: None,
        published_min_power: None,
        group: "amazon-linux",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Amazon Linux 2 is the long-term support release of Amazon Linux, with \
                      five years of security and maintenance updates and the AWS CLI, \
                      cloud-init and AWS SDKs preinstalled. Choose it to keep an application \
                      already qualified on Amazon Linux 2 running on a Lightsail instance.",
        product_view: "prodview-pzc4mb9kvtwr7",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_24_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 24.04 LTS from Canonical, a long-term support release \
                      maintained with security updates for five years. Its large package \
                      archive and current language runtimes make it a general-purpose base for \
                      web applications, APIs and development environments on a Lightsail \
                      instance.",
        product_view: "prodview-2jmwz6bqxkd4t",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_22_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 22.04 LTS from Canonical, a long-term support release \
                      maintained with security updates for five years. Choose it where an \
                      application is already qualified against the 22.04 package set and its \
                      language runtimes.",
        product_view: "prodview-kd8vqz3mwbtx5",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_20_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Ubuntu Server 20.04 LTS from Canonical, carried for applications pinned \
                      to its package set and runtimes. Newer long-term support releases are \
                      published in this catalog for a new deployment.",
        product_view: "prodview-9wmtb4kzqvxd2",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "ubuntu_18_04",
        published_name: None,
        published_min_power: None,
        group: "ubuntu",
        blueprint_type: BLUEPRINT_TYPE_OS,
        // Ubuntu 18.04 LTS is past its upstream end of standard support.
        is_active: false,
        description: "Ubuntu Server 18.04 LTS from Canonical. The release is past the end of \
                      its standard support period and is listed so an instance already running \
                      it still resolves its image; choose a current long-term support release \
                      for a new instance.",
        product_view: "prodview-bq6zktmwx3vd8",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "debian_12",
        published_name: None,
        published_min_power: None,
        group: "debian",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Debian 12 is the stable release of the Debian GNU/Linux distribution, \
                      maintained by the Debian project with security updates. Its conservative \
                      package set and small default install make it a predictable base for \
                      servers and self-managed application stacks.",
        product_view: "prodview-x4ktqzm9wbvd3",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "debian_11",
        published_name: None,
        published_min_power: None,
        group: "debian",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "Debian 11 is carried as the previous Debian stable release, for \
                      applications pinned to its package set and library versions, and is \
                      maintained with long-term security updates by the Debian project.",
        product_view: "prodview-m2wvqz7ktbxd5",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "debian_10",
        published_name: None,
        published_min_power: None,
        group: "debian",
        blueprint_type: BLUEPRINT_TYPE_OS,
        // Debian 10 is past its upstream end of life.
        is_active: false,
        description: "Debian 10 has reached the end of its life upstream and is listed so an \
                      instance already running it still resolves its image; choose a current \
                      Debian stable release for a new instance.",
        product_view: "prodview-7bkzqvm4twxd9",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "almalinux_9",
        published_name: None,
        published_min_power: None,
        group: "almalinux",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "AlmaLinux OS 9 is a community-owned, production-grade Linux \
                      distribution that is binary compatible with Red Hat Enterprise Linux 9. \
                      It is a drop-in base for RPM-based enterprise workloads, with security \
                      and maintenance updates from the AlmaLinux OS Foundation.",
        product_view: "prodview-vz3mqkt8wbxd6",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "centos_stream_9",
        published_name: None,
        published_min_power: None,
        group: "centos-stream",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "CentOS Stream 9 is the continuously delivered distribution that tracks \
                      just ahead of Red Hat Enterprise Linux 9, for developers who build and \
                      test RPM-based software against the next minor release.",
        product_view: "prodview-q8wmzkt2vbxd4",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "freebsd_14",
        published_name: None,
        published_min_power: None,
        group: "freebsd",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "FreeBSD 14 is an advanced open-source operating system from the FreeBSD \
                      Project, with the ZFS file system, jails and the pkg package collection. \
                      Choose it for network services and storage workloads that rely on the \
                      FreeBSD stack.",
        product_view: "prodview-t5zqmkw9vbxd7",
        license_agreement: "",
    },
    BlueprintSpec {
        blueprint_id: "opensuse_15",
        published_name: None,
        published_min_power: None,
        group: "opensuse",
        blueprint_type: BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "openSUSE Leap 15 is a stable Linux distribution built from SUSE Linux \
                      Enterprise sources, with the YaST administration tools and the zypper \
                      package manager, for administrators who want an enterprise-derived base \
                      under community maintenance.",
        product_view: "prodview-z9qmkt3wvbxd8",
        license_agreement: "",
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
        product_view: "prodview-c3thaabbmnmfy",
        license_agreement: "2a7f85c1-4b63-4d09-9e87-06c15df2b3a8/\
                            4d19c7e6-85b2-4f71-9a03-27be6cd81f54.txt",
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
        product_view: "prodview-g4jkpwqz2mvxb",
        license_agreement: "6b3d09f5-7c12-4a84-95e0-3f87ba6d41c2/\
                            8f27a1b4-0d96-4e53-87ca-5b19de402f76.txt",
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
        product_view: "prodview-n7qzmkw4tvbx9",
        license_agreement: "0c54e8a7-3b19-4f62-8d05-9ae71c24b6f3/\
                            2b96d7f0-5c84-4a19-93e7-61fd08ac527b.txt",
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
        product_view: "prodview-d2kmqzw8tvbx5",
        license_agreement: "7e28b1d4-6f05-4c93-85a1-2d9b70fe4a86/\
                            9a51c3e8-7b26-4d80-96f4-38ce15b7024d.txt",
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
        product_view: "prodview-j6wzqmk3tvbx7",
        license_agreement: "4f91a6c3-0e78-4b25-9d6f-83a21cb7e504/\
                            6c38f9a1-2d47-4e96-8b50-71de94a2c3f8.txt",
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
        product_view: "prodview-r8kzqmw2tvbx4",
        license_agreement: "1d86c4f9-5a03-4e71-92b8-4f7c06ad3e25/\
                            3f70b2d5-8c91-4a67-85de-029ab6f41c73.txt",
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
        product_view: "prodview-y3mzqkw7tvbx6",
        license_agreement: "8a23f7b6-1c94-4d58-90e2-6b5d31ca847f/\
                            5d81e6c0-4a37-4b92-87f1-93ce20b74da6.txt",
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
        product_view: "prodview-e5kzqmw9tvbx2",
        license_agreement: "3c67d0a8-9b41-4f26-85d7-1e2fa9c70b53/\
                            7b14a8f2-0d63-4c95-9e28-46bd17ca3e80.txt",
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
        product_view: "prodview-h4mzqkw5tvbx8",
        license_agreement: "9f05b8e2-6d37-4a14-93c8-57ba02fd6c91/\
                            1c48d7b9-3f20-4e86-85a7-62fd90ce413b.txt",
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
        product_view: "prodview-u9kzqmw6tvbx3",
        license_agreement: "5b94c1f7-8a26-4d03-97e5-30bc74af2d68/\
                            8e26f1a3-5b47-4c09-92d8-74ac03be5f17.txt",
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
        product_view: "prodview-k2mzqwt7vbxd4",
        license_agreement: "2e71a4d8-0b95-4f37-86c1-49fd25ba7e03/\
                            4a09c6b5-7d18-4e42-95f3-80be61da2c97.txt",
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
        product_view: "prodview-b6kzqmw8tvbx5",
        license_agreement: "7d38b0c5-4f61-4a29-98e7-15ca82bd6f40/\
                            9b57e2d1-6c03-4f84-87a9-23df05bc714e.txt",
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
        product_view: "prodview-w3mzqkt9vbxd7",
        license_agreement: "0a92f5b3-7c48-4d16-95e0-68ba13cf7d24/\
                            6f81b3a7-2e95-4c60-89d4-37ca58e012bf.txt",
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
        product_view: "prodview-p7kzqmw2tvbx6",
        license_agreement: "4c18d9e6-3b70-4a52-96f8-27bd50ac1e93/\
                            2d95f7c0-8a14-4b63-97e1-50bc39da648f.txt",
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
        product_view: "prodview-a8mzqkw3tvbx9",
        license_agreement: "8b60a3f1-5d29-4e74-93c6-01fa78bd5e42/\
                            3a74c8d2-9f51-4b06-85e7-16bd42af903c.txt",
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
        product_view: "prodview-l5kzqmw7tvbx2",
        license_agreement: "1f47e8b0-2a63-4c95-87d1-54ca09bf6e37/\
                            7e03d5a9-6b18-4f72-94c0-29bd81ea453f.txt",
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
    /// or blueprint": the AWS Marketplace listing this image is offered at
    /// ([`MARKETPLACE_PRODUCT_VIEW_URL_PREFIX`] and [`Self::product_view`]), and
    /// `None` where the catalog publishes no listing for the entry, since the
    /// member is optional and an empty URL leads nowhere.
    pub fn product_url(&self) -> Option<String> {
        (!self.product_view.is_empty())
            .then(|| format!("{MARKETPLACE_PRODUCT_VIEW_URL_PREFIX}{}", self.product_view))
    }

    /// `Blueprint$licenseUrl` — "The end-user license agreement URL for the
    /// image or blueprint": the agreement AWS publishes for this image
    /// ([`BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX`] and
    /// [`Self::license_agreement`]), and `None` where the image is offered under
    /// no agreement of its own.
    pub fn license_url(&self) -> Option<String> {
        (!self.license_agreement.is_empty()).then(|| {
            format!(
                "{BLUEPRINT_LICENSE_AGREEMENT_URL_PREFIX}{}",
                self.license_agreement
            )
        })
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
    /// `version` the catalog generation's ([`BLUEPRINT_CATALOG_VERSION`]), which
    /// every entry of one generation carries.
    pub fn view(&self) -> Blueprint {
        Blueprint {
            blueprint_id: Some(self.blueprint_id.to_string()),
            name: Some(self.name()),
            group: Some(self.group.to_string()),
            r#type: Some(self.blueprint_type.to_string()),
            description: Some(self.description()),
            is_active: Some(self.is_active),
            min_power: Some(self.min_power()),
            version: Some(BLUEPRINT_CATALOG_VERSION.to_string()),
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
