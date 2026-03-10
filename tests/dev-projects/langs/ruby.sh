#!/bin/sh
# Minimal Ruby project: run script, gem install one package.

. "$(dirname "$0")/../common.sh"

run_install ruby ruby-dev ruby-devel gcc make redhat-rpm-config

check_cmd ruby --version || lang_skip "no ruby for OS=$OS"

run_ebin ruby --version

run ruby -e "puts 1+1"
run ruby -e "puts \"ok\""

run /bin/sh -c 'mkdir -p /tmp/rubyproj && cd /tmp/rubyproj && echo "puts \"hello\"" > main.rb'
run /bin/sh -c 'cd /tmp/rubyproj && ruby main.rb' | grep -q hello

# Set GEM_HOME and GEM_PATH to writable locations inside the environment
# Also set XDG_CACHE_HOME to avoid permission issues with ~/.cache
if run which gem; then
    run /bin/sh -c 'export GEM_HOME=/tmp/gem GEM_PATH=/tmp/gem XDG_CACHE_HOME=/tmp/xdg-cache && gem install json'
    run /bin/sh -c 'export GEM_HOME=/tmp/gem GEM_PATH=/tmp/gem && ruby -e "require \"json\"; puts JSON.parse(\"{\\\"x\\\":1}\")[\"x\"]"' | grep -q 1
fi
run_ebin_if gem --version
run_ebin_if gem install json
lang_ok
