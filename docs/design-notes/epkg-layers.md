
# 基于分层定制, 构建多个epkg仓

1. 建一个git, 放spec2yaml 6k包源码
2. 建一个层，以分层定制的方法fix上述源码，不直接修改它们
3. 建一个层，加muslc定制
4. 建一个层，加llvm定制

这个技术面广，需要多团队协作。先打通基本流程，然后各层的成功率依靠各个团队和社区贡献推动。

1+2, 构建出 epkg 版 openEuler核心仓, 以及 DevStation 2.0 iso
1+2+3, 构建出 epkg 版 openEuler嵌入式仓，以及 openEuler容器镜像仓
1+2+3+4, 实施 llvm 平行宇宙计划，支持交叉编译

# 后续演进计划

spec2yaml输出
- 用 %make 的spec, 可以注入定制项到%make macro
- 用 make 的spec, spec2yaml 应当注入一个宏到命令行，实现yaml可定制
- 考虑到会存在一段时期，需要对开发者维护友好

为兼容性考虑，spec2yaml出来的yaml，设定`build_system=rpmspec`，
在构建期展开rpm macros，然后epkg build编译，出epkg包。

未来一个个软件包可以改写其yaml
- set `build_system=cmake`
- remove all rpm macros
来实现yaml/epkg原生化
