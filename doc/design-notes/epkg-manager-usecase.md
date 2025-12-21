

# epkg包管理器使用说明

## use case

### usecase1: 安装软件包 并使用

安装：yum install epkg

初始化： epkg self install

使能：bash

安装： epkg install xxx



### usecase2: 安装多版本 软件包

创建新环境： epkg env create $env_name --repo xxx

激活环境：epkg activate $env_name

安装：epkg install xxx



### usecase3: 切换环境





## epkg命令功能

### 安装

curl -O download.sh | bash -

```
实现epkg环境安装，部署；创建默认的epkg环境，修改bashrc
```



yum install epkg

````
安装epkg环境
````

### 初始化

epkg self install

```
创建epkg默认环境，修改bashrc
默认环境中，使用使用默认的channel。
```


### 软件包操作

epkg install $package

```
epkg在指定环境中，安装软件包：
1. 获取env下的channel配置，基于channel更新repo cache
2. 基于cache query 查询$package的所有依赖
3. 解压并连接到环境
```

epkg remove $package
```
在当前的环境中，去除$package的相关软件链接及文件
```

epkg upgrade

```
查看当前环境中的软件包，并进行版本更新
```



epkg search $package

```
搜索当前环境配置的repo中，软件包都有哪些
```



epkg list

```
列出当前环境中的所有软件包  # 需要env的db支持
```



### 环境操作

epkg env list

```
列出当前用户下的所有环境
```



epkg env create --repo xxxxx --repo  yyy  $env_name

```
创建环境，
```



epkg env remove $env_name

```
环境删除
```



epkg env enable/disable  $env_name

```
全局使能或去使能环境
```



epkg env activate/disactivate   $env_name

```
激活环境，去激活环境
```





### 管理命令

#### 软件源相关

epkg repo list

```
查看channel.json配置，列出所有channel和repo
```

epkg repo add xxx

epkg channel add xxx

#### 垃圾回收



