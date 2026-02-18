# Repositories (channels)

epkg uses **channels** to refer to a distribution and version (e.g. `debian`, `ubuntu`, `alpine`, `fedora`, `openeuler`, `archlinux`, `conda`). Each channel has a default version and one or more **repos** (e.g. main, community, extra). Mirrors are chosen automatically; the placeholder `$mirror` in index URLs is replaced by a mirror from [sources/mirrors.json](https://atomgit.com/openeuler/epkg/tree/master/sources/mirrors.json).

## List channels and repos

```bash
epkg repo list
```

Output is a table with columns:

- **channel** — Channel name (and possibly variant, e.g. `fedora/rpmfusion`).
- **default version** — Default distro version (e.g. `13`, `3.23`, `latest`).
- **repos** — Comma-separated repo names (e.g. `main,community`, `Everything`).
- **index_url** — Template for the index URL; `$mirror`, `$version`, `$arch`, `$repo` (and sometimes `$conda_arch`, `$conda_repofile`, `$app_version`, etc.) are substituted.

Example (abbreviated):

```
--------------------------------------------------------------------------------------------------------------------------------------------
channel              | default version | repos                                         | index_url
--------------------------------------------------------------------------------------------------------------------------------------------
alpine               | 3.23            | community,main                                | $mirror/v$version/$repo/$arch/APKINDEX.tar.gz
archlinux            | latest          | core,multilib,extra                           | $mirror/$repo/os/$arch/$repo.files.tar.gz
archlinux            | latest          | aur                                           | https://aur.archlinux.org/packages-meta-ext-v1.json.gz
conda                | latest          | free,r,main,pro                               | $mirror/pkgs/$repo/$conda_arch/$conda_repofile
conda                | latest          | conda-forge,pytorch,MindSpore                 | $mirror/cloud/$repo/$conda_arch/$conda_repofile
debian               | 13              | Official                                      | $mirror/debian/dists/$version/Release
fedora               | 43              | Everything                                    | $mirror/releases/$version/$repo/$arch/os/repodata/repomd.xml
openeuler            | 25.09           | update,EPOL/update/main,everything,EPOL/main  | $mirror/openEuler-$VERSION/$repo/$arch/repodata/repomd.xml
ubuntu               | 25.10           | Official                                      | $mirror/dists/$version/Release
...
--------------------------------------------------------------------------------------------------------------------------------------------
```

## Using a channel

When you create an environment with `-c CHANNEL` (e.g. `epkg env create myenv -c alpine`), that env is bound to that channel and its default version. Package operations (`install`, `update`, `upgrade`, `list`, `search`, `info`) then use the channel’s repos and mirrors.

Metadata is cached under `~/.cache/channels/` (user) or `/opt/epkg/cache/channels/` (root). Use `epkg update` to refresh it.

## Adding or changing channels

Common pre-defined channel definitions live in the epkg source tree (e.g. under `sources/`). To add or change a channel, you need to add or edit the corresponding source config and rebuild or redeploy epkg. The exact format is distribution-specific (YAML, repo definitions, etc.). See the repository and [design-notes/repodata.md](../../design-notes/repodata.md) for details.
