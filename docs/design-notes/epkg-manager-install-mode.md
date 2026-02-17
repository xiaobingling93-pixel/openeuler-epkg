support install modes:
1. install epkg manager for everyone
e.g. # yum install epkg.rpm (means --install-mode global)
2. install epkg manager for myself

# curl installer.sh --install-mode [global|user]

global mode:
	env self (epkg manager): shared
	/opt/epkg/store: shared

	env: per-user

global mode install actions:
	epkg manager's env -> global
	set uid epkg root-helper binary


design:
```bash
HOME_EPKG=$HOME/.epkg
OPT_EPKG=/opt/epkg
PUB_EPKG=$OPT_EPKG/users/public

if EPKG_INSTALL_MODE==global and EPKG_ENV_NAME==self:
    EPKG_USER_DIR=$HOME_EPKG
else:
    EPKG_USER_DIR=$PUB_EPKG

EPKG_SELF_ENV_ROOT: self env根目录
	global | user: $PUB_EPKG/envs
EPKG_ENV_ROOT:
	global | user: $EPKG_USER_DIR/envs
EPKG_CONFIG_DIR:
	global | user: $EPKG_USER_DIR/config
EPKG_TEMP:
	global | user: $EPKG_USER_DIR/tmp
EPKG_STORE_ROOT:
	global: $OPT_EPKG/store
	user:   $HOME_EPKG/store
EPKG_CACHE:
	global: $OPT_EPKG/cache
	user:   $HOME/.cache/epkg
EPKG_PKG_CACHE_DIR:
	global | user: $EPKG_CACHE/packages
EPKG_CHANNEL_CACHE_DIR:
	global | user: $EPKG_CACHE/channel
EPKG_INIT_ROOT:
	global: /opt/.epkg/.init
	user:   $HOME_EPKG/.init
```

# epkg-helper(root-helper)
1. epkg-helper: Operate the /opt/.epkg directory
	- EPKG_COMM_ENV_ROOT: /opt/.epkg/envs/
	- EPKG_PKG_CACHE_DIR: /opt/.cache/epkg/packages
    - EPKG_CHANNEL_CACHE_DIR: /opt/.cache/epkg/channel
	- EPKG_STORE_ROOT:    /opt/.epkg/store
	- EPKG_TEMP:          /opt/.temp
2. Add SUID permission to epkg-helper, and close other permissions for the /opt/.epkg/store directory
	- chown root:root /usr/bin/epkg_helper
	- chmod 4755 /usr/bin/epkg_helper
	- chmod 755 /opt/.epkg/
3. Keep the external interface unchanged, e.g., epkg install tree
	- Global mode: epkg calls epkg-helper to operate the comm & store directory
	- User mode: The store directory is located in $HOME, no need to use epkg-helper
