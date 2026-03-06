# Getting started

This guide walks you through installing epkg and running your first package operations.

## Installing epkg

```bash
wget https://raw.atomgit.com/openeuler/epkg/raw/master/bin/epkg-installer.sh
bash epkg-installer.sh

# Then start a new shell so PATH is updated
bash
```

## First environment and package

By default you have a `main` environment. Create another environment with a specific channel and install packages there.

1. **Create an environment** (e.g. Alpine):

   ```bash
   epkg env create myenv -c alpine
   ```

   Example output:

   ```
   Creating environment 'myenv' in $HOME/.epkg/envs/myenv
   ```

2. **Install packages in that environment**:

   ```bash
   epkg -e myenv install bash jq
   ```

   You will see a dependency plan and download/install progress. Example summary:

   ```
   Packages to be freshly installed:
   DEPTH       SIZE  PACKAGE
   0       469.7 KB  bash__5.3.3-r1__x86_64
   0       147.9 KB  jq__1.8.1-r0__x86_64
   0       520.1 KB  coreutils__9.8-r1__x86_64
...
   Packages to be exposed:
   - jq__1.8.1-r0__x86_64
   - bash__5.3.3-r1__x86_64
   0 upgraded, 19 newly installed, 0 to remove, 2 to expose, 0 to unexpose.
   Need to get 4.6 MB archives.
   After this operation, 11.0 MB of additional disk space will be used.
   ```

3. **Run a command from that environment**:

   ```bash
   epkg -e myenv run jq --version
   # e.g. jq-1.8.1
   ```

   Or register the environment so its binaries are on your PATH for daily cli use:

   ```bash
   epkg env register myenv
   # epkg() will auto run: eval "$(epkg env path)" to update PATH in your current shell
   jq --version
   ```

## Verify installation

- List environments: `epkg env list`
- List packages in an env: `epkg -e myenv list`
- Show env PATH: `epkg env path`

## Common workflows

### Use packages from different distributions

Create separate environments for different channels and register them:

```bash
epkg env create debian-env -c debian
epkg env create alpine-env -c alpine-3.23
epkg env register debian-env
epkg env register alpine-env
# Now both envs' binaries are in PATH, in register order
export PATH="$HOME/.epkg/envs/debian-env/ebin:$HOME/.epkg/envs/alpine-env/ebin:..."
```

### Project-specific environment

For a project that needs specific packages, create an environment at the project root:

```bash
cd /path/to/myproject
epkg env create --root ./.eenv -c alpine
epkg --root ./.eenv install python3-pip  # --root is optional if you are under project dir
epkg run ./script.py
```

Next steps: [Environments](environments.md), [Package operations](package-operations.md), [Advanced usage](advanced.md).
