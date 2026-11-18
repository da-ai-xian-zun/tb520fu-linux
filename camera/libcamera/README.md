# 相机：libcamera 补丁

基于上游 libcamera `08c7982`（0.8.0），只编译 `simple` 管线和软件 ISP。

| 补丁 | 内容 |
|---|---|
| `0001` | `simple` 管线的手动对焦控制（来自 iceland-mainline 项目的 Osilvfe） |
| `0002` | GC13A0、GC13A2、SC202CS、SC820CS 传感器的增益换算，让软件 ISP 能使用完整的自动曝光算法 |
| `0003` | 对焦马达只在出图时通电 |
| `0004` | 软件 ISP 从调校文件读取默认的伽马、对比度和饱和度 |
| `0005` | 软件 ISP 统计中计算对焦评分 |
| `0006` | `simple` 管线的对比度自动对焦：默认连续对焦，也支持单次对焦和手动对焦 |

其他文件：

- `pipewire-1.6.9-libcamera-0.8.patch`：让 PipeWire 1.6.9 的 libcamera 插件能用 0.8 编译；
- `51-tb520fu-cameras.conf`：WirePlumber 配置，给三颗摄像头起易懂的名字；
- `build.sh`：在平板上编译安装整套相机组件，见 [从源码构建](../../docs/building.md#相机)。

调校文件在 [`../tuning/`](../tuning/)。已知限制：还没有镜头阴影校正；白平衡是灰度世界算法，大面积单色场景会偏色；对焦只看画面中央，不支持点按对焦。
