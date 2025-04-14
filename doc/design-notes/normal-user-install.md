# epkg installed by normal user

Will install packages to EPKG_STORE_DIR=$HOME/.epkg/store/
There are many ways to run them.

## transparent container

Ideal:

    mount --bind $HOME/.epkg/store/ /opt/epkg/store/

However only /opt exists, so have to do some tricks:

    mkdir -p $HOME/.epkg/opt/epkg
    # this will become dead link in bind mount
    for i in /opt/*
    do
        ln -s $i $HOME/.epkg/opt/$(basename $i)
    done
    ln -s $HOME/.epkg/store $HOME/.epkg/opt/epkg/store
    mount --bind $HOME/.epkg/opt /opt

Another option is to create the whole root layout in $env/,
where you can create any dirs for mount.

<https://unix.stackexchange.com/questions/66084/simulate-chroot-with-unshare/303660#303660>

    unshare -r chroot <target_folder> <command_w_path>

However that will prevent apps from using user namespace:

<https://unix.stackexchange.com/questions/442996/what-rule-prevents-entering-a-user-namespace-from-inside-a-chroot>

## replace store dir to symlink (prefer)

After package installation,

    ln -s /tmp/epkg/store/$user

    if size($HOME/.e) < size(/opt/epkg/store)
        linkfile=$HOME/.e
    else
        linkfile=/tmp/epkg/xxxxx # should prevent /tmp auto-delete
    fi
    ln -s $HOME/.epkg/store $linkfile
    sed -i "s /opt/epkg/store/ $linkfile/ " $HOME/.epkg/store/new/files

However that will break md5sum integrity, which can be eliminated if we remove
the store path part from md5 computing (with a dedicated md5sum calculate tool).

## replace store dir to final dir

It will take some chars from the $hash part away, to make room for saving `$HOME/.epkg/store`
    sed -i "s /opt/epkg/store/ $HOME/.epkg/store " $HOME/.epkg/store/new/files

## embed and expand `${EPKG_STORE}`

- do not hardcode /opt/epkg/store in ELF apps/libs, use `"${EPKG_STORE}"` instead
- let the elf loader or ld-linux.so expand `"${EPKG_STORE}"`, however it looks hard/complex
- `${EPKG_STORE}` in non ELF/shell scripts can be hard to expand
