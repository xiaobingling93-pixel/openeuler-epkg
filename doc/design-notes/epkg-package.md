epkg-package
=============
[toc]

### 背景

为保障epkg软件包的快速上量，需要使用x2epkg(rpm2epkg)对现有软件生态的软件包进行批量转换，提升生成epkg软件的转换效率。本工程负责将现有rpm，archlinux，deb包等转换自动化，并且完成自动测试验证，安装，卸载，--help，保证epkg repo源的质量。

### 需求

- 基于rpm单包转换工具，生成openEuler，centos，fedora 仓库，并自动更新
- 对软件包进行自动测试验证，安装，卸载，--help
- 打通转换、测试、发布的pipeline全自动化流程

### 加包工程相关的代码仓

* [infrastructure: docker容器与pipeline服务脚本](https://gitee.com/openeuler/infrastructure)
  * 目录结构如下：
  ```
  - epkg_translate/                      <- epkg包批量转换和测试的pipeline服务源码目录>
    - translate/                         <- epkg包批量转换的源码目录>
    ├── container/
      ├── build_image.sh                 <- 构建转换docker镜像的脚本>
      ├── Dockerfile                     <- 转换的的docker描述文件>
      ├── start_container.sh             <- 构建转换docker镜像的脚本>
    ├── src/                             <- 批量epkg转换的源码目录>
      ├── translate-job.yaml             <- lkp-tests转换任务的描述文件>
      ├── schedule_main.py               <- pipeline批量任务的入口，由定时任务触发>
      ├── schedule_job.py                <- 单repo任务的调度入口，及批量任务的处理逻辑>
      ├── schedule_repos.py              <- 解析出哪些repos需要进行转换>
      ├── data_statistics.py             <- 结果数据统计任务>
      ├── reschedule_main.py             <- 为过程调试准备的脚本，如果已经转换过的repo也会再次被转换>
    ├── rpm-repos/
      ├── openeuler-24.03.toml
      ├── fedora-41.toml
      ├── openeuler-24.09.toml
      <!-- repo的配置文件参考 https://gitee.com/openeuler/epkg/commit/07f54a8f3797913666bbb670916b2aa54849505f 进行添加 -->
      <- 一个操作系统的一个版本对应着一个配置文件>
    - test/                              <- epkg包批量转换的源码目录>
    ├── container/
      ├── build_image.sh
      ├── Dockerfile
      ├── start_container.sh
    ├── src/
      ├── test-job.yaml
      ├── repos-path.yaml                <- 需要测试的临时epkg repo源的路径>
      ├── schedule_main.py
      ├── schedule_job.py
      ├── schedule_repos.py
      ├── data_statistics.py
  ```
* [lkp-tests: 单个repo构建job和测试job](https://gitee.com/compass-ci/lkp-tests)
  * 目录结构如下:
  ```
  - programs/epkg_translate/             <- epkg单包执行转换的源码脚本>
  ├── jobs
    ├── epkg_translate.yaml
  ├── meta.yaml
  ├── parse
  ├── run                                <- epkg单包执行转换的入口shell脚本>
  ├── rpm_down.py                        <- 批量从 repo url下载rpm包的脚本>
  ├── epkg_translate.py                  <- 使用rpm2epkg或x2epkg工具对rpm包进行转换>
  ├── result_deal.py                     <- 分析执行结果，将成功的epkg包上传至临时目录>
  - programs/epkg_test/
  ├── jobs
    ├── epkg_translate.yaml
  ├── meta.yaml
  ├── parse
  ├── run
  ├── result_deal.py                     <- 分析执行结果，将测试成功的epkg包发布repo源>
  ```
* [compass-ci: 分布式批量转换与测试的平台](https://gitee.com/openeuler/compass-ci)
  * 目前只是使用compassci的能力，暂时不会修改其代码


### 输入输出定义

**输入定义: rpm-repos 目录下的配置文件，以openeuler-24.03.toml为例**
  ```toml
  channel = "openEuler-24.03-LTS"
  arch = ["aarch64", "x86_64"]
  [repos.OS]
  baseurl = "https://repo.openeuler.org/openEuler-24.03-LTS/OS"
  [repos.update]
  baseurl = "https://repo.openeuler.org/openEuler-24.03-LTS/update"
  watch_update = true
  [repos.everything]
  baseurl = "https://repo.openeuler.org/openEuler-24.03-LTS/everything"
  [job]
  os = "openeuler"
  os_version = "24.03-LTS"
  ```

**输出定义:(具体存储位置待定)**
  - 正常可用的epkg repo源，比如: `https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64/`

### 实现思路

![整体思路](picture/epkg%20transform.jpg)
- epkg转换的加包流程，输入为 rpm repo 源的列表，输出为epkg转换成功并且可以正常使用的repo源列表，其中最核心的为转换和测试的两个后台任务。
  - 转换任务：批量地将rpm repos转换为临时的 epkg repos，每个repo对应一个compassCI的job；
  - 测试任务: 将转换后的epkg 临时repos批量转换为用户可用的epkg repos，每个repo对应一个compassCI的job；

实现约束：
- 全自动化服务使用docker容器化部署至z9管理节点
- 任务的入口和串联脚本集成至docker容器中，每天通过定时任务进行检测是否需要执行批量转换任务
- 按一个repo对应一个job，可能粗粒度地控制每天 repo 的个数进行流量控制，比如每天最多同时跑50个 repo 的转换
- 资源的分配与任务的调度使用compassCI的现有能力(最好是基于最新的compassCI代码部署的服务)
- 测试定时任务与epkg 发布脚本也集成至docker容器中，每天通过定时任务检查是否需要epkg包的测试，如果需要则将测试成功的epkg包发布至对应的repo源

#### rpm repo 源转换

- 范围：openEuler，centos，fedora 最新版本的所有 repo 源

rpm repo 源的感知主要分为一下两种情况：
1. **全量新增:** 第一次配置某个操作系统某个版本的rpm源，需要全量进行转换
   - 如果后续需要重新构建则调用 reschedule_main.py 的脚本手动触发
2. **存量更新:** 对每个操作系统的update的repo源进行diff监控，新增软件包才进行转换

转换任务 dokerfile 的定义:
```dockerfile
# 最好使用 openeuler:24.03-LTS 的镜像，工具链是最新的
from openeuler:22.03-LTS as BUILDER

WORKDIR /root

RUN sed -i "s|repo.openeuler.org|mirrors.nju.edu.cn/openeuler|g" /etc/yum.repos.d/openEuler.repo \
    && sed -i '/metalink/d' /etc/yum.repos.d/openEuler.repo \
    && sed -i '/metadata_expire/d' /etc/yum.repos.d/openEuler.repo

ENV CCI_SRC=/root/compass-ci
ENV LKP_SRC=/root/lkp-tests
ENV PATH $PATH:$LKP_SRC/sbin:$LKP_SRC/bin
ENV PIP_INDEX_URL=https://mirrors.aliyun.com/pypi/simple/

# https://gitee.com/openeuler/infrastructure.git 在这个本地目录下执行
COPY epkg_translate/translate .

RUN yum install -y git ruby rubygems make gcc diffutils util-linux lftp hostname sudo gzip git ruby-devel rubygem-json rubygem-bundler gcc-c++ ruby-devel rubygem-rake rpm-build python3-pip wget \
    && gem install rest-client \
    && git clone https://gitee.com/compass-ci/lkp-tests.git \
    && pip install schedule \
    && pip install requests

WORKDIR /root/epkg_translate/translate/src

CMD python3 schedule_main.py

```

**输入定义:  rpm-repos.yaml**
  - 全量repo源的yaml 文件，包含了新增repo源和存量待更行的update repo源, 即 rpm-repos.yaml 文件
  - 全量新增repo源的配置策略: 非 update 的源只做一次转换，如果确实有需要重做转换，通过 `reschedule_main.py` 单独执行该脚本进行转换, 相当于手动触发一次 rpm-repos 目录下配置文件的全量构建
  - 存量更新repo源的配置策略：update 的源做持续 diff 转换，先找到新增或转换失败的rpm包，再调用工具做增量转换

**输出定义: translate-job.yaml**
  ```yaml
  suite: epkg-translate
  category: functional

  group_id: epkg-translate

  program:
    epkg_translate:
      custom_repo_name: epkg
      mount_repo_name: epkg
      runtime: 1d
      strategy: {{main.strategy}}
      _epkg_repo_server: https://api.compass-ci.openeuler.org:20018
      _epkg_repo_dir: /rpm/testing/epkg/{{toml.channel}}

  os: {{toml.job.os}}
  os_version: {{toml.job.os_version}}
  os_arch: {{toml.arch}}
  ```
  - 需要批量进行转换的repo源信息(repos_list, arch, strategy)
  - strategy 暂定取值为: all(reschedule_main.py入口进行的job提交), delta(schedule_main.py入口进行的job提交)

- 全量新增功能实现思路：
  1. reschedule_main.py入口进行的job提交
  2. epkg 临时repo源是空的时，本质上还是一次全量的转换

- 存量功能实现思路：
  1. 直接通过配置文件中的 watch_update 进行判断, watch_update 为true的是存量任务
  2. 存量diff的rpm包的获取在job但任务里面进行处理，不在docker容器中进行处理
    - 先对比 rpm repo 源和 epkg 临时 repo 源的差异，找出新增的 repo 源
    - 如果 epkg 临时 repo 源不存在，则update下的所有rpm包都需要进行转换
    - 如果 epkg 临时 repo 源存在，则for循环获取差异，得到待转换的diff列表

#### 转换任务

转换任务本质就是利用epkg的工具链进行rpm包的转换，为了提高资源的使用效率，每个compassCI的job应该批量跑多个epkg包的转换，执行batch 转换。
![每个job的批量任务](picture/batch_transform_image.png)

- 功能实现思路：
  - 容器内服务实现思路
  ```bash
  # 解析 rpm-repos.yaml 配置文件，获需要在下一个周期进行转换的所有repo源信息
  need_repos = get_rpm_repos(repo_config=rpm-repos.yaml)
  # need_repos 为RepoInfo的数组
  class RepoInfo:
    base_url
    name
    # 这个参数是为了区分是否为update 源，是否做diff 增量转换
    stategy
    # 非update的源，在服务启动后的第一个周期执行完转换后，需要将该字段重置为 false
    force_onece

  # 遍历分批的rpm包列表，循环提交任务
  for repo_info in need_repos:
    if repo_info.stategy == "all" and repo_info.force_onece == false:
      continue
    submit job.yaml rpm_baseurl=repo_info.base_url repo_name=repo_info.name
                    traslate_strategy=repo_info.stategy
    count++
    sleep(30s)
    # 达到并发上限需要睡眠一段时间再执行，按repo控并发
    if count > 5:
        count = 0
        sleep(2h)
  ```
  - 在compassCI执行机上批量执行待转换的rpm包
  ```bash
  # epkg 工具链准备，环境初始化
  git clone https://gitee.com/rmp2epkg

  # 下载需要转换的repo源全量的rpm包并解压
  wget https://repo.openeuler.org/openEuler-24.03-LTS/OS/aarch64/repodata/*primary.xml.zst && zstd -d primary.xml.zst
  # 解析出rpm包的列表
  rpm_list = xml.etree.ElementTree primary.xml
  # 获取需要转换的rpm列表
  if traslate_strategy == "delta":
    # 增量 update 更新转换场景
    # 获取临时转换成功的rpm的列表
    # 可以参考https://gitee.com/openeuler/epkg/blob/master/doc/epkg-format.md，里面有类似RPM repodata的index.json，用里面的store-paths里的软件包清单文件
    # 需要解析 repodata/store-paths.txt.zst 这个源数据
    rpm_success_list = getOldSuccessRpmList(epkg_path={{job.mount_repo_addr}})
    delta_list = getNeedTransformRpmList(rpm_list, rpm_success_list)
    batch_list = delta_list
  else:
    batch_list = rpm_list
  # 遍历batch_list
  for one_rpm in batch_list：
    # 获取repo源和下载rpm包
    wget https://repo.openeuler.org/openEuler-24.03-LTS/OS/aarch64/packages/xxx.rpm（one_rpm）
    # 使用 rpm2epkg 工具将二进制转换为 epkg
    rpm2epkg -i rpm_path -o epkg_path
    # 检查转换成功的epkg包，并将epkg上传至对应的epkg repo（临时的repo路径）
    upload epkg_path=/tmp/xxx.epkg epkg_repo=OS os_version=openEuler24.03 arch=aarch64
  ```

#### 测试任务

测试任务本质就是利用epkg包管理器对转换后的rpm包进行安装、--help、卸载测试，为了提高资源的使用效率，每个compassCI的job应该批量跑多个epkg包的测试，执行 batch 测试。
![批量测试任务](picture/test_image.png)
- 功能实现思路：
  - 容器内服务实现思路，测试任务的分批逻辑和待测epkg列表的获取和转换任务基本类似；待测试的epkg列表需要利用转换后的临时存储与全量以发布的epkg源对比，得到增量待测试的epkg包列表。
  - 在compassCI执行机上批量执行epkg包的测试
  ```bash
    # epkg 工具链准备，环境初始化
    git clone https://gitee.com/openeuler/epkg.git
    # 仅root用户可使用global安装模式
    wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
    sh epkg-installer.sh

    # 初始化epkg
    epkg init --url=https://tmp/epkg-repo
    bash // 重新执行.bashrc, 获得新的PATH

    # 创建epkg的测试环境1
    epkg env create t1

    # 遍历batch_list
    for one_epkg in batch_list：
        epkg install one_epkg
        one_epkg --help
        epkg uninstall one_epkg
        # 检查安装测试是否成功，成功后的epkg包上传至public路径进行发布
        upload epkg_path=/public/xxx.epkg epkg_repo=OS os_version=openEuler24.03 arch=aarch64

    # 清理掉epkg的环境
    epkg destroy env t1
  ```

#### epkg repo 源上传

- 将测试成功的epkg包同步至外网可以正常访问的web服务器
- 策略采取push的模式，需要启动一个定时任务进行全量或增量更新公开的 epkg repo 源

#### 方案待讨论点

1. 前期x2epkg工具没有ready，使用rpm2epkg打通加包pipeline流程时，以一个 repo 对应一个job进行？
   - 对齐结论：x2epkg 与 rpm2epkg 均以单repo粒度进行包转换
2. 需要大量下载 rpm repo，是否需要做rpm的镜像源？如果需要，会有一个单独的任务做repo源镜像？
3. 是否需要做增量感知？是否还有比 rpm发布全量 - epkg发布全量 = epkg_delta 更好的方法？
4. 为了避免资源的过度占用，每个job批量执行500个epkg的转换或测试是否合理？
5. 临时过程文件比如转换后的epkg包、下载的rpm、最后安装测试成功的epkg包是否用单独的 ftp 服务器进行存储？
6. 目前以rpm2epkg作为批量工具，怎么将临时的epkg路径使用起来，用epkg 工具链对临时 repo 做测试？
