#!/bin/sh
# Minimal Node.js project: run script, minimal package.

. "$(dirname "$0")/../common.sh"

# brew: node needs libstdc++.so.6 from gcc, also bash/coreutils for shell commands
if [ "$OS" = "brew" ]; then
    run_install nodejs npm node ca-certificates bash coreutils gcc
else
    run_install nodejs npm node ca-certificates
fi
check_cmd node --version || lang_skip "no node package for OS=$OS"

run_ebin_if npx --version
run_ebin_if npm --version
run_ebin node --version

run node -e "console.log(1+1)"
run node -e "console.log('ok')"

# Create test file - use node for conda/Windows (no /bin/sh)
# brew: use bash instead of /bin/sh (vdso_time SIGSEGV)
if [ "$OS" = "conda" ]; then
    run node -e "require('fs').mkdirSync('/tmp/nodeproj', {recursive:true}); require('fs').writeFileSync('/tmp/nodeproj/index.js', 'console.log(\"hello\")')"
    run node /tmp/nodeproj/index.js | grep -q hello
    lang_ok
    exit 0
elif [ "$OS" = "brew" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/nodeproj && cd /tmp/nodeproj && echo "console.log(\"hello\");" > index.js'
run $SHELL_CMD 'cd /tmp/nodeproj && node index.js'
# Run node from within /tmp/nodeproj so it can find the locally installed lodash module
run $SHELL_CMD "cd /tmp/nodeproj && npm init -y && npm install lodash && node -e \"require('lodash'); console.log('ok')\""
# Exercise ebin for npm (install in a second dir)
run $SHELL_CMD 'mkdir -p /tmp/nodeproj2 && cd /tmp/nodeproj2 && npm init -y'
if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/npm" ]; then
    run /bin/sh -c 'cd /tmp/nodeproj2 && '"$ENV_ROOT"'/ebin/npm install lodash'
fi
lang_ok
