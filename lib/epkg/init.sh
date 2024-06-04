#!/usr/bin/env bash

epkg_init() {
	local reverse=false

	while [ $# -gt 0 ]; do
		case "$1" in
			--reverse)
				reverse=true
				;;
			*)
				echo "Invalid option: $1"
				return 1
				;;
		esac
		shift
	done

	init_paths
	create_environment epkg     # package manage tools
	create_environment main     # main user environment
	__epkg_update_path
	init_rc
}

init_rc() {
	local shell

	shell=$(basename "$SHELL")

	local rc_path
	case "$shell" in
		"bash")
			rc_path="$HOME/.bashrc"
			;;
		"zsh")
			rc_path="$HOME/.zshrc"
			;;
		*)
			echo "Unsupported shell: $shell"
			return 1
			;;
	esac
	append_user_rc "$rc_path"
}

# append content to user shell rc file
append_user_rc() {
	local rc_path="$1"

	if grep -qF "shell-path" "$rc_path"; then
		echo "epkg is already initialized in '$rc_path'"
	else
		echo '$HOME/.epkg/meta/shell-path.sh' >> "$rc_path"
		echo '$EPKG_RC' >> "$rc_path"
		echo "For changes to take effect, close and re-open your current shell."
	fi
}
