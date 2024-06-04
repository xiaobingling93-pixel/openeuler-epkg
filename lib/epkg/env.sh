#!/usr/bin/env bash

list_environments() {
	# List all environments
	echo "Available environments:"
	ls "$EPKG_ENVS_ROOT"
}

create_environment() {
	local env=$1

	create_yum_installroot  "$EPKG_ENVS_ROOT/$env/env-1"
	ln -sT env-1            "$EPKG_ENVS_ROOT/$env/env-current"

	mkdir "$EPKG_ENVS_ROOT/$env/env-1/tmp"

	__epkg_enable_environment $env
	activate_environment $env

	echo "Environment '$env' created."
}

# create YUM --installroot directory structure
create_yum_installroot() {
	local installroot="$1"

	if [ -z "$installroot" ]; then
		echo "Usage: create_yum_installroot <installroot>"
		return 1
	fi

	# Create YUM --installroot directory structure
	mkdir -p "$installroot/var/cache/yum"
	mkdir -p "$installroot/var/lib/yum"
	mkdir -p "$installroot/var/lib/rpm"
	mkdir -p "$installroot/etc/yum.repos.d"

	# Set up default yum.conf
	cat > "$installroot/etc/yum.conf" <<EOL
[main]
cachedir=/var/cache/yum/\$basearch/\$releasever
keepcache=0
debuglevel=2
logfile=/var/log/yum.log
exactarch=1
obsoletes=1
gpgcheck=1
plugins=1
installonly_limit=3
reposdir=/etc/yum.repos.d
EOL

	# Set up local repository in /etc/yum.repos.d
	cat > "$installroot/etc/yum.repos.d/local.repo" <<EOL
[local]
name=Local openEuler OS Repository
baseurl=file:///srv/os-repo/epkg/openEuler-20.03-LTS-SP1/OS/aarch64
enabled=1
gpgcheck=0
EOL

	echo "YUM --installroot directory structure created successfully in: $installroot"
}

remove_environment() {
	local env=$1

	mv "$EPKG_ENVS_ROOT/$env" "$EPKG_ENVS_ROOT/.$env"
}

# active environment for install/remove/upgrade
activate_environment() {
	local env=$1

	rm -f                "$EPKG_META_DIR/activate-env"
	ln -s "../envs/$env" "$EPKG_META_DIR/activate-env"
}

# Get current active environment for install/remove/upgrade
get_active_env() {
	readlink "$EPKG_META_DIR/activate-env"
}

env_history() {
	local env=$1

	ls -l $EPKG_ENVS_ROOT/$env
}

# Rollback environment to previous state
env_rollback() {
	local env=$1

	echo "Environment '$env' rolled back."
	# Add implementation for rollback (if available)
}
