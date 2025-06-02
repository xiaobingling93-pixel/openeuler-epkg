use lazy_static::lazy_static;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        // Map APK field names to common field names based on gen-package.py
        m.insert("pkgname",     "pkgname");
        m.insert("pkgver",      "version");
        m.insert("pkgdesc",     "summary");
        m.insert("url",         "homepage");
        m.insert("builddate",   "buildTime");
        m.insert("packager",    "maintainer");
        m.insert("size",        "installedSize");
        m.insert("arch",        "arch");
        m.insert("commit",      "commit");
        m.insert("origin",      "source");
        m.insert("maintainer",  "maintainer");
        m.insert("license",     "license");
        m.insert("depend",      "requires");
        m.insert("conflict",    "conflicts");
        m.insert("provides",    "provides");
        m.insert("replaces",    "replaces");
        m.insert("datahash",    "sha256");

        // Additional APK fields that might be present
        m.insert("checksum",    "md5sum");
        m.insert("install_if",  "suggests");
        m.insert("provider_priority", "priority");

        m
    };
}
