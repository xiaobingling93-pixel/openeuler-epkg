# Actions that need root privilege:

epkg-installer.sh in global mode:
- run by root, or sudo
- create global
  - store
  - base env
- so dont need `epkg_helper` prefix

epkg update base env in global mode:
- run by root, or sudo
- update global
  - base env
- so dont need `epkg_helper` prefix

epkg install
- run by normal user
- add packages to global
  - store
- so need `epkg_helper` prefix, or in future, `epkg-store` suid cmd

epkg init
- run by normal user
- only setup files in $HOME
- so dont need `epkg_helper` prefix

# main scripts/programs in future
- epkg-installer.sh
- epkg-rc.sh, shell internal functions for export `PATH/EPKG_ENV_NAME`
- epkg rust
- epkg-store rust, may suid
