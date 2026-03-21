#!/bin/sh
# Minimal Ruby project: run script, gem install one package.

. "$(dirname "$0")/../common.sh"

# Alpine needs ca-certificates-bundle for SSL certificate verification
# libcrypto3 expects /etc/ssl/cert.pem which is provided by ca-certificates-bundle
# Alpine also needs musl-dev for C runtime startup files (crt*.o) for native gem compilation
# Conda on Windows doesn't have ruby-dev or native compilation tools
case "$OS" in
    alpine) run_install ruby ruby-dev gcc make musl-dev ca-certificates-bundle ;;
    conda)  run_install ruby ;;
    *)       run_install ruby ruby-dev ruby-devel gcc make redhat-rpm-config ;;
esac

check_cmd ruby --version || lang_skip "no ruby for OS=$OS"

run_ebin ruby --version

run ruby -e "puts 1+1"
run ruby -e "puts \"ok\""

# Create test file - use ruby for conda/Windows (no /bin/sh)
if [ "$OS" = "conda" ]; then
    run ruby -e "Dir.mkdir('/tmp/rubyproj') rescue nil; File.write('/tmp/rubyproj/main.rb', 'puts \"hello\"')"
    run ruby /tmp/rubyproj/main.rb | grep -q hello
else
    run /bin/sh -c 'mkdir -p /tmp/rubyproj && cd /tmp/rubyproj && echo "puts \"hello\"" > main.rb'
    run /bin/sh -c 'cd /tmp/rubyproj && ruby main.rb' | grep -qx hello
fi

# Skip gem tests on conda/Windows (no native compilation toolchain)
if [ "$OS" = "conda" ]; then
    lang_ok
    exit 0
fi

# Set GEM_HOME and GEM_PATH to writable locations inside the environment
# Also set XDG_CACHE_HOME to avoid permission issues with ~/.cache
if run which gem; then
    run /bin/sh -c 'export GEM_HOME=/tmp/gem GEM_PATH=/tmp/gem XDG_CACHE_HOME=/tmp/xdg-cache && gem install json'
    run /bin/sh -c 'export GEM_HOME=/tmp/gem GEM_PATH=/tmp/gem && ruby -e "require \"json\"; puts JSON.parse(\"{\\\"x\\\":1}\")[\"x\"]"' | grep -qx 1
fi
run_ebin_if gem --version
# gem install needs XDG_CACHE_HOME set to avoid permission issues
XDG_CACHE_HOME=/tmp/xdg-cache run_ebin_if gem install json
lang_ok
