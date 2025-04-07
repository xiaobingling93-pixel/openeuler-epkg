# requirements

developers/users want both flexibility and reproducibility
- at pip install time, want libsolv to choose depends versions according to their requirements
- at distribute time, want pip freeze into requirements.txt to reproduce in 3rd party env

- github software POV: need flexible combination for different projects
- develop project POV: need reproducibility, via env, lockfile, hash export/import

# 2 types of depends

## fixed   pkgname + hash
- suitable for ELF NEEDED lib.so
- not suitable mechanism for python depends
- at invoke time: need search hash for updated libs

Comparing to RPM, Nix turns runtime depends to build time depends, e.g.
`sed -e "s| awk | ${gawk}/bin/awk |"`
- pro: scalable by avoiding global version range conflicts, or dimond-depends
- con: maintain costs on per-app fixups

global libsolv algorithm is no longer applicable for ELF lib.so depends:
/opt/epkg/store/hash per-dir multi-version and rpath fixed the dimond-depends
problem, which eliminates version range conflicts in another dimension

## libsolv pkgname + version range
- suitable for PATH cli command, PYTHONPATH python module
- at invoke time:
  - prepend current $env/bin to PATH: work for 'cmd', wont work for '/bin/cmd'
  - bind mount /bin: work in linux, WSL2, wont work in WSL, mac?

# optional depends
- conditional: +dep if selinux
- alternative: dep1|dep2, e.g. mawk|gawk, vim|nvim

# TODOs

https://conda-forge.org/
https://anaconda.org/conda-forge

scipy 2019: want nix improvements for python
- allow imperative installation of python packages
- maintain multiple versions of major packages in ixpkgs
- declarative standard for python instead of setup.py, setup.cfg, requirements.txt, MANIFEST.in etc.

# epkg + pypi
- epkg for fixed c/c++ hash
- multi-version python/gcc etc. in one repo
- make local 'epkg build' fast/easy:
  - source yaml is depends in version range
  - + lock file is version baseline, =>实例化构建
  - binary is reusable hash
  - how to auto get lock file? libsolv.. OS default baseline
    <https://wiki.nixos.org/wiki/Python>
- replace pyenv/virtualenv/conda, deep integration with pip
  - <https://ioflood.com/blog/pyenv/>
  - <https://prefix.dev/blog/pixi_a_fast_conda_alternative>
  - <https://prefix.dev/blog/pixi_launch>
  - <https://taras.glek.net/post/trying-pixi-modern-python-packaging/>
  - <https://pip.pypa.io/en/stable/cli/pip_install/>
  - <https://pip.pypa.io/en/stable/topics/vcs-support/>
- how to ensure OS self-contained depends for python modules, from other OS packages?
- how to meet AI project requirements like conda?

# epkg env for C+python projects

```
epkg env create mypyproj
epkg env activate mypyproj
epkg install C packages
epkg install python packages
epkg install pip
pip install pypi modules
epkg env export # C+python+pypi
epkg env import
```
