# 2 kind of OS channels for end user

- rolling channel, 使用每个上游软件的正式版本
- yearly channel, 6月份自动freeze, 后续跟进上游软件的小版本更新, cve update

# 2 kind of OS repos in each channel

- os repo, for end user
- dev repo, for developer/test/verify

# 组合测试探索可行空间, 多版本基线构建出包

```
os_channels = [rolling, ch24, ch23, ch22]

on a upstream software git's new commit:
    for channel in os_channels:
        if commit is formal version
            if channel != 'rolling' and version != 小版本更新
              continue
          repo = 'os'
        else:
          repo = 'dev'

        build the package, and rdepends packages
        if success:
            add package to $channel/$repo
        else:
            bisect and email bug report
```





## 软件分发

```
将epkg放入repo中
1. 提取epkg的package.json放入repo
2. 存放epkg到store
3. 更新store-path
```

