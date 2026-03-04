#!/bin/sh
#
# sandbox-epkg.sh — Run a shell or command in a lightweight namespace sandbox for the epkg project.
#
# epkg uses user namespaces; bwrap/firejail/chroot are unsuitable because they conflict with that.
# This script uses: sudo unshare --mount, then bind mounts, pivot_root, and runuser to create an
# isolated view of the system with the project and needed configs available.
#
# Environment:
#   IN_SANDBOX    Set by this script, when re-execing inside the sandbox; do not set manually
#   SUDO_HOME     Set by sudo, used as HOME inside the sandbox
#
# When using Claude or Cursor inside the sandbox, disable their /sandbox feature so the user
# namespace remains usable for epkg.

show_help() {
    cat << 'EOF'
Usage: sandbox-epkg.sh [-h|--help] [COMMAND [ARGS...]]

Run a shell or command inside a lightweight mount-namespace sandbox for the epkg project.
Must be run from the project root (e.g. /c/epkg). Uses `sudo unshare + pivot_root + runuser`.

Options:
  -h, --help   Show this help and exit.

Examples:
  cd /path/to/epkg   # must run from project root
  ./bin/sandbox-epkg.sh # runs bash by default
  ./bin/sandbox-epkg.sh bash
  ./bin/sandbox-epkg.sh claude --dangerously-skip-permissions
  ./bin/sandbox-epkg.sh cursor
EOF
}

case "$1" in
    -h|--help) show_help; exit 0 ;;
esac

if [ -z "$IN_SANDBOX" ]; then
    export IN_SANDBOX=1

    [ "$1" = claude ] &&
    [ -r "$HOME/bin/claude-env.sh" ] &&
    . "$HOME/bin/claude-env.sh"

    # Rerun myself in the sandbox
    exec sudo --preserve-env unshare --mount "$0" "$@"
fi

SANDBOX_ROOT=/tmp/sandbox-epkg
HOME=${SUDO_HOME:?}
PROJECT_DIR="$PWD"

# --- Mount helpers ---
mount_ro() {
    local mount_options='-o ro'
    mount_generic "$@"
}

mount_rw() {
    mount_generic "$@"
}

mount_generic() {
    [ -e "$1" ] || return
    if [ -d "$1" ]; then
        mkdir -p "$SANDBOX_ROOT/$1"
    else
        touch "$SANDBOX_ROOT/$1"
    fi
    mount --bind $mount_options "$1" "$SANDBOX_ROOT/$1"
}

mount_tmpfs() {
    mount -t tmpfs tmpfs "$SANDBOX_ROOT/$1"
}

make_own_dir() {
    mkdir -p          "$SANDBOX_ROOT/$1"
    chown "$SUDO_UID" "$SANDBOX_ROOT/$1"
}

# --- Sandbox layout: root tmpfs, dirs, symlinks ---
setup_sandbox_layout() {
    mkdir -p "$SANDBOX_ROOT"
    mount_tmpfs       # tmpfs on sandbox root ($1 empty => $SANDBOX_ROOT)
    cd "$SANDBOX_ROOT" || exit 1
    mkdir -p proc sys dev tmp run/user/"$SUDO_UID" run/dbus var/log var/tmp usr etc root old_root opt
    make_own_dir "$HOME"
    make_own_dir "$HOME/.config"
    make_own_dir "$HOME/.local/share"
    make_own_dir "$HOME/.local/libclang"
    make_own_dir "$HOME/.local/lib"

    ln -sf ../run       "$SANDBOX_ROOT/var/run"
    ln -sf usr/lib      "$SANDBOX_ROOT/lib"
    ln -sf usr/lib64    "$SANDBOX_ROOT/lib64"
    ln -sf usr/bin      "$SANDBOX_ROOT/bin"
    ln -sf usr/sbin     "$SANDBOX_ROOT/sbin"
}

# --- Core VFS: proc, dev, sys, devpts, tmp, shm ---
setup_core_fs() {
    mount -t proc proc "$SANDBOX_ROOT/proc"
    mount_rw /dev
    mount_rw /sys

    mount -t devpts -o gid=5,mode=0620,newinstance devpts "$SANDBOX_ROOT/dev/pts"
    [ -e "$SANDBOX_ROOT/dev/ptmx" ] || ln -sf pts/ptmx "$SANDBOX_ROOT/dev/ptmx"

    mount_tmpfs /tmp
    mount_tmpfs /var/tmp
    mount_tmpfs /dev/shm
    chmod 1777 "$SANDBOX_ROOT/tmp" "$SANDBOX_ROOT/var/tmp" "$SANDBOX_ROOT/dev/shm"
}

# --- System dirs and config (read-only) ---
mount_system_config() {
    mount_ro /usr
    mount_ro /boot
    mount_ro /lib/modules
    mount_ro /etc/resolv.conf
    mount_ro /etc/nsswitch.conf
    mount_ro /etc/localtime
    mount_ro /etc/timezone
    mount_ro /etc/machine-id
    mount_ro /etc/hostname
    mount_ro /etc/hosts
    mount_ro /etc/passwd
    mount_ro /etc/group
    mount_ro /etc/subuid
    mount_ro /etc/subgid
    mount_ro /etc/manpath.config
    mount_ro /etc/ld.so.cache
    mount_ro /etc/login.defs
    mount_ro /etc/default/
    mount_ro /etc/pam.d/
    mount_ro /etc/alternatives/
    mount_ro /etc/fonts/
    mount_ro /etc/ssl/
    mount_ro /etc/ca-certificates/
    mount_ro /etc/pki/
}

# --- Runtime: systemd, XDG_RUNTIME_DIR, Wayland, X11, D-Bus ---
mount_runtime() {
    mount_ro /run/systemd
    mount_ro /run/initctl
    mount_rw "/run/user/$SUDO_UID"
    export XDG_RUNTIME_DIR="/run/user/$SUDO_UID"

    WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-1}"
    mount_ro "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"

    XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}"
    mount_ro "$XAUTHORITY"
    mount_ro /tmp/.X11-unix

    mount_ro "/run/user/$SUDO_UID/bus"
    mount_rw "/run/user/$SUDO_UID/dbus-1"
    mount_ro /run/dbus/system_bus_socket
}

# --- Container runtimes and config ---
mount_containers() {
    mount_rw "/run/user/$SUDO_UID/podman"
    # mount_rw "/run/docker.sock"
    mount_rw "/run/user/$SUDO_UID/containers"
    mount_ro /etc/containers
    mount_ro "$HOME/.config/containers"
    mount_ro "$HOME/.local/share/containers"
}

# --- User env: git, cargo, shell config ---
mount_user_env() {
    mount_ro "$HOME/.gitconfig"
    mount_rw "$HOME/.cargo"
    mount_ro "$HOME/.rustup"
    mount_rw "$HOME/.local/libclang"
    mount_rw "$HOME/.local/lib"

    # Interactive shell environment
    mount_rw "$HOME/.zshrc"         # rw, since may be modified by `epkg self install` or remove
    mount_rw "$HOME/.bashrc"        # ditto
    mount_rw "$HOME/.zsh_history"   # for human; claude will use standalone .bash_history, so won't mess up personal history
    mount_ro "$HOME/.zshenv"
    mount_ro "$HOME/.shell/"
    mount_ro "$HOME/.inputrc"
    mount_ro "$HOME/.vimrc"
    mount_ro "$HOME/.vim/"
    mount_rw "$HOME/.vim/cache"

    # to avoid claude using the inherited SHELL=zsh
    unset SHELL
}

# --- IDE config and data (Cursor, Claude) ---
mount_ide_config() {
    mount_rw "$HOME/.cursor"
    mount_rw "$HOME/.cursor-server"
    mount_rw "$HOME/.config/Cursor"
    mount_rw "$HOME/.local/share/Cursor"

    mount_rw "$HOME/.claude"
    mount_rw "$HOME/.claude.json"

    mount_rw "$HOME/.local/share/opencode"
    mount_rw "$HOME/.local/state/opencode"
    mount_rw "$HOME/.cache/opencode"
    mount_rw "$HOME/.config/opencode"
}

# --- epkg state and project (incl. 3rd-party source trees) ---
mount_epkg_and_project() {
    mount_rw "$HOME/.epkg"
    mount_rw "$HOME/.cache/epkg"
    mount_rw /opt/epkg
    mount_rw /c/compass-ci/
    mount_rw /c/lkp-tests/
    mount_ro /c/os/
    mount_ro /c/rust/
    mount_rw /c/rust/osxcross
    mount_rw /c/rust/libkrun
    mount_ro /c/package-managers/
    mount_ro /c/rpm-software-management/
    mount_rw "$PROJECT_DIR"
}

# --- Pivot into sandbox root and drop host root ---
enter_sandbox() {
    cd "$SANDBOX_ROOT" || exit 1
    pivot_root . old_root
    umount -l /old_root
}

# --- Run command as original user from project dir ---
run_in_sandbox() {
    cd "$PROJECT_DIR" || exit 1
    if [ "$#" -gt 0 ]; then
        runuser -u "$SUDO_USER" -- "$@"
    else
        runuser -u "$SUDO_USER" -- bash
    fi
}

# --- Main ---
setup_sandbox_layout
setup_core_fs
mount_system_config
mount_runtime
mount_containers
mount_user_env
mount_ide_config
mount_epkg_and_project
enter_sandbox
run_in_sandbox "$@"
