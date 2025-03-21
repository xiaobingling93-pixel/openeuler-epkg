# epkg uninstall effects

```shell
wfg@crystal ~/epkg% bash bin/epkg-uninstaller.sh
Attention: Uninstall success
Attention: Remove epkg files  : /home/wfg/.cache/epkg/ /home/wfg/.epkg/
Attention: Remove epkg context: /home/wfg/.zshrc
Attention: For changes to take effect, close and re-open your current shell.
```

# epkg install effects

```shell
wfg@crystal ~/epkg% bash bin/epkg-installer.sh
Attention: Execute by wfg, epkg will be installed in the /home/wfg/.epkg/, sure to continue? (y: continue, others: exit)
y
Attention: Directories /home/wfg/.cache/epkg and /home/wfg/.epkg/envs/common will be created.
Attention: File /home/wfg/.zshrc will be modified.
download epkg manager
download static epkg binary
epkg has not been initialized, epkg initialization is in progress ...
Environment 'main' has been created.
Environment 'main' has been registered.
Warning: For changes to take effect, close and re-open your current shell.
wfg@crystal ~/epkg% zsh
```

# epkg entry point

```shell
Your ~/.bashrc or ~/.zshrc
=>
# grep -C1 epkg ~/.zshrc
source $HOME/.epkg/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
=>
1) set PATH
PATH=$HOME/.epkg/envs/main/profile-current/usr/app-bin:...

2) define epkg() shell builtin function
- run external rust epkg
- hash -r for newly added commands
- update $PATH after env (un)register or (de)activate
```

# epkg package manager env

```shell
wfg@crystal ~% tree ~/.epkg/envs/common/profile-current/
/home/wfg/.epkg/envs/common/profile-current/
├── bin -> usr/bin
├── etc
│   ├── epkg
│   │   └── channel.yaml
│   ├── pki/
│   └── resolv.conf
├── installed-packages.json
├── lib -> usr/lib
└── usr
    ├── bin
    │   ├── elf-loader      # entry point for binary-converted epkg applications
    │   └── epkg            # static-compiled rust, called by epkg() in epkg-rc.sh
    └── lib
        └── epkg
            ├── env.sh
            ├── epkg-rc.sh  # source from .bashrc / .zshrc
            ├── init.sh
            └── paths.sh
```

# dir layout in ~/.epkg

```shell
tree ~/.epkg/config
tree ~/.epkg/envs
ls ~/.epkg/store
```

# dir layout in ~/.cache/epkg

```shell
ls ~/.cache/epkg
```
