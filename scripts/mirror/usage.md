
```shell
python3 -m venv .venv

./.venv/bin/pip3 install -r requirements.txt
sudo apt-get install geoip-database chromium-driver lftp

# Step 1: Fetch new mirrors from various sources
# output: new-mirrors.json
./.venv/bin/python3 fetch_new_mirrors.py

# Step 2: List directory contents from mirrors
# input: ls-mirrors.json (accumulated data) + new-mirrors.json + DISTRO_CONFIGS
# output: ls-mirrors.json
./.venv/bin/python3 ls_mirrors.py

# Step 3: edit manual list, you may start from
wfg /c/epkg/scripts/mirror% grep '^#' recommend.log >> /c/epkg/channel/mirrors.yaml

# Step 4: Merge all mirror data into final JSON
# input: new-mirrors.json + ls-mirrors.json + ../../channel/mirrors.yaml
# output: ../../channel/mirrors.json
./.venv/bin/python3 merge_mirrors.py
```

## Notes

- **ls_mirrors.py**: This script fetches directory listings from mirror URLs and saves them to `ls-mirrors.json`.
- **merge_mirrors.py**: Now also merges the `ls` field from `ls-mirrors.json` into the final mirrors.json output.
- The `ls` field contains filtered directory names that match the distro configurations (`DISTRO_CONFIGS`).
- Directory listings are filtered to only include directories that match known distro names or distro_dirs from the YAML configurations.
