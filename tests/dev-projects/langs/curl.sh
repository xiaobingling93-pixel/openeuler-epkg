#!/bin/sh
# HTTPS fetch smoke test: catches TLS/CA issues inside the env (mirrors, corporate proxies, etc.).

. "$(dirname "$0")/../common.sh"

# Alpine: explicit CA bundle (same concern as ruby.sh). Other distros: curl usually pulls certs.
# brew: uses_from_macos dependencies (krb5, openldap) are now automatically resolved
case "$OS" in
    alpine) run_install curl ca-certificates-bundle ;;
    brew)   run_install curl ;;
    *)       run_install curl ;;
esac

check_cmd curl --version || lang_skip "no curl for OS=$OS"

run_ebin_if curl --version

# TLS + redirect exercise (bing.com -> www.bing.com). Discard body to keep logs small.
run curl -fsSL --max-time 60 -o /dev/null https://bing.com/

lang_ok
