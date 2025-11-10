#!/usr/bin/env bash

# Usage example:
# tests/debug_solve.sh fedora vtk
# tests/debug_solve.sh fedora vtk hdf5
# grep -C3 hdf5 /tmp/dd|copy

os=$1
pkg_to_install=$2
pkg_no_candidate=$3

RUST_LOG=debug epkg --assume-no -e $os install --no-install-essentials $pkg_to_install 2>&1 | sed 's/.*DEBUG //' > /tmp/dd
echo "
reproduce command:
	RUST_LOG=debug epkg --assume-no -e $os install --no-install-essentials $pkg_to_install

package metadata query command:
	epkg -e $os info $pkg_to_install
" >> /tmp/dd

# If pkg_no_candidate is provided, run grep command
if [ -n "$pkg_no_candidate" ]; then
    echo "
	epkg -e $os info $pkg_no_candidate

debug log grep command:
	grep -F -C3 '$pkg_no_candidate' /tmp/dd | xclip -in -selection clipboard
" >> /tmp/dd
    grep -F -C3 "$pkg_no_candidate" /tmp/dd | xclip -in -selection clipboard
fi

less /tmp/dd
