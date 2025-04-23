# Command: python3 -m unittest unit_test.py
import json
import os
import unittest

output_dir = "/tmp/x2epkg_unittest"

def get_origin_url_version(pkg_dir):
    with open(f"{pkg_dir}/package.json") as f:
        content = f.read()
    json_data = json.loads(content)
    return json_data.get("originUrl"), json_data.get("version")


def execute_testcase(package):
    tarball_url, version = get_origin_url_version(package)
    basename = os.path.basename(package)
    tarball_name = os.path.basename(tarball_url)
    status = os.system(f"wget {tarball_url}")
    if status != 0:
        print("download file failed")
    result = os.system(
        f'cd .. && bash x2epkg.sh "test/{tarball_name}" --out-dir {output_dir} --origin-url "{tarball_url}"')
    if result != 0:
        return 0, 1
    executed = os.system(f'find {output_dir}/store -type f -name "*{basename}*{version}*" ' + '-exec tar --zstd -xf {} \;')
    if executed != 0:
        return 1, 0
    with open("info/package.json", "r") as f1:
        content = f1.read()
    with open(f"{package}/package.json", "r") as f:
        expecting = f.read()
    return content, expecting


class TestTrans(unittest.TestCase):
    def setUp(self):
        pass

    def assert_msg(self, value1, value2):
        if value1 == 0:
            msg = "Run x2epkg error."
        elif value2 == 1:
            msg = "Can't decompress epkg file."
        else:
            msg = "Not same to expect package.json "
        self.assertEqual(value1, value2, msg=msg)

    def test_deb2epkg(self):
        content, expecting = execute_testcase("deb/ncurses-base")
        self.assert_msg(content, expecting)

    def test_rpm2epkg(self):
        content, expecting = execute_testcase("rpm/acl")
        self.assert_msg(content, expecting)

    def test_archlinux2epkg(self):
        content, expecting = execute_testcase("archlinux/acl")
        self.assert_msg(content, expecting)

    def test_apk2epkg(self):
        content, expecting = execute_testcase("apk/aaudit-server")
        self.assert_msg(content, expecting)

    def test_conda2epkg(self):
        content, expecting = execute_testcase("conda/_pytorch_select")
        self.assert_msg(content, expecting)

    def tearDown(self) -> None:
        os.system("rm -f *.conda *.apk *.rpm *.deb *.pkg.tar.zst")
