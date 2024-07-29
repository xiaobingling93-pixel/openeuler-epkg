#!/usr/bin/env bash

list_environments() {
	# List all environments
	echo "Available environments(sort by time):"
	all_envs=$(ls -lt $EPKG_ENVS_ROOT | grep '^d' | awk '{print $9}')
	echo "$all_envs"
	echo "You are in [$EPKG_ENV_NAME] now"
}

create_environment() {
	local env=$1

	_check_env_existed $env
	if [ $? -eq 0 ]; then
		echo "$env already existed!"
		return
	fi

	create_yum_installroot  "$EPKG_ENVS_ROOT/$env/profile-1"
	ln -sT profile-1        "$EPKG_ENVS_ROOT/$env/profile-current"

	mkdir "$EPKG_ENVS_ROOT/$env/profile-1/tmp"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/bin"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/sbin"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/lib"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/lib64"

	ln -sT  "usr/bin"  "$EPKG_ENVS_ROOT/$env/profile-1/bin"
	ln -sT  "usr/sbin"  "$EPKG_ENVS_ROOT/$env/profile-1/sbin"
	ln -sT  "usr/lib"  "$EPKG_ENVS_ROOT/$env/profile-1/lib"
	ln -sT  "usr/lib64"  "$EPKG_ENVS_ROOT/$env/profile-1/lib64"

	__epkg_activate_environment $env
	echo "Environment '$env' created."
}


activate_environment() {
	local env=$1

	create_yum_installroot  "$EPKG_ENVS_ROOT/$env/profile-1"

	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/bin"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/sbin"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/lib"
	mkdir -p "$EPKG_ENVS_ROOT/$env/profile-1/usr/lib64"

	__epkg_activate_environment $env
	echo "Environment '$env' activated."
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
baseurl=file:///srv/os-repo/epkg/openeuler/openEuler-20.03-LTS-SP1/OS/aarch64
enabled=1
gpgcheck=0
EOL

	echo "YUM --installroot directory structure created successfully in: $installroot"
}

remove_environment() {
	local env=$1

	_check_env_existed $env
	if [ $? -eq 1 ]; then
		echo "$env no existed!"
		return
	fi
	
	mv "$EPKG_ENVS_ROOT/$env" "$EPKG_ENVS_ROOT/.$env"
}

# setup env variable
get_active_env() {
	env="$*"
	env="${env#*--env }"

	[ "$env" != "$*" ] && {
		env=${env%% *}
		return
	}

	[ -n "$EPKG_ENV_NAME" ] && {
		env=$EPKG_ENV_NAME
		return
	}

	env=main
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