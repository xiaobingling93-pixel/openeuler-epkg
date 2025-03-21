# epkg list

## epkg list: data source

Auto download all available repodata store-paths.zst file, cache/unpack locally:

```
wfg@crystal ~/.cache/epkg/channel/openEuler-24.09/everything/aarch64/repodata% head store-paths
ZHNXZNVU2HAGMX4FBAFGF4JVH3LGZB2J__texlive-xdvi__20210325__8.oe2409
ZHBBEFHR6TO7BWWH7JFAXAKNKKTFJX67__pcp-selinux__6.2.2__2.oe2409
ZHCF7QQHK2H35B6ME65EPIARWNCZS7LL__podman-gvproxy__4.9.4__8.oe2409
```

## epkg list: arg forms

```
epkg list xxx
=>
grep __xxx__ store-paths

epkg list '*xxx'
=>
grep xxx__

epkg list 'xx*x'
=>
grep -o __xx.*x__
remove if contain __
```

## epkg list: output form

```
channel            pkgname              version-release     hash
============================================================================================
openEuler-24.09    texlive-xdvi         20210325-8          ZHNXZNVU2HAGMX4FBAFGF4JVH3LGZB2J
openEuler-24.09    pcp-selinux          6.2.2-2             ZHBBEFHR6TO7BWWH7JFAXAKNKKTFJX67
openEuler-24.09    podman-gvproxy       4.9.4-8             ZHCF7QQHK2H35B6ME65EPIARWNCZS7LL
```

## epkg install: auto match channel

Only apps from the same channel can be installed into one env.
So epkg install should only search apps in the same channel.

If not found, then try other channels and if found there, list all available,
recommend user to switch to the matching env or create new env.
