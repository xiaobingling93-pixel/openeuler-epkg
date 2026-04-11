#!/bin/bash
# Create a temporary bindir containing a "busybox" wrapper and .config
# so BusyBox runtest can drive epkg applets without modifying the testsuite.
#
# Usage: mk-epkg-bindir.sh <bindir> [epkg_bin]
# Output: prints bindir path (for use with bindir=... runtest)

BINDIR="${1:?need bindir}"

# Get script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

. "$PROJECT_ROOT/tests/common.sh"
set_epkg_bin

[ -n "${2:-}" ] && EPKG_BIN="$2"
APPLETS_LIST=""

# Resolve to absolute path for wrapper
case "$EPKG_BIN" in
    /*) ;;
    *) EPKG_BIN="$(cd "$(dirname "$EPKG_BIN")" && pwd)/$(basename "$EPKG_BIN")" ;;
esac

mkdir -p "$BINDIR"
APPLETS_LIST=$("$EPKG_BIN" busybox --list)

# Wrapper: when run as "busybox" with no args, list applets in BusyBox format;
# when run as other name (e.g. symlink "unknown") with no args, run epkg busybox <name> so epkg prints "applet not found";
# otherwise run epkg busybox <applet> ...
WRAPPER="$BINDIR/busybox"
cat > "$WRAPPER" << 'WRAPPER_SCRIPT'
#!/bin/sh
EPKG_BIN="__EPKG_BIN__"
# Use ${0##*/} not basename so we do not invoke ourselves via PATH
BASE="${0##*/}"
if [ $# -eq 0 ] && [ "$BASE" = "busybox" ]; then
    echo "Currently defined functions:"
    echo "__APPLETS_LINE__"
    exit 0
fi
if [ "$BASE" = "busybox" ]; then
    exec "$EPKG_BIN" busybox "$@"
else
    exec "$EPKG_BIN" busybox "$BASE" "$@"
fi
WRAPPER_SCRIPT

# Replace placeholders (avoid commas so runtest's sed 's/,//g' still works)
APPLETS_LINE=$(echo $APPLETS_LIST | tr ' ' ' ')
{ sed "s|__EPKG_BIN__|$EPKG_BIN|g" "$WRAPPER" | sed "s|__APPLETS_LINE__|$APPLETS_LINE|g"; } > "$WRAPPER.tmp" && mv "$WRAPPER.tmp" "$WRAPPER"
chmod +x "$WRAPPER"

# .config: CONFIG_<APPLET>=y for each applet so runtest does not mark tests UNTESTED.
# OPTIONFLAGS is derived from CONFIG_* keys; optional(FEATURE_X) skips if not in OPTIONFLAGS.
# Add EPKG_BUSYBOX_SKIP_FEATURES as "is not set" so those optional tests are skipped.
CONFIG="$BINDIR/.config"
: > "$CONFIG"
# Use underscores (not hyphens) so .config is valid when sourced by tests (e.g. ls.tests)
for a in $APPLETS_LIST; do
    uc=$(echo "$a" | tr 'a-z-' 'A-Z_')
    echo "CONFIG_$uc=y" >> "$CONFIG"
done

# Enable xargs features for quote and replace-str support
echo "CONFIG_FEATURE_XARGS_SUPPORT_REPL_STR=y" >> "$CONFIG"
echo "CONFIG_FEATURE_XARGS_SUPPORT_QUOTES=y" >> "$CONFIG"
if [ -n "$EPKG_BUSYBOX_SKIP_FEATURES" ]; then
    for f in $(echo "$EPKG_BUSYBOX_SKIP_FEATURES" | tr ',:' ' '); do
        echo "# CONFIG_$f is not set" >> "$CONFIG"
    done
fi

echo "$BINDIR"
