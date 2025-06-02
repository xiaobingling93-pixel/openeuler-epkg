use lazy_static::lazy_static;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        // Map APK field names to common field names based on gen-package.py
        // Core package metadata
        m.insert("pkgname",     "pkgname");
        m.insert("pkgver",      "version");
        m.insert("pkgdesc",     "summary");
        m.insert("url",         "homepage");
        m.insert("builddate",   "buildTime");
        m.insert("packager",    "maintainer");
        m.insert("size",        "installedSize");
        m.insert("arch",        "arch");
        m.insert("origin",      "source");
        m.insert("commit",      "commit");
        m.insert("maintainer",  "maintainer");
        m.insert("license",     "license");

        // Dependencies and relationships
        m.insert("depend",      "requires");
        m.insert("conflict",    "conflicts");
        m.insert("provides",    "provides");
        m.insert("replaces",    "replaces");
        m.insert("install_if",  "suggests");
        m.insert("triggers",    "triggers");

        // Priority and versioning
        m.insert("replaces_priority", "replaces_priority");
        m.insert("provider_priority", "provider_priority");

        // Checksums and hashes
        m.insert("datahash",    "sha256");
        m.insert("checksum",    "md5sum");

        m
    };
}

/// PKGINFO field definitions based on APK v2 specification
pub struct PkgInfoField {
    pub name: &'static str,
    pub description: &'static str,
    pub repeatable: bool,
}

lazy_static! {
    pub static ref PKGINFO_FIELDS: std::collections::HashMap<&'static str, PkgInfoField> = {
        let mut m = std::collections::HashMap::new();

        m.insert("pkgname", PkgInfoField {
            name: "pkgname",
            description: "package name",
            repeatable: false,
        });
        m.insert("pkgver", PkgInfoField {
            name: "pkgver",
            description: "package version",
            repeatable: false,
        });
        m.insert("pkgdesc", PkgInfoField {
            name: "pkgdesc",
            description: "package description",
            repeatable: false,
        });
        m.insert("url", PkgInfoField {
            name: "url",
            description: "package url",
            repeatable: false,
        });
        m.insert("builddate", PkgInfoField {
            name: "builddate",
            description: "unix timestamp of the package build date/time",
            repeatable: false,
        });
        m.insert("packager", PkgInfoField {
            name: "packager",
            description: "name (and typically email) of person who built the package",
            repeatable: false,
        });
        m.insert("size", PkgInfoField {
            name: "size",
            description: "the installed-size of the package",
            repeatable: false,
        });
        m.insert("arch", PkgInfoField {
            name: "arch",
            description: "the architecture of the package (ex: x86_64)",
            repeatable: false,
        });
        m.insert("origin", PkgInfoField {
            name: "origin",
            description: "the origin name of the package",
            repeatable: false,
        });
        m.insert("commit", PkgInfoField {
            name: "commit",
            description: "the commit hash from which the package was built",
            repeatable: false,
        });
        m.insert("maintainer", PkgInfoField {
            name: "maintainer",
            description: "name (and typically email) of the package maintainer",
            repeatable: false,
        });
        m.insert("replaces_priority", PkgInfoField {
            name: "replaces_priority",
            description: "replaces priority field for package (integer)",
            repeatable: false,
        });
        m.insert("provider_priority", PkgInfoField {
            name: "provider_priority",
            description: "provider priority for the package (integer)",
            repeatable: false,
        });
        m.insert("license", PkgInfoField {
            name: "license",
            description: "license string for the package",
            repeatable: false,
        });
        m.insert("datahash", PkgInfoField {
            name: "datahash",
            description: "hex-encoded sha256 checksum of the data tarball",
            repeatable: false,
        });

        // Repeatable fields
        m.insert("depend", PkgInfoField {
            name: "depend",
            description: "dependencies for the package",
            repeatable: true,
        });
        m.insert("replaces", PkgInfoField {
            name: "replaces",
            description: "packages this package replaces",
            repeatable: true,
        });
        m.insert("provides", PkgInfoField {
            name: "provides",
            description: "what this package provides",
            repeatable: true,
        });
        m.insert("triggers", PkgInfoField {
            name: "triggers",
            description: "what packages this package triggers on",
            repeatable: true,
        });
        m.insert("install_if", PkgInfoField {
            name: "install_if",
            description: "install this package if these packages are present",
            repeatable: true,
        });

        m
    };
}
