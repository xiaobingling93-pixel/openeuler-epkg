#!/bin/sh
# Minimal Python project: create, run, optional pip install -e .

. "$(dirname "$0")/../common.sh"

# Note: Arch Linux uses python-pip, Debian/Ubuntu uses python3-pip, Alpine uses py3-pip
# Conda/msys2 on Windows provides 'python' not 'python3', and no /bin/sh
# brew: host /bin/sh fails with vdso_time SIGSEGV, use bash and coreutils
if [ "$OS" = "conda" ] || [ "$OS" = "msys2" ]; then
    run_install python pip
    check_cmd python --version || lang_skip "no python for OS=$OS"
    PYTHON_CMD="python"
    # Conda/msys2 on Windows has no /bin/sh, use python to create test files
    SHELL_CMD=""
elif [ "$OS" = "brew" ]; then
    run_install python3 bash coreutils expat
    check_cmd python3 --version || lang_skip "no python3 for OS=$OS"
    PYTHON_CMD="python3"
    SHELL_CMD="bash -c"
else
    run_install python3 py3-pip python3-pip python-pip
    check_cmd python3 --version || lang_skip "no python3 for OS=$OS"
    PYTHON_CMD="python3"
    SHELL_CMD="/bin/sh -c"
fi

# msys2: use bash to run python commands (workaround for Windows arg quoting issues)
if [ "$OS" = "msys2" ]; then
    run bash -c "$PYTHON_CMD -c 'print(1+1)'"
    run bash -c "$PYTHON_CMD -c 'print(\"ok\")'"
else
    run $PYTHON_CMD -c "print(1+1)"
    run $PYTHON_CMD -c "print(\"ok\")"
fi

# Create test project directory and file
if [ -n "$SHELL_CMD" ]; then
    run $SHELL_CMD 'mkdir -p /tmp/pyproj && cd /tmp/pyproj && echo "print(\"hello\")" > main.py'
    run $SHELL_CMD "cd /tmp/pyproj && $PYTHON_CMD main.py" | grep -q hello
else
    # Windows conda/msys2: use python to create the test file
    run $PYTHON_CMD -c "import os; os.makedirs('/tmp/pyproj', exist_ok=True); open('/tmp/pyproj/main.py', 'w').write('print(\"hello\")')"
    run $PYTHON_CMD /tmp/pyproj/main.py | grep -q hello
fi

# Use python -m pip so we don't rely on pip3 in PATH.
# Skip pip tests on conda/Windows due to Python 3.14 _ctypes module issue
if [ "$OS" != "conda" ]; then
    if run $PYTHON_CMD -m pip --version; then
        run $PYTHON_CMD -m pip install --break-system-packages six || run $PYTHON_CMD -m pip install six
        run $PYTHON_CMD -c "import six; print(six.__version__)" | grep -q .
    fi
    # Exercise ebin for pip (exposed as pip3 or pip)
    run_ebin_if pip3 --version
    run_ebin_if pip --version
    run_ebin_if python --version
    run_ebin $PYTHON_CMD --version
    if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/pip3" ]; then
        run "$ENV_ROOT/ebin/pip3" install --break-system-packages six || run "$ENV_ROOT/ebin/pip3" install six
        run $PYTHON_CMD -c "import six; print(six.__version__)" | grep -q .
    elif [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/pip" ]; then
        run "$ENV_ROOT/ebin/pip" install six
        run $PYTHON_CMD -c "import six; print(six.__version__)" | grep -q .
    fi
fi
lang_ok
