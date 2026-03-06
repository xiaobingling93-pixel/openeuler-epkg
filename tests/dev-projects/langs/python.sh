#!/bin/sh
# Minimal Python project: create, run, optional pip install -e .

. "$(dirname "$0")/../common.sh"

run_install python3 py3-pip python3-pip
check_cmd python3 --version || lang_skip "no python3 for OS=$OS"

run python3 -c "print(1+1)"
run python3 -c "print('ok')"

run /bin/sh -c 'mkdir -p /tmp/pyproj && cd /tmp/pyproj && echo "print(\"hello\")" > main.py'
run /bin/sh -c 'cd /tmp/pyproj && python3 main.py' | grep -q hello

# Use python3 -m pip so we don't rely on pip3 in PATH. Validate pip by installing one package (pyproj has no setup.py/pyproject.toml).
if run python3 -m pip --version; then
    run python3 -m pip install --break-system-packages six || run python3 -m pip install six
    run python3 -c "import six; print(six.__version__)" | grep -q .
fi
# Exercise ebin for pip (exposed as pip3 or pip)
run_ebin_if pip3 --version
run_ebin_if pip --version
if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/pip3" ]; then
    run "$ENV_ROOT/ebin/pip3" install --break-system-packages six || run "$ENV_ROOT/ebin/pip3" install six
    run python3 -c "import six; print(six.__version__)" | grep -q .
elif [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/pip" ]; then
    run "$ENV_ROOT/ebin/pip" install six
    run python3 -c "import six; print(six.__version__)" | grep -q .
fi
lang_ok
