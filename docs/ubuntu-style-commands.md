# ElaraOS 初始化后命令与模块开发对照

> 按 Ubuntu 常见命令组织操作入口；区分当前可用能力与待实现目标，供其他会话结合模块总览选择开发增量。

## 1. 权威入口、范围与证据

- **核对日期**：2026-11-15。内部 VFS 命令见[本批](#internal-vfs-commands)，应用/缓存命令见[资源生命周期](#resource-commands)。独立资源已通过489项完整回归；应用关联增量20项专项已通过，新增验收中。完整能力与最新结果以[文件系统单一账本](filesystem.md)为准；408/421/432是历史快照。
- **项目入口**：[README](../README.md) 管理工程状态、启动验收与文档索引。
- **模块职责**：[完整操作系统模块总览](operating-system-modules.md) 解释底层模块及依赖。
- **命令账本**：本文件维护初始化后命令的可用性、实际语义、Ubuntu 风格目标及命令到模块的映射；新增命令必须同步本文件、程序 `help` 和 README。
- **源码依据**：[命令分发](../src/shell.rs)、[受限文件访问](../src/filesystem.rs)、[启动入口](../src/main.rs)、[Rust 初始化](../src/initialization.rs)、[集中配置与目录布局](../src/config.rs)。
- **范围**：ElaraOS 自己的命令环境，不是 Windows PowerShell，也不是宿主 Ubuntu 的 Bash。下文命令输入到 ElaraOS 中；不要把未来的管理命令复制到宿主执行。
- **定位**：当前为使用宿主标准库的 Rust 命令行原型，不是独立内核。Ubuntu 风格意味着先参考命令名称与常见用法，不承诺 Bash、POSIX、GNU 工具参数或 Linux ABI 完整兼容。
- **历史运行证据（2026-11-08）**：`cargo test --offline --target-dir target/shell-build -- --test-threads=1 --nocapture`：169模块单元+145脚本功能+6实际进程/ConPTY，**320全部通过，exit 0**；源码/测试前后一致。共同前端/嵌套/常用语法和M5流式/合作式作业已功能验收，2MiB管道与取消130已真实运行。**严格Clippy仍未通过**：FS的ownership.rs/ownership_model.rs三处lint，未改对方文件或放宽检查。详细证据和必要性对照由[脚本专题](shell-scripting.md#8-验收记录)维护；275/236/190等只作历史不累计。文件系统语义见[文件系统专题](filesystem.md)，句柄见[句柄命令](file-descriptors.md)。Linux原生、真实断电和全部敌对并发未验证。

宿主权限限制见[硬边界](filesystem.md#host-impact-boundary-2026-11-09)；内部挂载及根内普通镜像现可使用 vfs 命令，不提供宿主 format/mount、设备或外部网络操作。

最新四项权限/生命周期实现、严格 Clippy 通过及运行测试权限阻塞见[当前文件系统账本](filesystem.md#directory-access-task-2026-11-09)。上一阶段受管 fd、跨命令对象授权、cp/mv 归属和质量门见[文件系统单一账本](filesystem.md#owned-open-2026-11-09)；2026-11-08 的 Clippy 未过已改判为历史，不代表当前质量门。

### 1.0 运行方式

在项目目录执行 `cargo run`。Cargo 在项目 `target` 中构建；程序启动后读取 `.env` 并由 Rust 初始化虚拟根。虚拟根不存在时也可以先构建，运行时再创建。

### 1.1 状态定义

| 状态 | 含义 | 其他会话应如何处理 |
|---|---|---|
| 当前可用 | 命令已经接入实际入口；本轮运行覆盖情况见第 8 节 | 在注明的语义边界内使用，未覆盖能力不能自行推定 |
| 部分可用 | 已有命令，但与 Ubuntu 的常见能力有差异 | 只使用明确列出的子集，先补契约再扩展 |
| 待实现 | 当前入口没有此能力 | 用于选模块和定验收，不得报告为初始化后已经能运行 |

**本表列三十四个常用系统/会话命令：`help`、`info`、`pwd`、`cd`、`ls`、`cat`、`mkdir`、`touch`、`rm`、`rmdir`、`mv`、`cp`、`stat`、`find`、`du`、`echo`、`sleep`、`ps`、`wait`、`jobs`、`fg`、`bg`、`kill`、`whoami`、`users`、`useradd`、`passwd`、`su`、`log`、`sh`、`source`、`fd`、`clear`、`exit`。`.` 为 `source` 别名，`&` 是语法操作符，`fd` 子命令不重复计。共同语言另有 `printf/wc/test/true/false/export/:` 等内建；新增对象归属桥以文件系统账本为准，不将三十四误称全部可解析命令总数。创建了 Ubuntu 风格目录，不等于对应内核、服务或工具已经实现。**

`owned` 是额外的受管普通对象接口，不计入上面的 34 个常用命令子集；语法见下表与文件系统账本。

交互与脚本共用词法/AST/求值入口，支持常用语法、嵌套 `sh/source`、适用内建阶段的并发流式管道和作用域 `&` 作业。非适用阶段显式回退到64KiB缓冲；不是完整 Bash、内核 IPC 或进程组，每次提示符输入仍要求完整单行程序。

### 1.2 五模块增量（2026-11-07）

| 模块 | 新增命令 / 能力 | 边界 |
|---|---|---|
| 文件读写层 | `cat`、`mkdir`、`touch`、`rm`、`rmdir` | 只在虚拟根内；拒绝 `rm /`、系统目录、`..`、宿主路径、链接、设备名；`rm` 对目录、`rmdir` 对非空目录给出精确错误 |
| 交互命令解析器 | 共用脚本前端，引号、变量、连接符、管道与重定向 | 旧单行解析器不再参与普通命令分发；不支持的语法整行拒绝 |
| Shell 脚本 | `sh/source/.`、常用语法、并发流式阶段、有界回退、作用域作业 | 不执行宿主程序；流式与回退策略/成本、cooperation/身份边界见[脚本专题](shell-scripting.md) |
| 虚拟执行入口与身份 | `exec FILE [ARGS]` | 仅受管UTF-8 Elara脚本，检查x/ACL/绑定后执行不可变映像；成功替换当前虚拟脚本边界，失败保留调用环境；不执行宿主程序；文件setuid/SGID改变虚拟有效身份，`id`显示真实/有效/保存UID/GID；sh/source不提权；[边界与验收](filesystem.md#execution-identity-2026-11-14) |
| 文件句柄命令 | `fd open/read/readat/write/writeat/seek/truncate/stat/dup/same/lock/unlock/sync/close/closeall` | 由文件系统实现IO，ModuleCommands薄适配；打开检查账号，表绑定uid，身份切换关闭能力 |
| 受管普通对象 | `owned create/read/write/append/mv/rm/chmod/stat` | 新对象使用会话 umask（默认0600），旧数据不自动认领；目录、共享组/access ACL、任务临时回收已接入，严格 Clippy 已通过，当前目录setgid快照390/390通过；[当前FS账本](filesystem.md#directory-access-task-2026-11-09) |
| 会话创建策略 | `umask [MASK]` | 默认0077；无default ACL时文件0666、目录0777分别扣mask；有模板时只按requested mode裁剪；source保留、sh隔离、后台继承、su重置；当前目录setgid快照390/390通过 |
| 虚拟访问元数据 | `chmod MODE PATH`、`chgrp GID PATH`、`getfacl PATH`、`setfacl SPEC PATH`、`setfacl -b PATH`、`setfacl -d SPEC DIR`、`setfacl -k DIR` | 受管文件/目录；目录MODE额外支持sticky 1000/setgid 2000，普通文件额外支持setuid 4000/setgid 2000，写内容先清06000；文件sticky与目录setuid拒绝；[sticky当前验收](filesystem.md#sticky-2026-11-09)；access ACL/mask及目录default ACL继承专项与真实CLI已通过，当前目录setgid快照390/390通过；虚拟权限不映射宿主chmod；[增量协议与字面子集](filesystem.md#default-acl-2026-11-09) |
| 持久共享组 | `groupadd GID NAME`、`groupmem add\|del GID UID`、`groupmem primary GID UID`、`groups` | 管理root-only；按主组/成员授权，数字ID为字面值 |
| 临时生命周期 | 无清理命令 | 新任务作用域/tmp对象结束回收，启动只回收有归属证明的死租约；旧/未知数据保留 |
| 任务模块 | `sleep N [bg]`、`ps`、`wait ID` | 任务是程序内部对象，`ps` 不列出宿主进程，不能操作宿主进程 |
| 用户与权限 | `/etc/passwd`、`/etc/shadow`、`whoami`、`users`、`useradd`、`passwd`、`su` | `root`（uid 0）是唯一管理员；密码为加盐 SHA-256 多轮哈希；普通用户不能读 `/root` 和 `/var/log`，只能写自己的 home 和 `/tmp` |
| 日志模块 | `/var/log/elara.log`、`log [N]` | 启动和每条命令都记录；`su/passwd/useradd` 不记录密码；仅 root 可查看 |

### 1.3 权限模型

| 主体 | 读 | 写 |
|---|---|---|
| `root` | 合法虚拟路径（文件系统保留空间除外） | 可请求写入，但必须通过文件系统写入分类；系统只读区与未分类路径不能写，系统目录本身不可删除 |
| 普通用户 | 除 `/root`、`/var/log`、`/etc/shadow` 外的路径 | 仅自己的 home（`/home/<name>`）和 `/tmp` |

路径规则之外，受管普通文件另检查所有者/mode 与归属绑定；fd 新建和 cp 新目标登记发起者，gid==uid 仅为私有主组。fd stat 的 owner_uid 是会话主体，uid 是对象所有者。

Windows 路径的权限比较同时覆盖大小写别名，例如 `/ROOT`、`/VAR/LOG`、`/ETC/SHADOW` 不能绕过规则；其他平台保持本地大小写语义。

`su <name>` 由 root 执行时不需要密码；普通用户切换账户必须提供密码：`su <name> <password>`。切换后若当前目录对新用户不可读，自动回到 `/`。

<a id="resource-commands"></a>
### 1.4 应用与缓存资源生命周期

以下为 ElaraOS 内部命令，不是宿主软件安装器；除 help 外均限管理员。登记只创建全新目录，不导入、扫描认领或覆盖已有内容。所有配额参数为十进制整数：BYTES 为1..67108864，OBJECTS 为1..4096，根目录本身计1对象。最多登记64个资源。

| 字面语法 | 实际语义 |
|---|---|
| `app register PATH BYTES OBJECTS` | 在 `/opt` 或 `/srv` 下新建并持久登记应用资源根；不是下载/安装软件包 |
| `app list` / `app stat PATH` | 列应用记录 / 查看记录及当前登记对象逻辑用量 |
| `app quota PATH BYTES OBJECTS` | 改配额；低于当前用量拒绝；既有FD下一次写也受新配额约束 |
| `app cache APP PATH BYTES OBJECTS` / `app data APP PATH BYTES OBJECTS` | 为已登记应用原子创建关联缓存/数据目录；数据位置限home用户子目录或root子目录，仍由管理员拥有和控制 |
| `app locations APP` | 列出该应用绑定的缓存/数据资源位置、ID和配额 |
| `app uninstall PATH` | 一次持久事务冻结应用及所有关联位置，再先子后父逐项回收；未知或被替换对象保留，整组支持重启续办 |
| `cache register PATH BYTES OBJECTS` | 在 `/var/cache` 下新建并持久登记缓存资源根 |
| `cache list` / `cache stat PATH` / `cache quota PATH BYTES OBJECTS` | 列表、用量及配额，语义与应用资源一致 |
| `cache clear PATH` | 将明确登记缓存标记Evicting，只失效其登记子对象；完成后根目录及ID保留，可再生成缓存 |
| `cache unregister PATH` | 持久标记Removing并撤销该缓存资源，完成后移除登记根 |
| `app help` / `cache help` | 查看实际参数和边界 |

以上是已登记资源的显式生命周期操作，不提供全盘清理、未知文件删除或宿主优化。出现无法核验的绑定/未知内容时保留它们，输出 `Cleanup deferred; registration retained for recovery.` 并返回状态1；下次启动按原登记继续，不用目录名猜测归属。配额不包含未知文件、协议暂存、元数据或仍由旧FD持有的脱离代际，因此不是物理磁盘限额。

独立`cache register`不自动属于应用；通过`app cache`/`app data`显式创建的关联位置随应用卸载。无关数据不联动；仍不把此接口当作设计原则P4的完整应用模型。没有依赖解析、独立应用执行身份和压力自动驱逐。专项命令验收使用新测试数据根，未运行实际 `.env` 数据根。核心/脚本/真实进程证据统一在[资源账本](filesystem.md#resources-2026-11-15)。

## 2. 初始化完成的含义

启动顺序为：读取配置 → Rust 初始化专用数据根 → 校验数据根 → 进入命令循环。不是先进入 Shell，再执行某条 `init` 命令；目前没有 `init` 命令；`source` 运行的是虚拟根内的脚本，不用于加载项目启动配置 `.env`。

默认读取启动工作目录中的 `.env`，也可以通过程序的唯一位置参数指定配置文件。当前仓库配置为 `ELARA_DATA_ROOT=D:/os`，不是 Linux 通用配置；部署到 Linux 时必须另行指定当地合法的绝对路径，不得照搬 Windows 盘符。

初始化输出可能为：

| 输出状态 | 实际含义 |
|---|---|
| `CreatedNewRoot (N directories)` | 新建专用数据根并创建目录布局 |
| `ClaimedEmptyRoot (N directories)` | 接管已有空目录并建立布局 |
| `CompletedOwnedRoot (N directories)` | 已标记的数据根补齐缺失目录，不覆盖已有文件 |
| `AlreadyReady` | 数据根和布局已满足校验，重复初始化没有新增布局目录 |

当前布局清单包含 24 个目录项；`N` 是此次实际新增的清单目录项数，不固定为 24。无合法标记的非空目录、错误标记、目录与文件冲突或非法路径会导致启动拒绝，不能自动换另一个数据根继续运行。

进入终端后：

- 交互模式提示符包含当前用户和虚拟当前目录，初始为 `root@elara:/> `，切换后例如 `alice@elara:/usr> `；重定向输入时不显示提示符。
- 启动时自动创建 `/etc/passwd`（仅 `root`）、`/etc/shadow` 和 `/var/log/elara.log`（如不存在），并记录一条 `startup` 日志。
- **root 首次启动没有密码时会提示设置**：输入并确认新密码即生效；按一次 Enter 跳过，不会额外消费下一条命令，下次启动再提醒。没有 root 密码时普通用户无法切回 root。
- 交互密码读取使用固定版本 `rpassword 7.3.1` 的原生 Rust 接口（Windows 独立 `CONIN$` 句柄，Unix `/dev/tty`），不调用 PowerShell、`stty` 或其他辅助进程。隐藏读取失败时返回错误，**拒绝退回可见输入**。重定向输入是显式的测试/自动化输入方式，不代表隐藏输入。
- 新密码输入不匹配、确认前 EOF 都不会修改密码。权限不足的 `useradd/passwd` 在读取密码前被拒绝，不会消费后续命令。
- 每次进入命令环境，虚拟当前目录从 `/` 开始；`cd` 不修改宿主进程的工作目录。
- `/` 表示 ElaraOS 的虚拟根，当前 Windows 实例对应配置的数据目录，而不是宿主磁盘根。
- `/root` 只是目录名，不表示当前身份是 Linux `root` 用户。
- `/proc`、`/sys`、`/dev` 现在默认由虚拟 provider 提供：只读身份/系统信息与 null/zero/full 字节设备；启动拒绝隐藏宿主 backing 内的未知内容，系统挂载不可卸载。`/run`、`/var/run` 仍是宿主能力目录，新运行态对象必须登记任务租约；不是完整进程文件系统或设备驱动模型。
- `/bin`、`/sbin`、`/lib` 是普通目录，不是 Ubuntu 的合并 `/usr` 软链接。
- `/snap` 不表示安装了 Snap；`/boot` 不表示已有启动镜像。
- 不执行宿主命令，不建立网络连接，不修改宿主服务、驱动、分区或启动配置。
- 路径、能力句柄与恢复规则由文件系统模块约束；这些不是完整 OS 级安全沙箱，不能限制恶意改写后的宿主程序。

## 3. 当前初始化后可以输入的命令

### 3.1 命令清单与真实语义

| 命令 | 示例 | 当前实际行为 | Ubuntu 风格差异 / 状态 |
|---|---|---|---|
| `help` | `help` | 列出常用命令 | 当前可用；独立提示符 UI 命令，不是 Bash 的全部内建帮助 |
| `mv` | `mv /tmp/a /tmp/b` | 真实文件/目录原子改名，无覆盖和copy/delete回退 | 部分可用；凭据分别报告源/目标目录持久性 |
| `cp` | `cp /tmp/a /tmp/b` | 复制一个文件到新名字，上限 1 MiB，不覆盖 | 部分可用；原子发布与跟进清理有显式报告 |
| `stat` | `stat /tmp/a` | 读取真实宿主后端元数据 | 部分可用；host_readonly不是虚拟账号权限 |
| `find` | `find /tmp` | 有界、逐路径授权的目录树列举 | 部分可用；无GNU选项，不公布失败的半棵树 |
| `du` | `du /tmp` | 逻辑字节和对象计数 | 部分可用；physical_usage明确UnavailableOnHostBackend |
| `fd` | `fd help`、`fd open /tmp/a create`、`fd read SLOT:GEN 10` | 真实文件对象/代际表的薄接入，交互与脚本共用表；su 关闭旧能力 | 部分可用；SLOT:GEN 只在本会话有效，read 不添换行；详见[句柄命令](file-descriptors.md) |
| `sh` | `sh -c 'echo hello; false || echo recovered'`、`sh /tmp/demo.sh`、`sh -n /tmp/demo.sh` | 运行脚本文本/文件或仅检查语法；采用隔离的脚本上下文 | 部分可用；详见[脚本专题](shell-scripting.md)，不执行宿主程序 |
| `source` | `source /tmp/context.sh`、`. /tmp/context.sh` | 保留变量、函数、目录；参数调用后恢复，`.` 为别名 | 部分可用；`return`结束source调用，`exit`结束当前调用环境 |
| `info` | `info` | 输出版本、hosted 模式、宿主平台、数据根、当前用户和能力边界 | 当前可用；项目专用命令，不是 Ubuntu 通用系统信息接口 |
| `pwd` | `pwd` | 输出会话的虚拟当前目录，初始为 `/` | 当前可用；不支持 `-L/-P` 等选项 |
| `cd` | `cd /usr`、`cd bin`、`cd ..`、`cd ~/docs`、`cd` | 切换已有虚拟目录；无参数使用 `$HOME`；失败保持原目录 | 部分可用；不支持 `cd -`、`~user` |
| `ls` | `ls`、`ls /usr`、在 `/usr` 下 `ls bin` | 列出一个虚拟目录；无参数列当前目录，相对路径从当前目录解释 | 部分可用；Shell有glob，但ls仅接受一个目录，无选项/文件详情，匹配多项仍报参数错误 |
| `cat` | `cat /etc/passwd`、`cat < /tmp/a`、`cat /tmp/large \| wc -c` | 文件/当前阶段stdin的原始字节，不补换行 | 部分可用；最终stdout和缓冲路径限65536字节，适用流式中间数据可超限；无GNU选项 |
| `mkdir` | `mkdir docs notes` | 在已有目录内逐个创建目录 | 部分可用；无 `-p`，父目录必须存在，已存在报错 |
| `touch` | `touch a.txt b.txt` | 逐个创建空文件 | 部分可用；已存在报错而不是更新时间戳 |
| `rm` | `rm a.txt b.txt` | 逐个删除普通文件 | 部分可用；对目录报 `use rmdir`；无 `-r/-f`；系统/根/保留路径拒绝 |
| `rmdir` | `rmdir empty-dir` | 逐个删除空目录 | 部分可用；非空目录报错；无递归；系统目录被拒绝 |
| `echo` | `echo hello`、`echo -n "$X"` | 已展开参数按空格连接，默认换行；`-n`不换行 | 部分可用；变量与重定向通过共同语言求值 |
| `sleep` | `sleep 2`、`sleep 5 bg`、`sleep 5 &` | 前台等待并记旧任务；bg参数为旧计时器；&为作用域脚本worker | 部分可用；整数0–3600秒；worker内sleep可合作式暂停/取消，不是宿主调度 |
| `ps` | `ps` | 列出legacy TaskTable的ID、状态、命令 | 不列宿主进程；状态同时投影到`/proc/jobs`为`legacy-task`，作用域脚本作业用jobs查看，两个ID命名空间分开 |
| `wait` | `wait 2`、`wait %2` | 数字ID等旧任务；%ID等脚本作业、收集输出、移除并join | 一次一个ID；作用域ID回收后不复用；worker不能等待自身或阻塞等旧任务 |
| `jobs` | `jobs` | 列出当前UID的作用域作业及实际状态 | 部分可用；完成未wait仍占槽，总记录最多16 |
| `fg` | `fg %2` | 请求恢复、等待并回收作用域作业 | 合作式，不切换宿主TTY进程组 |
| `bg` | `bg %2` | 请求恢复作用域作业，不等待 | 仅恢复已有ID，不创建新的后台作业 |
| `kill` | `kill -STOP %2`、`kill -CONT %2`、`kill -TERM %2` | STOP等实际ACK；CONT恢复；TERM/INT请求取消 | 部分可用；缺省TERM，取消需实际观察；不接受宿主PID/真实信号 |
| `whoami` | `whoami` | 显示当前 ElaraOS 用户名 | 当前可用；与宿主登录账户无关 |
| `users` | `users` | 列出 `/etc/passwd` 中的账户、uid、home | 部分可用；格式为项目自定义 |
| `useradd` | `useradd alice`、`useradd alice pass1234` | 创建账户、设置密码并建立 `/home/alice`；仅 root。省略密码时隐藏输入两次 | 部分可用；密码至少 4 字符，不含空格和 `:` |
| `passwd` | `passwd`、`passwd alice`、`passwd alice newpass` | 设置密码；无参数改自己；root 可改任何人，普通用户只能改自己。省略密码时隐藏输入两次 | 部分可用；两次输入不一致则不改 |
| `su` | `su`、`su alice`、`su root pass` | 切换当前用户；无参数切 root；root 免密，其他人需密码（省略时隐藏输入）；切到自己直接提示 | 部分可用；无 `-` 登录 shell 语义 |
| `log` | `log`、`log 50` | 显示 `/var/log/elara.log` 最后 N 行（默认 20，范围 1–1000）；仅 root | 当前可用；项目专用命令 |
| `clear` | `clear` | 交互模式发送 ANSI 清屏序列；重定向模式输出 `[screen cleared]` | 当前可用；依赖终端支持 ANSI |
| `exit` | `exit`、`exit 7` | 输出 `Bye.`，结束本程序并返回指定状态；缺省使用 `$?` | 当前可用；0–255，不关机、不关闭宿主终端 |

`help/info/clear` 不接受参数，认证及UI控制命令仅支持独立提示符调用。普通命令共享状态码，`echo $?`可以观察；最终程序状态不能证明之前所有命令成功。命令名区分大小写。

命令行使用共同脚本语法：引号/转义保留字面属性，变量展开不重新解释为语法。`< > >> | ; && || &`可用；未支持的语法在整行解析时拒绝，前面的命令不先执行。多路径命令按顺序执行，不是事务；运行时后续失败不回滚此前操作。

`ls` 的当前规则：

- 目录项排序后逐行输出；目录后缀为 `/`，普通文件没有后缀。
- 名为 `.elara-root` 的标记项被隐藏；其他点开头的条目没有统一隐藏规则，不能等同于 Ubuntu 默认 `ls`。
- 宿主原生符号链接或 Windows reparse point 条目显示 ` [link blocked]`，不能沿链接访问。
- 访问不存在的目录、普通文件或非法路径时输出 `Error: ...`。
- Shell 在虚拟命名空间内归一化 `.`、`..` 和重复 `/`；例如 `ls /usr/../etc` 访问 `/etc`。任何走到根以上的路径仍被拒绝。
- 宿主文件访问后端仍只接受不含 `.`、`..` 的已归一化路径；拒绝反斜杠、盘符、冒号、控制字符、标记路径和保留设备名。
- `ls -l` 会把 `-l` 当作路径，而不是详细列表选项；`ls /etc /usr` 会因参数过多提示用法。
- 含空格路径用引号传入，例如 `ls "my docs"`。

### 3.2 当前可用的逐行试用清单

**只在进入 ElaraOS 后逐行输入**，不是宿主启动命令。`exit` 必须放最后：

```text
help
whoami
useradd alice
su alice
su alice
cd /root
log
cd /home/alice
touch mine.txt
passwd
su root
whoami
exit
```

说明：首次启动会先要求设置 root 密码（输入两次，或 Enter 跳过）。`useradd alice` 后会隐藏输入两次 alice 的密码；`su alice` 第二次输入得到 `Already alice; nothing to do.`；alice 执行 `cd /root`、`log` 被拒绝；`passwd` 无参数修改 alice 自己的密码；`su root` 要求输入 root 密码，正确后 `whoami` 显示 `root`。

### 3.3 与 Bash 仍有差异的语法

| 输入 | 当前行为 | 边界 |
|---|---|---|
| `cd` 无参数、`echo $HOME` | 使用真实账户home初值的变量，支持~和~/path | 改USER不改变安全主体；`~user`、`cd -`尚未支持 |
| `echo -n hello`、`echo hello > /tmp/a.txt` | 不换行或实际重定向 | byte-exact；不是GNU全部选项 |
| `ls \| cat`、`cat /tmp/large \| wc -c`、`pwd && ls`、`a; b` | 共同AST求值和状态传播 | cat/wc等适用内建真实并发流式；ls/模块/复杂展开/输出重定向等显式64KiB回退，不冒充内核IPC |
| `sh -c 'sh -c "echo nested"'` | 嵌套解释，复用模块和总调用预算 | 不运行宿主sh |
| `rm *.txt` | 在授权虚拟目录展开并逐个删除匹配的普通文件；未匹配保留字面量 | 有界glob，不递归，不跟链接；后续运行失败不回滚前面操作 |
| `$(...)`、`$((...))`、`${X:-word}` | 已支持常用展开子集 | 算术无自增/赋值；参数替换仅字面模式；详见专题 |
| `case/local/unset/set -e/-u/trap EXIT` | 已支持明确子集 | local仅函数，EXIT非宿主信号，未支持Bash全部扩展 |
| `sleep 5 &` | 实际作用域worker；jobs/fg/bg/wait %ID/kill合作式控制 | 后台stdin为EOF，输出由wait/fg收集；Shell退出/EOF/身份变更取消并join，非宿主进程 |
| `rm -r dir`、`mkdir -p a/b` | 不按GNU选项执行 | 不提供递归删除/递归创建 |
| `su -` | `-`被当作用户名 | 无登录shell语义 |
| `exit 7` | 退出程序，实际进程码7 | `sh`隔离exit，source的exit结束调用环境 |

普通命令状态可通过 `$?`、`&&/||` 观察；不能用进程最终状态0证明所有中间命令成功。

## 4. Ubuntu 风格目标命令 → 模块对照

**本节是扩展目标，不是统一的未实现列表。** 第3节/脚本专题已注明的基础子集（stat/cp/mv、常用语法、流式及缓冲回退、合作式作业）已接通；未注明的GNU选项、内核级IPC/进程组和程序装载仍待实现。示例要求文件实际存在；不直接移植GNU/systemd/apt，不承诺全部选项。

“§” 对应[模块总览](operating-system-modules.md)的章节号；各行给出需要联动的底层模块，不是规定每条命令必须独占一个模块。

### 4.1 路径、目录、文件与文本（优先形成使用闭环）

| 命令 / 目标示例 | 应实现的可见能力 | 对应模块 | 前置条件与验收重点 |
|---|---|---|---|
| `cd /usr`、`cd ..`、`pwd` | 当前基础子集已可用；后续可补目录历史等扩展 | §13 文件系统；§19 Shell | 虚拟路径归一化；切换失败保持原目录；根上越界拒绝 |
| `ls -l /etc`、`ls -a /` | 目录元数据、隐藏项策略和选项 | §13 文件系统；§19 Shell | 元数据接口和参数解析；不能把宿主权限直接当 ElaraOS 权限 |
| `stat /etc/os-release` | 文件类型、大小、时间等信息 | §13 文件系统；§15 访问控制 | 定义支持的字段及时间来源，未实现字段不能伪造 |
| `cat /etc/os-release` | 读取普通文件内容 | §12 I/O；§13 文件系统；§19 工具 | 受限打开和读取；目录、无权限、无效 UTF-8、读取中断分别处理 |
| `wc -c /tmp/a.txt`、`wc -l /tmp/a.txt` | 字节数或行数 | §12 I/O；§13 文件系统 | 流式读取；明确换行与字节语义；不只数 Unicode 字符 |
| `head -n 10 /var/log/elara.log`、`tail -n 10 /var/log/elara.log` | 文件前后若干行 | §12 I/O；§13 文件系统 | 本行仅列目标，不在本轮工具命令中用作下游过滤器；边界行数和大文件内存有测试 |
| `grep error /var/log/elara.log` | 文件内文本查找 | §13 文件系统；§19 工具 | 先选定字面查找或正则子集；定义无匹配与读取失败的区别 |
| `find /home -name '*.txt'` | 按条件遍历虚拟目录 | §13 文件系统；§19 参数解析 | 限定深度、链接政策与错误传播；不越界访问宿主 |
| `mkdir /home/demo`、`mkdir -p /home/demo/docs` | 创建目录 | §13 文件系统；§15 访问控制 | 受限写路径、父目录与同名文件冲突；已有目录幂等性 |
| `touch /tmp/a.txt` | 创建空文件；后续可补更新时间 | §13 文件系统；§5 时间；§15 权限 | 已有文件不截断；区分创建与时间更新，保护标记 |
| `cp /tmp/a.txt /home/demo/a.txt` | 单文件基础子集已可用；大文件流式等扩展待实现 | §12 I/O；§13 文件系统 | 当前不覆盖，复制上限1MiB，显式提交/持久性/跟进报告 |
| `mv /tmp/a.txt /tmp/b.txt` | 原子改名基础子集已可用；跨后端迁移待实现 | §13 文件系统；§12 持久化 | 不覆盖，无copy/delete回退；按凭据区分可见提交与目录持久性 |
| `rmdir /home/demo/empty` | 删除空目录 | §13 文件系统；§15 权限 | 非空拒绝；根、标记及受保护布局目录政策先明确 |
| `rm /tmp/a.txt` | 删除普通文件 | §13 文件系统；§15 权限 | 删除与递归策略分别设计；实际清理操作仍先交清单等授权 |
| `ln -s /home/demo /tmp/demo` | 符号链接 | §13 路径与命名空间；§15 隔离 | 原生链接仍禁止；受管虚拟链接使用vfs symlink，不通过取消宿主守卫实现 |

### 4.2 Shell、终端与程序运行

| 命令 / 目标示例 | 应实现的可见能力 | 对应模块 | 前置条件与验收重点 |
|---|---|---|---|
| `help cd`、`COMMAND --help` | 命令级帮助 | §19 Shell 与基础工具 | 参数解析与统一帮助契约；本文的 `COMMAND` 是占位符 |
| `echo "hello world"`、`echo -n hello` | 引号、选项及输出规则 | §19 Shell | 先定义兼容子集；不能意外调用宿主 Shell |
| `echo hello > /tmp/a.txt`、`cat /tmp/a.txt` | 重定向写入后读取 | §19 Shell；§13 文件；§9 句柄 | 参数解析、截断/追加、受限打开和短写；往返内容一致 |
| `cat /tmp/a.txt \| wc -c` | 管道传递字节流 | §19 Shell；§10 IPC；§7 调度 | 先有管道和执行模型；背压、EOF、读端退出不能死锁 |
| `env`、`export NAME=value`、`echo $NAME` | 会话环境、变量展开与继承 | §19 Shell；§9 运行环境 | 与启动 `.env` 配置解析分开；不能读取或改写宿主秘密与全局变量 |
| `history` | 查看当前会话历史 | §19 Shell；可选 §13 文件 | 会话内存历史先做；持久化与敏感内容政策另定 |
| `clear` | 真正的终端清屏 | §19 TTY / 终端 | 终端能力探测；非终端输出不插入不适当控制序列 |
| `/bin/demo`、`demo arg` | 装载并执行 ElaraOS 程序 | §9 ABI / 装载；§7 进程；§8 内存；§13 文件；§15 隔离 | 定义可执行格式和搜索路径；不是转给宿主 `Command` 运行 |
| `sleep 1` | 等待指定时长 | §5 时间；§7 阻塞与唤醒；§19 工具 | 单调时钟、参数范围、取消与唤醒；不能忙等冒充调度 |
| `jobs`、`fg`、`bg`、`wait` | 作用域合作式子集已接；内核作业控制待实现 | §19 Shell / TTY；§7 进程；§10 事件 | 当前%ID、ACK、取消与join；真实进程组、TTY所有权和宿主信号未实现 |

### 4.3 身份、权限、进程与资源

| 命令 / 目标示例 | 应实现的可见能力 | 对应模块 | 前置条件与验收重点 |
|---|---|---|---|
| `whoami`、`id` | ElaraOS 当前主体与组 | §15 身份；§18 账号服务 | 先有身份模型；目录 `/root` 或宿主用户名不能充当结果 |
| `uname -a` | ElaraOS 自身系统信息 | §4 架构；§18 管理；§21 版本 | 不把 Windows/Linux 宿主信息冒充自研内核信息 |
| `hostname` | ElaraOS 系统名称 | §18 配置；§14 网络 | 自有配置存储；不能修改宿主机器名 |
| `chmod 600 /home/demo/a.txt` | 访问模式管理 | §15 授权；§13 文件元数据 | 身份与访问检查必须真正生效；改元数据但访问不受限不能验收 |
| `chown demo /home/demo/a.txt` | 文件归属管理 | §15 身份授权；§13 元数据 | 先有用户查询与特权规则，非授权主体必须拒绝 |
| `sudo COMMAND`、`su demo` | 受控身份/权限转换 | §15 认证授权；§18 账号；§7 进程 | 后期设计；不是给宿主提权，也不能绕过访问控制 |
| `ps`、`ps -ef` | 列出 ElaraOS 的任务/进程 | §7 进程；§21 可观测性；§13 可选 procfs | 对象注册与一致快照；不能默认列宿主进程来冒充 |
| `top` | 持续查看任务与资源 | §7 调度；§5 记账；§21 统计；§19 终端 | 先有准确采样及时间基准，刷新期间退出可用 |
| `kill PID` | 请求目标进程终止 | §7 生命周期；§10 信号；§15 授权 | `PID` 为目标语法占位符；仅作用于 ElaraOS 任务，不终止宿主进程 |
| `free -h` | 查看 ElaraOS 内存用量 | §8 内存；§16 记账；§21 统计 | 明确总量、已用与缓存；宿主原型指标必须显式标注来源 |
| `uptime` | 启动以来的时长与可选负载 | §5 单调时间；§7 调度；§21 统计 | 原型运行时长与内核启动时长分开；没有负载模型就不编数字 |
| `date`、`date -u` | 查看墙上时间 | §5 时间；§18 时间配置 | 明确时区、格式和时钟来源；只读先做，不改宿主时钟 |

### 4.4 存储、设备、网络、服务与维护

| 命令 / 目标示例 | 应实现的可见能力 | 对应模块 | 前置条件与验收重点 |
|---|---|---|---|
| `df -h /` | 文件系统容量与可用空间 | §13 文件系统；§12 存储；§16 配额 | ElaraOS 配额与宿主卷剩余量不能混为一谈 |
| `du -sh /home` | 目录占用统计 | §13 文件系统；§12 存储 | 逻辑大小、实际占用与硬链接去重策略分别定义 |
| `lsblk` | ElaraOS 块设备与分区列表 | §11 设备；§12 块存储 | 先有设备注册；不能把普通 `/dev` 目录当作设备模型 |
| `mount`、`umount /mnt/data` | 查看挂载和卸载 | §13 VFS / 挂载；§12 存储；§15 权限 | 先有挂载表和打开句柄；忙碌挂载、失败回退分别验收 |
| `sync` | 按契约同步待写数据 | §12 持久化；§13 一致性 | 流程必须到实际后端；只刷新语言缓冲不等于断电持久化 |
| `ip addr`、`ip link`、`ip route` | 接口、地址和路由 | §14 网络；§11 网卡；§18 网络配置 | 先有 ElaraOS 网络状态；读查询与写配置分开授权 |
| `ping -c 3 127.0.0.1` | 有界 ICMP 诊断 | §14 IP / ICMP；§5 超时；§15 权限 | 先有回环和协议栈；不得伪造成功、无限发送或调用宿主工具代替 |
| `ss -lnt` | 查看 ElaraOS 监听连接 | §14 Socket；§21 统计；§15 授权 | 先有 Socket 注册与连接状态；限制可见范围 |
| `getent hosts localhost` | 通过名称服务解析地址 | §18 名称服务；§14 DNS | 本地 hosts 与 DNS 来源分开；解析失败不能静默换公共服务 |
| `curl http://127.0.0.1:8080/` | HTTP 客户端请求 | §14 TCP / Socket 与用户态 HTTP；§9 运行库 | 后期工具；HTTPS 另需 TLS 与证书，不等于内核网络完成 |
| `dmesg` | 查看 ElaraOS 内核/原型日志 | §21 日志；§5 时间；§15 授权 | 原型日志须标注，不读取宿主内核日志冒充 |
| `journalctl` | 查看结构化服务日志 | §18 日志服务；§21 诊断；§13 存储 | 参考 systemd 用法，不表示已有 journald；先定格式、轮转与查询范围 |
| `systemctl status SERVICE`、`systemctl start SERVICE`、`systemctl stop SERVICE` | 服务查询与生命周期控制 | §18 服务管理；§7 进程；§10 IPC；§15 权限 | `SERVICE` 为占位符；先有真实状态机，失败与重启循环必须可观察 |
| `apt list --installed`、`apt install PACKAGE` | 软件包查询与安装 | §20 软件管理；§13 文件；§15 校验；§18 配置 | `PACKAGE` 为占位符；Debian 包格式兼容未决定，先设计包数据库、验证、事务和回滚 |
| `reboot`、`shutdown -h now` | ElaraOS 系统重启/关机 | §17 生命周期；§18 服务；§13 同步；§11 设备 | 独立内核阶段能力；当前只能 `exit`，绝不对宿主执行关机 |

不把分区、格式化、文件系统修复、内核模块装卸列为早期可用命令；这些要先有对应设备与存储实现、权限设计和独立安全验收。本文不建立定时任务、cron 或本机一次性自动化。

## 5. 给其他会话的选模块流程

1. **先读 README**，确认处于宿主原型还是独立内核阶段，以及最新启动命令是否真正通过。
2. **读本文件第 3 节与第 8 节**，不要凭文件名或历史 exe 推定能力。
3. **从第 4 节选择一个小的命令增量**，再读模块总览对应章节和已存在的相关源码入口。涉及写入、任务或资源的增量必须先读[设计原则](design-principles.md)，按其 P1–P7 设计写入边界与生命周期。
4. **先写契约**：支持语法、状态变化、路径与权限边界、返回结果、错误和降级行为；未支持选项明确拒绝。
5. **按依赖实现**，命令分发只负责解析与调用；文件系统规则不要复制在每条命令里，不能靠调用宿主工具假装实现。
6. **从真实入口验收**，覆盖成功、失败、不越界和重启后的状态；一个命令在 Shell 中能跑才是该增量完成。
7. **同步单一账本和入口**：更新本文件、`help`、README 和相应修订记录；模块总览只有职责变化时才修改，不把命令验收日志复制过去。

### 5.1 推荐的早期增量顺序（建议，非已批准里程碑）

| 增量 | 可运行闭环 | 先读模块 | 不夹带的能力 |
|---|---|---|---|
| A（基础子集已验收） | `cd /usr` → `pwd` → `ls bin` → `cd /` | §13、§19 | 不加入文件写入，不放开宿主路径或链接 |
| B | `stat`、`cat`、`wc` 读取已有受控文件 | §12、§13、§19 | 不实现包管理和程序执行；测试文件须明确来源 |
| C | `mkdir`、`touch` → `ls`、`cat` 检查结果 | §13、§15、§5 | 不直接加递归删除或覆盖复制 |
| D | 写入 → 读取 → 重启 → 再读取 | §12、§13、§19 | 先单文件写入和明确持久化，再做重定向语法 |
| E | `cp`、`mv` 与精确授权的删除 | §12、§13、§15 | 先定覆盖、原子性与降级代价，不用 `Option` 混掉分支 |
| F | 程序启动 → `ps` → 等待/退出 | §7、§8、§9、§10、§15 | 不把宿主进程启动包装成自研 OS 调度 |
| G | 服务、网络、安装更新 | §11、§14、§18、§20 | 必须以前述对象、权限、IPC 和一致性为前置条件 |

CPU、中断、物理内存等模块不一定有直接对应的日常命令。需要实现独立内核时，仍按模块总览的底层依赖推进，不能只补 Shell 命令绕过它们。

**当前 [virtual_path.rs](../src/virtual_path.rs) 已通过 `main.rs` 接入，`shell.rs` 的 `cd/ls` 使用它解析路径，文件访问后端使用严格路径入口。** 本轮重新读取和实际运行后，已将初次扫描中的“尚未接入、没有 `cd`”就地改判为“基础导航已可用”；期间源码发生变化，版本号仍为 v0.1.3，所以第 8 节另附源码摘要。其他会话建议从增量 B 的一个只读命令开始，先确认对应文件访问接口是否已被其他会话继续扩展。

## 6. 每个新增命令的验收契约

- **正常路径**：从初始化后的真实命令入口输入，观察输出和状态，不只调用模块函数。
- **负例**：未知选项、缺参数、多参数、不存在对象、对象类型错误和读取/写入失败都不能伪装成功。
- **边界**：盘符、UNC、反斜杠、ADS、设备名、标记路径、符号链接和根上越界分别测试；增加 `..` 时仅在虚拟命名空间内归一化，不将原始路径直接交给宿主。
- **必要性对照**：验证守卫“没有就不成立”；例如移除专用标记必须启动失败，恢复才成功。红了先判断实现错还是判据错，不放宽阈值凑通过。
- **状态与恢复**：失败的 `cd` 保持原目录；失败写入不毁掉旧数据；重复初始化不覆盖数据；持久化能力必须重启再读。
- **数据往返**：文件复制、导出导入或包数据库迁移须读取原始值并比较；只产出可读报告不等于往返验证。
- **降级成本**：跨后端移动、不可原子更新等分支单独验收，以返回枚举区分路径和代价。
- **统计来源**：原型对象、宿主借用指标和自研内核指标分开标注；没有真实能力就报不支持。
- **完成证据**：记录源码版本、平台、命令字面值、原始输出和失败用例；历史通过不能替代当前版本验收。

## 7. Ubuntu / 官方语义核验入口

以下是后续细化兼容子集的官方参考入口，**本轮未逐条联网核验其最新版本，也没有在 Ubuntu 上执行第 4 节的目标命令**。实现前按选定命令查看对应规范，不能仅凭本表承诺完整兼容。

- [Ubuntu Manpages](https://manpages.ubuntu.com/)：按发行版和工具查看命令手册；工具可用性受安装包与系统组件影响。
- [GNU Coreutils](https://www.gnu.org/software/coreutils/manual/coreutils.html)：`ls/pwd/cat/cp/mv/rm/mkdir/stat/wc/date` 等 GNU 工具语义。
- [GNU Bash](https://www.gnu.org/software/bash/manual/bash.html)：引号、变量、重定向、管道、内建命令与作业控制。
- [POSIX Issue 8](https://pubs.opengroup.org/onlinepubs/9799919799/)：可移植接口与 Shell/工具契约；GNU 扩展不自动属于 POSIX。
- [systemd 官方手册](https://www.freedesktop.org/software/systemd/man/latest/)：`systemctl/journalctl` 等具体用户态实现，不是所有 OS 的必选设计。
- [Linux 内核文档](https://docs.kernel.org/)：具体内核子系统、设备、网络与文件系统机制；不能替代 ElaraOS 自己的实现证据。

## 8. 本轮验收记录

### 8.1 原生 Rust 密码输入与计数修正验收（2026-11-07，当前有效）

| 项目 | 结果 | 证据边界 |
|---|---|---|
| 完整单元测试 | 73 passed、0 failed | 包括 EOF/空行区分、一次 Enter 跳过、拒绝权限操作不消费输入、shadow 保护、Windows 大小写别名 |
| Windows 原生终端集成 | 1 passed、0 failed | `tests/terminal_pty.rs` 通过 ConPTY 启动真实程序；root 密码两次不回显，正常命令在 Enter 前可见（证明模式恢复），退出码 0，shadow/log 无测试密码明文 |
| 依赖与运行边界 | 运行依赖 `rpassword = 7.3.1`；测试依赖 `portable-pty = 0.8.1` | 均为 Rust 库；运行时代码不启动 PowerShell、`stty` 或其他辅助进程。隐藏失败拒绝明文回退 |
| 命令计数 | 22 个 | 程序 help、命令表和 README 同步；此前“23 个”的计数错误已修正，实际未增删命令 |
| 未验证范围 | Ubuntu/Linux 原生、快速粘贴竞态、全部 Windows 终端宿主 | ConPTY 测试不等于所有终端兼容性都已确认 |

原生测试最初匹配提示符尾部字面空格失败；ConPTY 将该空格编码成光标移动序列。读取输出后修正提示符识别，不改密码不回显、回显恢复等安全断言。

### 8.2 root 引导、交互式密码、su 自检修正验收（历史批次）

| 项目 | 结果 | 证据边界 |
|---|---|---|
| `cargo test --offline` | 68 passed、0 failed | 新增 `terminal.rs`（2）、启动引导（2）、交互式提示行消费（1） |
| 真实进程端到端 | 全新虚拟根，21 行输入，退出码 0 | 启动提示设置 root 密码并成功；`useradd alice` 隐藏输入两次；`su alice` 第二次报 `Already alice`；`passwd` 无参数改自己；`su root` 错误密码被拒、正确密码成功并进入 `/root` |
| 密码不落明文 | `/etc/shadow`、`/var/log/elara.log` 均不含明文密码 | 实测文件内容 |
| 隐藏回显 | 非交互路径已验证；**真实终端隐藏效果未在本会话验证**（无控制台句柄） | 需用户在 Windows Terminal / PowerShell 中实际确认 |
| Ubuntu / Linux 原生运行 | 未实测 | `stty` 路径未执行 |

### 8.3 权限、rmdir、clear 修正验收（2026-11-07）

| 项目 | 结果 | 证据边界 |
|---|---|---|
| `cargo test --offline` | 63 passed、0 failed | 新增 `permissions.rs`（3）、`sha256.rs`（1，含 FIPS 测试向量）、用户密码（1）、rmdir（1）、Shell 权限/rmdir/clear/日志脱敏（4） |
| 真实进程端到端 | 全新虚拟根，20 行输入，退出码 0 | `rm 1.txt` 对目录报 `use rmdir`，`rmdir` 删除成功；alice 被挡在 `/root`、`/var/log`、`/etc` 写之外；`su root` 无密码被拒；`passwd` 后 `su alice newpass1` 成功 |
| 密码不落明文 | `/etc/shadow` 与 `/var/log/elara.log` 均不含明文密码 | 实测文件内容 |
| 真实交互 TTY、Ubuntu / Linux 原生运行 | 未实测 | Windows 非交互结果不外推；`clear` 的 ANSI 序列在 Windows Terminal/PowerShell 7 可用，旧 conhost 未验证 |

### 8.4 五模块验收（2026-11-07）

| 项目 | 结果 | 证据边界 |
|---|---|---|
| `cargo test --offline` | 54 passed、0 failed | 覆盖配置、虚拟路径、文件读写、解析器、任务、用户、日志及 Shell 集成 |
| `cargo build --offline` | 通过 | 产物在项目 `target/debug/elara-host.exe` |
| 真实进程端到端 | 以全新空目录作为虚拟根，25 行输入，退出码 0 | `CreatedNewRoot (24 directories)`；含空格文件名创建/读取/删除；`echo hello > out.txt` 报错且未建文件；`sleep 1 bg` → `ps` 显示 running → `wait 2` 显示 `exited 0`；`useradd alice` 成功、`alice` 执行 `useradd bob` 被拒；`rm /` 被拒；`/etc/passwd` 两行；`/var/log/elara.log` 25 行 |
| 宿主隔离 | `ps` 不含宿主进程；`home/bob` 不存在；虚拟根外无新文件；`usr` 完好 | 实测检查虚拟根实际状态 |
| 真实交互 TTY、Ubuntu / Linux 原生运行 | 未实测 | Windows 非交互结果不外推 |

### 8.5 历史快照验收（八命令阶段，仅作历史）

| 项目 | 结果 | 证据边界 |
|---|---|---|
| 已读源码与命令分发 | 当时接通八个命令 | `virtual_path.rs` 已接入；初次扫描的七命令结论因源码更新而废止 |
| 当时 v0.1.3 编译 | `rustc exit code: 0` | 不经过 Cargo；编译前后、运行后源码 SHA-256 相同 |
| 初始化后八命令运行与负例 | 4 个会话、54 行命令输入通过 | 覆盖导航、失败保持目录、会话重置、中文、40 空行清屏、文本不展开及重定向不写文件 |
| Cargo 构建 | 当时失败，exit code 101；2026-11-07 后续已恢复 | 旧 `.cargo/config.toml` 在 `[env]` 中设置 `CARGO_HOME` 被 Cargo 拒绝；该配置后来已备份并删除，Cargo 回到项目 `target` 构建 |
| 单元测试全套、真实交互 TTY | 快照验收时未执行；后续 `cargo test --offline` 25 passed | 实际进程验收使用非交互输入；真实 TTY 仍未执行 |
| Ubuntu / Linux 原生运行 | 未实测 | Windows 结果不外推为 Ubuntu 平台验收 |
| 第 4 节其余目标能力 | 未实现、未执行 | `cd/pwd/ls` 的第 3 节子集除外；其余仅作模块选择依据 |
| 文档链接与状态自洽 | 通过：3 份文档、27 个本地链接、17 个引用模块章节 | 八个命令行与 19 行试用清单一致；4 个源码摘要及验收程序路径核对通过 |

该快照验收程序曾放在旧的 `D:/os/var/build/` 下（文件名 `command-check-v013-1793982386656756800.exe`）。构建产物指向虚拟根的做法已撤回，不再把它作为运行入口；当前运行方式见第 1.0 节的 `cargo run`。源码后续改变时要重新构建并验收，不能仅因版本号仍为 v0.1.3 就沿用结果。

关键原始输出节选：

```text
rustc exit code: 0
Initialization: AlreadyReady
ElaraOS host prototype v0.1.3
cd     - change virtual directory (default /)
virtual filesystem: working directory and read-only listing
Error: Cannot move above the virtual root.
Error: Host path syntax and control characters are refused.
Unknown command: cat. Type 'help'.
PASS: 4 sessions; 54 input lines; 8 commands; cd/pwd/ls state; Unicode; 40-line clear; failures; normalization; no redirection write; stable source snapshot.
```

同版本号可能对应不同源码，以下 SHA-256 用于辨认当时的导航能力快照；配置迁入 `src/config.rs` 后，`filesystem.rs`、`virtual_path.rs` 等文件已变化，这些值仅标识历史快照，不代表当前文件：

| 文件 | SHA-256 |
|---|---|
| `src/main.rs` | `3fd35fb9af045f0e68b017fd53f6737788e54b0ffc2ff3864bdc55fc70ddf90d` |
| `src/shell.rs` | `be55a4a066b4c9aa5dd3b9b7babf5fe6042692a7f91725c4d365d94d3ce7b50c` |
| `src/filesystem.rs` | `2a53617b6bc94ea0d1e6503a0cbf40ea62eca205289198e9230e7be09ba2a0f5` |
| `src/virtual_path.rs` | `16e7d7b8d0cb41d133a755be6c9a545dc2826ec16a72a91a395a1d5bae3edd26` |

本轮遇到的失败也保留：先因 Cargo 配置失败；随后旧判据断言 `cd` 应为未知命令，但实际已成功切换到 `/usr`，因此该验收脚本以 `AssertionError` 退出。重新读取源码确认导航模块已接入后，更新判据与文档再验收；没有修改引擎或放宽路径边界让测试通过。

## 9. 修订记录

| 日期 | 修订内容 |
|---|---|
| 2026-11-09 | fd 接账号/对象/归属门；受管 cat/wc/脚本/输入流与只读诊断不绕对象权限；cp 新目标归调用者、mv 保持受管身份，目录受管后代整体移动仍拒绝。新增 owned 说明并同步 help；质量门与最终计数只引用文件系统单一账本。 |
| 2026-11-08 | M5流式和作用域作业接通，常用清单34项；修订旧不支持&及全部顺序缓冲口径，注明%ID/旧数字ID、cooperation/队列限额、typed fallback、退出/身份join。稳定全量169+145+6=320通过、三个必要性对照成立；严格Clippy仍有FS三处lint，未冒称全绿。 |
| 2026-11-08 | 常用语法限定子集接入提示符/脚本并验收；新增展开、glob/tilde、case/作用域/IFS/退出选项/EXIT，堵住提示符和累计参数、保存绑定预算通道。最终152+118+5=275和严格Clippy通过，真实程序已运行核心语法；旧236等保留历史。真正流式及一般作业继续实施。 |
| 2026-11-08 | 文件命令桥、共同前端、exit状态和嵌套sh/source完成验收；三十常用命令清单与额外内建分开计。cat不补换行，多路径顺序执行；修订旧拒绝变量/操作符规则并保留安全守卫。全量152+79+5=236及严格Clippy通过；source exit结束调用环境。常用语法、真正流式管道与一般作业继续实施。 |
| 2026-11-08 | 新增fd子命令族，顶层计数25；记录token局部作用域、按实际账号打开、su关闭旧能力、流式写入进度和模块输出限额。FD14项与真实CLI通过，最新190（133+54+3）全过，严格Clippy通过；旧批次不叠加。 |
| 2026-11-07 | 文件系统收尾后复核最新全量87模块+39脚本+1真实脚本进程+1ConPTY=128通过，FS24项包含于87；链接文件系统专题，保留126及旧计数为历史。所有者登记、全量临时目录清扫、日志轮转仍是待落实设计，不扩大平台与断电证据范围。 |
| 2026-11-07 | 新增sh/source脚本入口并标注`.`别名，保持交互顶层与脚本语法范围分离；支持变量/控制流/函数/重定向/有界管道，直接调用实际文件系统与共享任务/用户/日志。新增测试在tests，39脚本功能测试与全量126通过；以脚本专题为子集语义和覆盖权威。 |
| 2026-11-07 | 首次建立初始化后命令账本：记录已接入命令及其 Ubuntu 差异，按 Ubuntu 常见用法列待实现命令与模块依赖，补充跨会话选模块顺序、失败边界、验收契约和官方参考入口。 |
| 2026-11-07 | 初次扫描后源码在核对期间新增并接入 `cd` 与虚拟当前目录；撤销“七命令、`pwd` 固定根、`ls` 相对路径从根解释、路径模块未接入”的过时结论，就地更新为八命令与当前目录语义。当前源码经 rustc 编译及 4 会话 / 54 行输入验收，保留 Cargo 失败、旧判据失败、未测试范围及源码摘要。 |
| 2026-11-07 | 构建边界纠正后同步：运行方式改为 `cargo run`，目录布局来源改为 `src/config.rs`，撤回指向虚拟根的旧验收程序入口；记录 `cargo test` 25 passed、`cargo build` 通过，源码摘要改标为历史快照。 |
| 2026-11-07 | 五模块落地：文件读写（`cat/mkdir/touch/rm`）、命令解析器（引号/转义/元字符拒绝）、任务（`sleep/ps/wait`）、用户（`/etc/passwd`、`whoami/users/useradd/su`）、日志（`/var/log/elara.log`、`log`）。命令表更新为二十个命令，试用清单与不支持语法表同步；54 个测试与全新虚拟根真实进程验收通过。 |
| 2026-11-07 | 用户实测反馈修正：新增 `permissions.rs` 权限模型（普通用户不可读 `/root`、`/var/log`，只可写 home 与 `/tmp`）、`/etc/shadow` 加盐 SHA-256 密码、`passwd` 命令、带密码的 `su`；新增 `rmdir`，`rm` 对目录/缺失路径给出精确错误；`clear` 改为 ANSI 清屏序列；日志不记录密码。命令数二十二；63 个测试与真实进程验收通过。 |
| 2026-11-07 | 第二轮反馈修正：root 首次启动无密码时引导设置（可跳过）；`su` 到当前用户直接提示；新增 `terminal.rs` 隐藏回显读取，`useradd/passwd/su` 省略密码时交互输入两次确认；`passwd` 无参数改自己。68 个测试与真实进程验收通过；真实终端隐藏效果待用户确认。 |
| 2026-11-07 | 当前收尾：修正 22 个命令计数；移除 PowerShell/stty 辅助执行，改用原生 Rust 密码读取；隐藏不可用时失败关闭，跳过/EOF/确认不匹配以枚举区分。新增权限预检、shadow 读取保护与 Windows 大小写别名测试。73 单元测试和 1 Windows ConPTY 集成测试通过；旧批次证据继续按历史保留。 |

目录setgid继承、默认ACL独立性及历史sticky审批阻塞见[目录SGID增量](filesystem.md#directory-setgid-2026-11-09)；当前命名语义与验收见[sticky增量](filesystem.md#sticky-2026-11-09)；`chmod 2770 DIR`只改变虚拟元数据，不执行宿主chmod或提权。

<a id="internal-vfs-commands"></a>
## 内部文件系统命令（2026-11-14）

下列输入到 ElaraOS，不是 PowerShell；只演示新建的内存卷和根内普通文件。

```text
vfs mount /mnt/work 65536 64
mkdir /mnt/work/dir
printf payload > /mnt/work/dir/file
vfs link /mnt/work/dir/file /mnt/work/alias
vfs symlink dir/file /mnt/work/link
cat /mnt/work/link
vfs snapshot /mnt/work /mnt/snapshot
vfs clone /mnt/snapshot /mnt/clone
vfs save /mnt/work /root/volume.img
vfs unmount /mnt/clone
vfs unmount /mnt/snapshot
vfs unmount /mnt/work
vfs load /root/volume.img /mnt/restored
cat /mnt/restored/alias
vfs unmount /mnt/restored
```

| 命令 | 精确语义 |
|---|---|
| `vfs mount PATH BYTES INODES` | root 登记新 /mnt/NAME 内存卷，或在已有内部卷的目录下登记新名字的嵌套卷；不覆盖已有宿主/卷内对象；BYTES 为逻辑 payload 上限，INODES 包含根 inode |
| `vfs mounts` / `vfs unmount PATH` | 列出内部挂载／卸载；有解析 pin、文件或目录租约时 Busy；不是宿主 umount |
| `vfs inode PATH` / `vfs stats PATH` | 统一Host/卷元数据（显式身份域、未知归属与不同时间语义）／卷字节、inode、目录项与读写计数；不伪造物理占用 |
| `vfs link SOURCE DEST` | 同内部卷或同Host受管普通文件硬链接，目录拒绝，跨后端 CrossDevice；Host必须对象所有者/root、同存储类别/资源域且目标不存在；[27项专项、默认实跑及573项全量通过](filesystem.md#host-hardlinks-2026-11-15) |
| `vfs symlink TARGET DEST` / `vfs readlink PATH` | 内部卷绝对目标以卷根解释；Host受管链接绝对目标以虚拟根解释、相对目标以链接父目录解释，最多40次；Host增量[专项及默认程序实跑通过](filesystem.md#host-symlinks-2026-11-15)；后续[跨Host/内部卷共享解析](filesystem.md#cross-backend-2026-11-16)627项全目标冻结回归及默认程序实跑通过 |
| `vfs symlink-global TARGET DEST` | 显式绝对虚拟命名空间链接；可在DataRoot门面跨Host/内部卷，整个walk共享40次预算；[当前验收状态](filesystem.md#cross-backend-2026-11-16) |
| `vfs rename SOURCE DEST` | 同内部卷或受管Host覆盖rename；旧目标FD和其余硬链接保留；不同类型/非空目录/跨域拒绝；[Host专项与真实双进程通过](filesystem.md#host-rename-2026-11-15) |
| `vfs quota PATH UID BYTES INODES` | root 设置 UID 配额；已用量超过新限额则事务失败 |
| `vfs snapshot SOURCE DEST` / `vfs clone SOURCE DEST` | 卷根复制元数据／索引并共享不可变 4KiB 数据页；snapshot 只读，clone 可写；冻结快照经保存/重载保留只读属性；独立镜像间不共享磁盘块 |
| `vfs save MOUNT IMAGE` / `vfs load IMAGE MOUNT` | 保存/加载带校验镜像；保存必须指向宿主后端的根内受管普通文件，加载结果明确 Clean 或 RecoveredReadOnly |
| `vfs pseudo proc\|sys\|dev PATH` | 在新 /mnt/NAME 或内部卷目录下的新名字挂生成信息或虚拟字节设备；默认 /proc /sys /dev 另有不可卸载的 System provider |

普通 `cat/mkdir/touch/rm/rmdir/mv/cp/chmod/chgrp/getfacl/setfacl/find/du/exec`、重定向和 `owned` 接入内部卷；`mv` 仍不覆盖，只有 `vfs rename` 显式覆盖。所有账号路径门仍有效，普通用户不会因为卷内 mode 可写便获得 /mnt 写权限。

```text
vfs pseudo proc /mnt/proc
cat /mnt/proc/identity
cat /mnt/proc/uptime
vfs pseudo sys /mnt/sys
cat /mnt/sys/limits
vfs pseudo dev /mnt/dev
printf discarded > /mnt/dev/null
fd open /mnt/dev/zero read
fd read 0:1 4
fd closeall
vfs unmount /mnt/dev
vfs unmount /mnt/sys
vfs unmount /mnt/proc
```

proc 的 identity 在每次 open 按调用者虚拟凭据生成；uptime 是该 provider 挂载实例运行时间，不是宿主开机时间。每个打开描述中的生成正文在部分读取间保持不变。dev/null 读 EOF、写丢弃；zero 有界读返回零、写丢弃；full 写非空明确 NoSpace。没有宿主设备、进程或路径透传。

**验证**：21 项 VFS 专项含两个真实进程保存/加载、未知根外哨兵保留；后续新增的受管接口及配额边界以完整回归为准。持久化、容量和未实现项见[能力边界](filesystem.md#internal-vfs-2026-11-14)。

### 运行态文件与数据页统计（2026-11-15）

```text
mkdir /run/session
printf ready > /run/session/state
cat /run/session/state
owned stat /run/session/state
exit
```

以上命令只在 ElaraOS 中执行。新运行态文件报告 RuntimeState / TaskExit；会话退出按登记精确回收。CLI 崩溃后下一次启动先核验租约是否已死，再回收所属对象；不会扫描删除未知 /run 文件。普通用户原路径门不变，示例使用虚拟 root。API 新建必须提供 CreationOptions.scope；已有运行态文件的读取不要求新 scope。历史 OwnerRequest 目录保留，不自动认领。

`vfs stats` 可报告非零数据页分配、共享页引用和 COW 复制字节；snapshot/clone 共享数据页，但仍复制元数据，save/load 仍是完整 checkpoint。用法和实测以[运行态与 COW 账本](filesystem.md#runtime-cow-2026-11-15)为准。

### 挂载目录与工作目录（2026-11-15）

`ls /mnt`、`find /mnt`、`du /mnt`、`echo /mnt/*` 统一看到挂载目录项；支持 `vfs mount /mnt/work/parent/child BYTES INODES`，其中 parent 必须是已存在的真实卷内目录，不能是 symlink。子卷不随父卷 save/snapshot 隐式导出。

`cd` 使用 search-only 工作目录租约，进入 0111 目录不额外要求 read。内部 cwd 规范化到实际目录；其目录/祖先在租约存在时不允许改名或删除，返回 Busy。上下文/后台任务 clone 共享租约；离开目录且最后持有者释放后才允许卸载。持有列出子卷的目录快照同样使该子卷 Busy，关闭快照后释放。详见[文件系统命名空间契约](filesystem.md#namespace-2026-11-15)。

当前专项真实命令链已通过，完整回归进行中；不把卷内 symlink 说成支持跨挂载。

### 默认虚拟信息与设备（2026-11-15）

不需要先执行 vfs mount：`cat /proc/identity`、`cat /sys/version`、`cat /sys/limits`、`printf discarded > /dev/null` 可直接使用。有限读取用 `fd open /dev/zero read`、`fd read 0:1 4`、`fd closeall`。`/dev/full` 非空写明确 NoSpace。guest 也可进行这些设备 IO，但不能创建/删除/替换设备或修改系统目录。

`vfs mounts` 输出路径与 User/System(Proc)/System(Sys)/System(Dev) 类型。系统 provider 不能卸载；启动发现这些目录已有未知对象时报告 SystemProviderConflict 并保留原物，不静默隐藏或清空。详见[实现与验收](filesystem.md#system-providers-2026-11-15)。

### 块镜像、认证密封、去重与冻结overlay（2026-11-15）

以下为ElaraOS内部命令，不是宿主Shell命令。镜像/密钥须根内允许的UserData受管普通文件；这些管理命令要求root。

| 命令字面值 | 实际语义 |
|---|---|
| `vfs disk-create MOUNT IMAGE SLOTS CACHE_PAGES` | 新建ELARABK1镜像；8..65536个4KiB槽、1..512缓存页；容量预检失败不创建文件 |
| `vfs disk-save MOUNT IMAGE CACHE_PAGES` | 显式提交变化记录、新清单和另一根；不自动持久化每次内存写入 |
| `vfs disk-load IMAGE MOUNT CACHE_PAGES` | 完整验证并物化内存卷；回退代为RecoveredReadOnly |
| `vfs disk-stat IMAGE CACHE_PAGES` | 本次打开的缓存/IO统计和槽分配统计，不是常驻历史累计或宿主物理用量 |
| `vfs disk-repair IMAGE CACHE_PAGES` | 验证图后重发幸存根；不恢复损坏版本已丢的数据 |
| `vfs keygen KEYFILE` | 新建32字节随机0600密钥；不接受资源卸载生命周期，不输出密钥 |
| `vfs seal MOUNT IMAGE KEYFILE` | ChaCha20-Poly1305认证密封ELARAFS1 checkpoint；不是透明块加密 |
| `vfs unseal IMAGE MOUNT KEYFILE` | 先认证后解析；错误密钥/篡改拒绝且不挂载 |
| `vfs dedup MOUNT` | 显式卷内页去重；全字节比较和后续COW隔离；输出引用减少而非RSS/磁盘释放 |
| `vfs overlay LOWER UPPER DEST [WHITEOUT...]` | 分别冻结源卷、upper覆盖、目录合并；whiteout是卷内绝对路径；结果独立可写，不跟随源变化 |

保存前的内存修改仍可能丢失；密钥与密文同根不等于离线隔离。冻结快照可保存为独立命名镜像，但没有共享块快照目录管理；frozen overlay不是live overlayfs。详见[块镜像](filesystem.md#block-store-2026-11-15)、[密封](filesystem.md#sealed-images-2026-11-15)、[去重与overlay](filesystem.md#dedup-overlay-2026-11-15)。

### 保存并恢复冻结快照（2026-11-15，专项与真实进程已通过）

以下命令在ElaraOS内部依次执行；使用新的镜像名字，不覆盖已有文件：

```text
vfs mount /mnt/work 65536 64
printf original > /mnt/work/file
vfs snapshot /mnt/work /mnt/snapshot
vfs disk-create /mnt/snapshot /root/frozen.disk 64 1
vfs unmount /mnt/snapshot
vfs disk-load /root/frozen.disk /mnt/restored 1
vfs stats /mnt/restored
vfs clone /mnt/restored /mnt/editable
printf changed > /mnt/editable/file
cat /mnt/restored/file
```

恢复卷显示`access: FrozenSnapshot`，最后读取仍为`original`。直接对恢复卷写入返回`ReadOnly`；需要修改时显式clone。`Clean`仅表示镜像没有损坏，不表示卷可写。`disk-repair`消除恢复降级，不清除快照冻结属性。普通旧镜像没有冻结标记，保持原来的可写恢复语义。格式与边界见[快照属性持久化](filesystem.md#persistent-snapshot-2026-11-15)。

### 跨内部挂载链接（已验收）

`vfs symlink-global /mnt/data/file /mnt/front/link`创建显式全局虚拟链接；`vfs symlink file /mnt/front/local`仍按原有卷内规则解释。读写和`cd`可跟随跨挂载目标；`rm`删除链接本身，`vfs readlink`输出原字符串。历史版本目标落到Host时返回`CrossDevice`；后续[共享解析增量](filesystem.md#cross-backend-2026-11-16)支持受权限和身份门约束的Host落点，缺失目标仍报不存在；该命令不创建宿主原生链接。不同卷之间的`mv`/硬链接仍拒绝。详见[跨挂载解析边界](filesystem.md#cross-mount-links-2026-11-15)。

### 虚拟任务租约目录

```text
sh -c 'touch /tmp/proc-lease; ls /proc/tasks; cat /proc/tasks/*/status'
```

租约在首次创建操作时惰性登记；纯读取脚本不会产生条目，空目录上的未匹配通配符不能用于cat。示例先创建任务临时文件，最后租约持有者退出时回收；交互shell嵌套sh共享会话租约，子sh返回不一定已回收。这里的ID是资源租约ID，不是`jobs`的`%ID`，也不是宿主PID。状态为`LeaseHeld`、`Recoverable`、`MissingLease`或`Unconfirmed`，不把持锁写成Running。状态内容仅owner/root可读，观察不会触发清理。退出后名称消失，已打开FD保留原内容。详见[proc租约目录](filesystem.md#proc-task-leases-2026-11-15)。

### 作用域作业状态目录

```text
sleep 30 &
kill -STOP %1
cat /proc/jobs/*/status
kill -TERM %1
wait %1
ls /proc/jobs
```

这里的sleep与kill只作用于Elara内部worker，不启动宿主命令或向宿主进程发信号。`state: Stopped`要求worker已确认暂停；`PauseRequested`不是Stopped。路径中的观察ID与`job_id`是不同编号域，不能把观察ID直接用于kill。wait回收后新名称消失，打开的status FD保留旧内容。详细边界见[作用域作业目录](filesystem.md#proc-runtime-jobs-2026-11-15)。

### 打开对象身份比较

在新会话中执行`fd open /root/identity create`，用输出token执行`fd dup TOKEN`，然后`fd same TOKEN DUP_TOKEN`应输出SameObject。独立打开也可比较；对象替换后旧新FD为DifferentObject；Host不能确认时明确Unconfirmed，不比较路径或字节猜身份。具体实跑证据见[身份账本](filesystem.md#opened-identity-2026-11-15)。Host协作锁原专项及默认程序实跑通过；后续双见证身份、V3出生协议与按冲突扫描27项专项通过，默认程序复测亦通过，当前全目标560项通过，严格Clippy与默认构建通过；详见[锁账本](filesystem.md#host-locks-2026-11-15)，不能将本身份命令的证据挪作锁验收。

### 2026-11-15 Host受管普通文件硬链接

以下序列已由默认程序在独立测试根实际执行：

```text
owned create /root/a original
vfs link /root/a /root/b
fd open /root/a read
fd open /root/b read
fd same 0:1 1:1
owned write /root/b changed!
printf OLD=
fd read 0:1 8
printf NEW=
cat /root/a
exit 0
```

输出包含`SameObject`及`OLD=originalNEW=changed!`。重启后经`owned rm`、`owned mv`删除全部名称，预先打开的FD仍可读`changed!`。命令不操作宿主账号、权限或挂载；目录同步仍可能明确报告`PermissionDenied`而不是伪装已持久。完整证据及全量进度见[Host硬链接账本](filesystem.md#host-hardlinks-2026-11-15)。


### legacy计时与当前前台执行观察

```text
sleep 0
cat /proc/jobs/*/status
cat /proc/executions/*/status
```

第一类带`task_id`和`kind: legacy-task`，已完成计时显示`TaskExited(0)`，有界历史在身份切换/会话结束退休。第二类显示当前这次前台解释器调用自身，返回后不保留历史；后台作业仍由`jobs`管理，不重复算为前台。所有目录均为虚拟只读投影，不读取宿主进程。实现与验收边界见[前台观察账本](filesystem.md#proc-foreground-2026-11-15)。
