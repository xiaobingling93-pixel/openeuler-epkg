import datetime
import yaml

SYSTEM_PKG_FORMAT_MAPPING = {
    "openeuler": "rpm",
    "fedora": "rpm",
    "centos": "rpm",
    "opensuse": "rpm",
    "rockylinux": "rpm",
    "debian": "deb",
    "ubuntu": "deb",
    "alpine": "apk",
    "archlinux": "archlinux",
    "conda": "conda"
}


class IndexJson:
    def __init__(self):
        self.json_data = {
            "store-paths": [{"filename": "store-paths.zst"}],
            "pkg-info": [{"filename": "pkg-info.zst"}],
            "pkg-files": [],
            "origin_time": "",      # 溯源的url的repodata的时间
            "create_time": "",      # 执行create-repo的时间
            "channel": "",
            "repo": "",
            "depend_repos": [],
            "origin_repo_url": "",
            "origin_package_format": "rpm|deb|apk|conda|archlinux"
        }

    def get_create_time(self):
        self.json_data["create_time"] = datetime.datetime.now().strftime("%Y%m%d-%H:%M:%S") + " +0800"

    def get_index_json(self, config_path):
        with open(config_path, "r") as f:
            config_data = dict(yaml.safe_load(f.read()))

        # channel的值全小写拼写，且系统和版本号之间用冒号连接
        if "channel" in config_data:
            self.json_data["channel"] = config_data["channel"].lower().replace("openeuler-", "openeuler:")

        # 获取repo值
        self.json_data["repo"] = config_data.get("repo")

        # 添加repo源清单
        self.json_data["depend_repos"] = config_data.get("repos")
        if isinstance(self.json_data["depend_repos"], list) and self.json_data["repo"] in self.json_data["depend_repos"]:
            self.json_data["depend_repos"].remove(self.json_data["repo"])

        # 获取原始repo源地址
        self.json_data["origin_repo_url"] = config_data.get("origin_repodata_url")

        if "os" in config_data:
            self.json_data["origin_package_format"] = SYSTEM_PKG_FORMAT_MAPPING.get(config_data["os"].lower())
        self.json_data["origin_time"] = config_data.get("origin_time")
        self.get_create_time()
        return self.json_data
