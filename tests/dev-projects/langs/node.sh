#!/bin/sh
# Minimal Node.js project: run script, minimal package.

. "$(dirname "$0")/../common.sh"

run_install nodejs npm node
check_cmd node --version || lang_skip "no node package for OS=$OS"

run_ebin_if npx --version
run_ebin_if npm --version
run_ebin node --version

run node -e "console.log(1+1)"
run node -e "console.log('ok')"

# Create test file - use node for conda/Windows (no /bin/sh)
if [ "$OS" = "conda" ]; then
    run node -e "require('fs').mkdirSync('/tmp/nodeproj', {recursive:true}); require('fs').writeFileSync('/tmp/nodeproj/index.js', 'console.log(\"hello\")')"
    run node /tmp/nodeproj/index.js | grep -q hello
    lang_ok
    exit 0
fi

run /bin/sh -c 'mkdir -p /tmp/nodeproj && cd /tmp/nodeproj && echo "console.log(\"hello\");" > index.js'
run /bin/sh -c 'cd /tmp/nodeproj && node index.js' | grep -q hello
# Run node from within /tmp/nodeproj so it can find the locally installed lodash module
run /bin/sh -c "cd /tmp/nodeproj && npm init -y && npm install lodash && node -e \"require('lodash'); console.log('ok')\"" | grep -q ok
# Exercise ebin for npm (install in a second dir)
run /bin/sh -c 'mkdir -p /tmp/nodeproj2 && cd /tmp/nodeproj2 && npm init -y'
if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/npm" ]; then
    run /bin/sh -c 'cd /tmp/nodeproj2 && '"$ENV_ROOT"'/ebin/npm install lodash'
fi
lang_ok
