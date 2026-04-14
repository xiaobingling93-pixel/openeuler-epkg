#!/bin/sh
# HTTPS fetch smoke test: catches TLS/CA issues inside the env (mirrors, corporate proxies, etc.).

. "$(dirname "$0")/../common.sh"

# Alpine: explicit CA bundle (same concern as ruby.sh). Other distros: curl usually pulls certs.
# brew: curl has implicit lib dependencies (libldap, libgssapi_krb5, libsasl2) not declared in formula
case "$OS" in
    alpine) run_install curl ca-certificates-bundle ;;
    brew)   run_install curl krb5 openldap cyrus-sasl ;;
    *)       run_install curl ;;
esac

check_cmd curl --version || lang_skip "no curl for OS=$OS"

run_ebin_if curl --version

# TLS + redirect exercise (bing.com -> www.bing.com). Discard body to keep logs small.
run curl -fsSL --max-time 60 -o /dev/null https://bing.com/

lang_ok
