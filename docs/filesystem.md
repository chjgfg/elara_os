# 文件系统实现与验收

> 当前任务来源：用户确认“当前会话做文件系统，另一会话做 Shell 脚本解析”，要求实现和测试按职责分开，并进一步要求完整文件系统能力版图“全都做”。本文是文件系统实现与验收的单一账本，不替代命令账本或脚本会话的文档。

## 完整能力账本

**当前增量：2026-11-16。** 最新完整冻结基线为627 passed / 0 failed（400核心、189脚本、38其余），严格Clippy与默认程序双进程实跑通过，详见[跨后端解析验收](#cross-backend-2026-11-16)。不以测试数替代九层完成率。

“基础已有”不等于整项完成；宿主实现不等于自有磁盘格式或独立内核实现。后续按依赖顺序持续推进，不删掉尚未完成的项。

| 序 | 能力 | 当前状态 / 剩余工作 |
|---|---|---|
| 1 | VFS 路径与命名空间 | 内部 Registry 最长前缀解析已接；cwd pin、嵌套挂载与挂载目录遍历已验收，显式symlink-global与Host双向链接、统一40次预算已通过[627项全量](#cross-backend-2026-11-16)。 |
| 2 | 目录对象与遍历 | 固定目录句柄、快照分页/定位/刷新、有界树遍历已实现；Unix 原生未测 |
| 3 | 文件对象与句柄表 | 共享对象、代际/表身份校验、重复句柄、会话退出与账号切换释放已实现并接 fd；任务 FD 引用回收已接，当前权限/生命周期及sticky矩阵408/408通过，见文末最终矩阵 |
| 4 | 打开模式 | 类型化 access/creation/behavior、独占创建与统一分类门已实现并接 fd |
| 5 | 随机 IO 与长度 | 逻辑位置、read_at/write_at、seek/truncate、追加位置异常显式状态已实现并接 fd |
| 6 | rename/move/copy | 一般 rename 和有界新文件原子 copy 后端已实现；mv/cp 已接共享 Shell/脚本命令；打开目录的宿主限制见正文 |
| 7 | 虚拟链接 | 内部卷硬链接/符号链接已实现；硬链接共享 inode；普通symlink保留卷内语义；显式symlink-global跨内部挂载，不跟随宿主链接。 |
| 8 | 虚拟 stat 与元数据 | 内部卷有独立 inode/nlink/逻辑时钟及带卷 ID 的身份域；宿主后端保持原 ObjectId/代际。逻辑时钟不伪装为宿主时间。 |
| 9 | 所有者/组/umask/ACL/特权 | 执行 setuid/file SGID 和写入清位已验收；内部卷接 access/default ACL、继承 umask、chgrp、执行映像。路径/类别上界仍生效。 |
| 10 | 写入归属与生命周期 | 受管文件、任务临时对象及 RuntimeState 租约已接；/run 与 /var/run 新对象强制有效任务 scope，精确回收、活任务及未知/替换对象保留；应用/缓存登记、关联位置及整组卸载已验收，压力自动驱逐未接 |
| 11 | 并发/共享打开/锁/删除语义 | 内部卷共享/独占打开、强制共享/独占锁、打开后 unlink/覆盖 rename 已接；受管Host协作锁/独占共享已接并通过V3全量验收（未知归属Host仍Unsupported），跨卷 rename 明确拒绝。 |
| 12 | 自有缓存 | ELARABK1显式镜像IO接有界1..512页LRU、脏淘汰/写回与命中统计；不是透明挂载缓存，disk-load仍完整物化内存卷。 |
| 13 | 数据/目录同步 | 句柄 sync_data/sync_all、目录同步、原子提交及rename父目录凭据已实现；真实断电未测 |
| 14 | 分配/空闲空间/碎片/耗尽 | 根内镜像4KiB槽分配、双代引用保护、空闲孔洞复用、耗尽拒绝及最大连续空闲段统计已接；碎片/容量专项通过。不是宿主真实磁盘分配器。 |
| 15 | 自有磁盘布局 | ELARAFS1压缩/去重checkpoint保留；新增ELARABK1双超级块、稳定inode/目录元数据分页和稀疏原始数据页，显式disk-save只写变化数据和新清单/根。 |
| 16 | 磁盘级一致性与恢复 | ELARABK1持久COW：保护两代引用→写新数据/清单→同步→发布另一根→同步；故障后只读，重开旧版或完整新版。未做真实断电试验，无透明逐次写入持久化。 |
| 17 | 校验/坏块/只读降级/修复 | checkpoint双副本校验保留；块镜像校验根/清单/记录，最新坏只读回退，皆坏拒绝；disk-repair验证图后重发幸存根，不恢复已丢失新数据，不接真实设备。 |
| 18 | 注册/挂载/卸载/繁忙检测 | 内部注册、卸载、文件/目录打开引用及 Arc pin 繁忙检测已接；不挂载宿主目录。cwd/search-only 父目录租约与嵌套挂载已验收；显式symlink-global跨内部挂载已验收。 |
| 19 | 特殊文件系统 | 内存卷、显式 pseudo 及默认 /proc /sys /dev 已接；默认 provider 已通过完整回归。提供虚拟身份/版本/运行时间/上限与 null/zero/full，不读取宿主进程或设备；资源租约、scoped JobState目录与legacy TaskTable计时观察已接（见下文），前台解释器执行目录已接，扩展验收中。 |
| 20 | 容量与配额 | 内部卷逻辑字节、inode、UID 配额已接；目录项/配额记录另有 4096 上限。运行态登记/租约/精确回收已验收；应用/缓存逻辑字节/注册对象配额及关联整组卸载已通过专项；完整物理计量仍未实现。 |
| 21 | 统计与只读诊断 | list、句柄 info、stat、有界 walk 与逻辑 usage 已实现；stat/find/du 已接命令，逐节点授权且不返回冒充完整的部分结果；日志可查协议预算；物理空间/全量 IO 统计待实现 |
| 22 | 高级能力 | 快照/克隆改为复制元数据和共享 4KiB 不可变数据页，写入时按页 COW；镜像 RLE/去重已接。共享成本正反对照已通过；磁盘显式COW已接；认证加密checkpoint、卷内运行时页去重和冻结overlay专项通过；独立镜像保存冻结快照已通过专项；共享块快照目录管理、live overlay及网络FS仍未实现 |

实施顺序：对象/句柄 → 目录/命名/元数据 → 归属/生命周期/权限/一般锁 → VFS/内存与特殊后端 → 缓存/配额/同步 → 根内自有镜像格式/恢复/校验 → 高级后端。自有格式只使用虚拟根内的普通镜像文件，不操作真实磁盘、分区、启动配置或宿主挂载。


## 入口与职责

- 项目入口：[README](../README.md)。
- 设计约束：[无垃圾与流畅](design-principles.md)。
- `src/filesystem.rs`：薄门面，仅注册模块和导出稳定 API。
- `src/filesystem/backend.rs`：能力目录句柄、受限读取、创建、追加、删除、共享命名锁入口。
- `src/filesystem/modes.rs`：类型化打开模式，不允许只读创建/截断/追加，截断与追加不能同时构造。
- `src/filesystem/ownership_model.rs`：可往返验证的 V1 文本 catalog、虚拟 ObjectId/存储代/所有者/mode。
- `src/filesystem/ownership.rs`：归属事务、公共名称绑定、受管文件新建/改写/移动/删除/chmod 与恢复。
- `src/filesystem/owned_access.rs`：账号入口、旧文件只读与别名拒绝、受管文件能力授予。
- `src/filesystem/binding.rs`：同对象/不同对象/无法确认的显式锁探测，不以内容 hash 猜对象身份。
- `src/filesystem/open.rs`：能力根内的打开、独占创建与分类策略，不处理登录身份。
- `src/filesystem/objects.rs`：共享文件对象、逻辑位置、随机 IO、长度与文件同步。
- `src/filesystem/handles.rs`：有表身份和代际的句柄、复制/转交、关闭/退出释放、表容量。
- `src/filesystem/directory.rs`：固定目录句柄、排序快照、分页/定位/刷新。
- `src/filesystem/metadata.rs`：普通元数据、阻断链接识别、时间与目录同步的显式支持状态。
- `src/filesystem/namespace.rs`：一般 rename 与原子发布的新文件 copy，不做复制删除式移动回退。
- `src/filesystem/namespace_access.rs`：账号命名分派，不用目标数据读权限代替rename授权；最终后端重新取锁验证。
- `src/filesystem/traversal.rs`：有数量/深度上限的迭代遍历；不跟随宿主链接，不做有损名称转换。
- `src/filesystem/diagnostics.rs`：只读逻辑用量统计，不伪造物理分配量。
- `src/filesystem/policy.rs`：普通写入的统一分类门及布局目录保护。
- `src/filesystem/transaction.rs`：完整内容的原子替换、暂存和锁。
- `src/filesystem/recovery.rs`：启动时回滚有归属的事务残留。
- `src/filesystem/log_store.rs`：受管日志分代、长度/hash提交凭据、源头限额、轮转跟进、未知数据保留、兼容视图及显式持久化状态。
- `src/filesystem/types.rs`：错误类型、提交凭据和恢复结果。
- `src/config.rs`：集中保存目录布局、写入类别、事务目录标记与大小上限。
- `src/initialization.rs`：能力目录内准备布局并恢复，prepare_open 转交固定根句柄，不重开宿主路径。
- `src/filesystem/host_boundary.rs`：配置/初始化/打开共用的专用本地根位置校验。

测试全部位于 `tests/filesystem/`，与功能对应：`io.rs`、`policy.rs`、`boundary.rs`、`transaction.rs`、`recovery.rs`、`modes.rs`、`open.rs`、`objects.rs`、`handles.rs`、`directory.rs`、`metadata.rs`、`namespace.rs`、`traversal.rs`、`diagnostics.rs`、`binding.rs`、`ownership.rs`、`owned_open.rs`、`access.rs`、`default_acl.rs`、`host_boundary.rs`。测试模块由薄门面的 `cfg(test)` 路径注册；私有故障注入和守卫摘除不会进入生产构建。

## 普通写入策略

系统路径 `/bin`、`/sbin`、`/lib`、`/usr`、`/boot`、`/dev`、`/proc`、`/sys` 及其后代只读，普通写入 API 不因 root 身份而绕过。错误为 `ReadOnlySystemPath`，不是“根目录受保护”。更新/安装事务尚未提供；原子替换 API 本身仍遵守普通只读规则。

其他分类：用户数据、临时数据、缓存、日志、运行时状态、配置、应用。前缀匹配有路径组件边界，Windows 覆盖大小写别名；没有分类的写入明确拒绝，不默认为可写。

目录分类与用户/任务权限是两层：分类描述存储用途，上层仍通过现有权限接口检查身份。任务文件所有权、临时/运行态租约及精确回收已实现；应用资源登记与缓存登记/精确回收已通过专项；应用外部数据/缓存关联已接、专项通过；压力自动驱逐尚未接。不提供全量 `/tmp`/`/run` 清空；日志限额轮转仅对本轮创建的受管日志实现，历史未知归属数据不自动接管。

## 能力目录边界

固定版本 `cap-std 3.4.4` 提供 Rust 安全接口。只有最初对已验证根目录的授权使用宿主路径；之后数据 IO 只通过持有的目录句柄和相对路径操作。

原来的链接/reparse point 检查保留；实际打开操作不再退回 `std::fs` 绝对路径。测试将已检查的父目录替换成指向测试根之外的 junction：普通宿主路径读取能读到哨兵，能力目录打开/写入均拒绝，哨兵不变。

这是已授权根内 IO 的约束，不是完整 OS 级沙箱：不限制恶意改过的程序使用其他宿主 API，也不承诺抵御首次授权时的恶意根目录替换、宿主挂载操作或全部根内策略竞态。

## 原子替换与代价

`write_file` 保持原 `Result<(), FsError>` 兼容接口；需要提交细节时调用 `replace_file`，返回 `CommitReceipt`：

- 文件内容全部写入并 `sync_all` 后才替换。
- 替换只有原子 rename 一条路径；不能原子替换时返回 `AtomicReplaceUnavailable`，**没有复制覆盖回退**。
- 提交后目录同步结果为 `DirectoryDurability::Synced` 或 `Unconfirmed(ErrorKind)`；Windows 的目录 flush 不支持/权限限制不会被冒充为断电持久化保证。
- 提交后暂存清理结果为 `CleanupStatus::Complete` 或 `Deferred(ErrorKind)`。已提交但跟进失败，不误报为“旧文件未变”。

普通兼容接口保证原子可见性，不承诺目录项的断电持久化；应用若依赖后者必须读取显式凭据。

每次替换的成本包括事务锁、独立目录与归属文件、暂存文件写入/同步、rename、目录同步尝试和归属清理。事务 payload 上限为集中配置中的 1 MiB。追加仍是追加接口，不冒充原子完整替换。

## 暂存与恢复选择

采用设计原则 P3 的方案 (a)：暂存只在虚拟 `/tmp/.elara-transactions/` 的系统私有目录，不在用户文件旁生成 `.elara-staged-*`。

私有目录有专用归属标记；每个事务使用原子创建的独立目录、归属文件和 payload。普通虚拟路径不允许访问这个保留组件。根内事务锁由 Rust 1.89 起稳定的文件锁持有（Cargo 已声明最低版本）；正常退出/进程终止后由 OS 释放；恢复和替换共享同一锁，不能在写入过程中清走活跃事务。

恢复只丢弃有归属的未完成暂存，不重放或覆盖目标文件。未知名字、错误归属、额外文件或链接保留；未标记且非空的暂存目录明确报错，不接管、不删除。普通 `/tmp` 用户文件和历史同目录暂存文件不扫描清理。

跨设备不能 rename 的分支通过故障注入验证，旧内容保留；没有据此宣称做过真实跨卷迁移或断电试验。

## 文件对象、打开模式与句柄

- `FileMode` 字段私有，通过 `new(FileAccess, FileCreation, FileBehavior)` 构造；`READ` 和 `CREATE` 是明确常量。只读不能创建/截断/追加；`Truncate`、`Append` 是互斥枚举，不存在“两种同时开启”的非法组合。
- `OpenFile` 持有能力根内实际打开的文件，`clone` 共享同一个对象、互斥锁和逻辑位置；再次 `open_file` 是独立位置。`opened_path` 是打开时的名称，不冒充 rename 后的新名称或虚拟 inode。
- `read` / `write` 是单次、有界的流式 IO，允许短读/短写并返回实际字节数，不是完整内容原子替换。默认每次最多 1 MiB；旧 `cat` 显示上限仍是 64 KiB，旧 `TooLarge` 文本保持兼容，句柄上限使用 `IoRequestTooLarge`。
- `read_at` / `write_at` 不修改逻辑位置；每次普通 IO 都从逻辑位置重新定位，不依赖宿主指针是否被随机 IO 改动。`seek` 支持 Start/Current/End，负位置/加法溢出拒绝；截断不改变当前位置，扩展由宿主后端零填充。
- Append 始终写到 EOF，不受前面的 seek 影响；追加句柄的 `write_at` 拒绝。若内容已写但位置查询失败，返回 `WritePositionUnconfirmed { bytes, error }`，位置显式变为 `FilePosition::Unconfirmed`；不能把旧位置当新位置，不重试写入。必须从 Start/End 重新定位才恢复顺序读取/相对定位。
- 只读句柄拒绝 write/write_at/truncate/**文件同步**；只写句柄拒绝 read/read_at。对象锁中毒明确拒绝访问，不取出内部数据继续运行。
- `HandleTable` 默认上限 256，创建/截断前先检查表容量。令牌包含表身份、槽位与代际；关闭、close_all 和复用后旧令牌都拒绝，代际溢出时永久退役该槽位，不回绕。复制句柄共享位置；转交给另一表获得目标表令牌；释放一个表不破坏其他表仍持有的引用。
- 表或会话退出释放**它持有的引用**，不自动删普通数据文件；其他合法持有者的引用继续有效。接入任务退出与普通临时文件的持久生命周期仍是后续工作。
- CLI 薄适配由 Shell 会话维护，使用 `fd` 子命令族。`SLOT:GEN` 是当前会话的局部文本令牌，不是跨会话授权；账号切换要关闭旧表、保留递增代际，不能让普通用户拿到旧 root 能力。底层 DataRoot 分类门不替代上层账号授权。

## 目录、元数据、命名和只读诊断

- `open_directory` 返回持有目录能力的排序快照游标：分页最多 256 项、快照最多 4096 项。定位/rewind 操作当前快照；refresh 成功才替换并回到起点，失败保留旧快照和位置。隐藏协议保留名，不把快照描述成实时目录。
- Windows 当前 `cap-primitives 3.4.6` 的目录句柄刻意关闭 `FILE_SHARE_DELETE`，这是其路径式能力边界的一部分，**没有修改这道门**。打开目录游标时移动该目录返回 `BusyPath`（宿主共享冲突 32/33）；打开子文件时移动非空父目录也可能被宿主拒绝，原错误保留在 `AtomicRenameUnavailable`。关闭相关句柄后相同移动成功。Unix 的“目录改名后游标仍定位原目录”是独立测试，本机未测。
- 一般 `rename` 支持普通文件与目录树移动，拒绝布局目录、系统只读路径、既有目标和移动到自身后代。创建/删除/可写打开/追加/rename/事务恢复共用根内命名锁，锁失败不无锁回退。**不覆盖既有目标的保证针对遵守锁的本系统写入者**；敌对宿主直接写入导致的竞态不在该保证内。原子移动不支持时明确失败，不改做复制删除。
- `copy_file` 只复制观测到的内容到新普通文件，不克隆所有者/权限/时间。最大 1 MiB；复用私有事务暂存，经 payload 同步后以硬链接**原子发布新目标**，随后移除暂存链接。复制品不与原源文件共享对象。发布失败、目标占用或不支持链接时不退回目标旁写入，不发布半内容。这是宿主内部提交原语，不是虚拟硬链接功能。
- `stat` 明确 File/Directory/BlockedLink/Other；末级链接只识别不跟随，中间链接拒绝。时间是 `Timestamp::Available` / `Unavailable(ErrorKind)`；`host_readonly` 只是宿主属性，不是虚拟账号权限，不杜撰 uid/gid/inode。
- `sync_directory` 与 rename 的两个父目录同步均返回 Synced/Unconfirmed；文件句柄的 sync_data/sync_all 不等价于目录项持久化。
- `walk` 迭代遍历，默认最多 16384 项、64 层，不跟随宿主链接，不把无效/非 UTF-8 的宿主名字有损转换成另一虚拟路径。超限/中途失败不把部分结果冒充完整树。它是各目录的观测集合，不是整个磁盘的原子快照。
- `usage` 报普通文件逻辑字节数、文件/目录/阻断链接/不可寻址名称数量；物理使用量显式 `UnavailableOnHostBackend`，不把逻辑字节当作已分配块或对宿主硬链接去重。私有协议存储不包括在普通命名空间统计中。计数溢出报错，遍历和统计不创建事务存储，也不删除数据。

## 宿主增量：命令桥与日志协议（2026-11-08）

### 现有能力接入

`mv/cp/stat/find/du` 复用同一个模块适配器，文件系统业务不复制进解释器。`walk_checked/usage_checked` 在每个路径的 stat/目录枚举之前调用授权门；任意节点拒绝时不输出冒充完整的树/总量。一般移动仍不覆盖、不跨卷复制删除回退；复制仍为单普通文件、最大 1 MiB。Windows 目录同步不确定的凭据保持可见。

真实进程测试入口为 `tests/filesystem_cli.rs`；功能测试为 `tests/script/fs_commands.rs`。Shell 共享入口由另一会话维护，命令数量与语法契约以命令账本为准。

### 受管日志 V2

- 新建日志使用 `/var/log/.elara-logs/` 私有协议；普通路径禁止访问该组件，列举和遍历隐藏它。`store.owner` 为 V2 标记，manifest 是活动代/归档集合的唯一权威。每代有正文 payload 和纯文本 journal，journal 记录已提交长度及 SHA-256；hash 是完整性检查，不单独充当所有者证明。
- 新建代的 journal/payload 先写入 `/tmp` 下带 `ELARA_LOG_GENERATION_TX_V2` 归属的独立暂存目录，同步后整体 rename 到最终代目录；不在 `/var/log` 或用户文件旁准备半成品。启动恢复按 owner 版本识别 V1 替换和 V2 日志代暂存，V1 仍不接受额外 journal 文件。
- 默认单代正文最多 64 KiB、保留 4 个归档，journal 也有独立上限（单代正文上限的 4 倍加 256 字节）。`LogPolicy::peak_named_bytes` 另计正文、journal 和协议固定开销；这是**受管命名文件预算**，不是物理分配、未链接打开对象或其他未知文件的空间配额。共享 `/tmp` 事务暂存属于另一个协议，不把它混进日志空间统计。
- 正常记录仅在共享命名锁内追加正文和提交凭据，不逐条进行 fsync/完整文件替换；`LogDataDurability::HostBuffered` 明示代价。轮转才同步旧代、创建并同步新代、原子发布 manifest；目录同步仍为 Synced/Unconfirmed，不声称真实断电保证。
- `/var/log/elara.log` 是指向当前代正文的内部硬链接兼容视图，普通修改/追加/删除被服务归属门拒绝。Logger 的 tail 读 manifest/journal，不把兼容视图中的未确认尾部当成已提交记录。此链接不是用户可创建的虚拟链接功能。
- 轮转前后的 maintenance/pending 状态覆盖视图绑定、退休代与 `/tmp` 暂存清理；已提交而跟进失败单独报告，不能诱导重试同一条记录。下一次访问或初始化在同一锁内完成跟进，不增设定时任务。`NamespaceGuard` 绑定具体 DataRoot，私有发布和锁内恢复拒绝其他根的守卫。
- 半条追加或半条 journal 只在归属协议有效、已提交前缀 hash 匹配时裁掉未确认后缀；前缀被破坏时保留数据并报错。陌生文件、错误标记、额外内容不按名字扫删。
- 历史无标记的 `elara.log` 保持 LegacyReadOnly/Unmanaged：有界尾读、返回实际字节数，不自动截断、认领或施加新日志预算。旧 V1 受管目录也不静默升级为 V2；需另行迁移。普通对象 owner/ACL/配额并未因此完成。
- 性能修订：最初逐记录分代替换方案在全量中导致原有 `sleep 1 → ps` 判据失败；改到追加/轮转策略后，未调整时长或判据的同一测试复验通过（1 passed，1.91s）。不能将日志完整性测试耗时当成单条 IO 基准。

测试入口：`tests/filesystem/log_store.rs`（轮转上限、前后提交故障、半记录、hash/服务守卫摘除、旧日志与未知数据保留、同进程独立实例）；`tests/logging_cli.rs`（两个实际 binary 竞争记录并触发轮转）。最终数量以本轮验收记录为准，不提前标全量通过。

## 当前可验证命令

```text
cargo test --offline --lib --target-dir target/fs-build filesystem:: -- --nocapture
cargo test --offline --target-dir target/fs-build
```

测试/编译产物属于宿主项目 `target`；测试根属于 `target/test-data`。本轮没有对用户现有 `D:/os` 做清空、递归删除或迁移。

## 验收记录

- 写入策略阶段：13 项文件系统测试通过，包含摘掉统一写入门后 `/usr` 创建能成功的对照。
- 事务/能力边界阶段：23 项文件系统测试通过；包含 IO、权限分类、模拟部分暂存失败、模拟跨设备拒绝、保留未知数据、启动实际调用恢复和父路径替换的能力边界对照。
- 该阶段全量兼容：86 项库单元测试、39 项脚本语言测试、1 项脚本真实进程测试和 1 项原生终端测试通过。
- 2026-11-07 历史最终验收：24 项文件系统功能测试通过（含并发替换与恢复共享锁）；87 项库单元测试、39 项脚本语言功能测试、1 项脚本真实进程测试、1 项原生 ConPTY 测试全部通过，共 128 项非空测试。
- 真实进程文件系统验收通过：全新测试根中 `/usr` 文件创建、`/bin` 目录创建均拒绝；用户文件创建/读取/删除与目录删除闭环成功；root 密码更新触发 shadow 原子替换成功，`/etc` 旁无暂存文件，事务私有目录没有已完成事务残留；退出码 0。
- 2026-11-08 对象/目录/命名增量：70 项文件系统功能测试通过；全量 `cargo test --offline --target-dir target/fs-build`：133 项库测试、52 项脚本语言测试、1 项 fd 真实进程、1 项脚本真实进程、1 项原生 ConPTY，共 **188 项非空测试**通过。该快照覆盖账号切换清掉 root 句柄、同槽复用拒绝旧代际、脚本共享句柄、写入正文不进入日志。实际 Rust 测试程序启动真实 binary，并非只以进程 exit 0 代替逐命令断言。
- 本轮错误修订：只读文件句柄 Windows flush 返回 PermissionDenied，统一改为可写句柄才可同步；打开目录时移动失败，读 cap-primitives 的安全门后保留禁 SHARE_DELETE，改为平台明确的占用契约并做关闭前失败/关闭后成功的对照，Unix 保留独立测试；`TooLarge` 文字改动破坏旧 cat 判据，恢复旧文字并为句柄新增独立 `IoRequestTooLarge`，未改外部测试来迁就回归。
- 2026-11-08 最终收尾快照：`cargo test --offline --target-dir target/fs-build` 为 133 项库测试、54 项脚本语言测试、fd/script/ConPTY 真实进程各 1 项，共 **190 项非空测试通过**；文件系统仍为 70 项，不累计历史批次。新增脚本空/非空谓词四组合与文档顺序逐条执行回归，closeall 放在最后使用之后。
- 构建：独立 `cargo build --offline` 成功，19.82 秒；Cargo metadata 确认默认 target/build 目录均为本项目 `target`，实查 `target/debug/elara-host.exe` 已生成。构建不运行初始化，不依赖或清空实际数据根。
- 静态检查：独立 `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings` 复验通过，2.63 秒、退出码 0。此前发现的脚本布尔表达式、测试故障构造、终端布尔断言及残留导入均按等价语义修正，未降低检查级别；脚本与终端测试修正由原会话维护。
- 2026-11-08 本会话宿主增量最终独立验收：`cargo test --offline --lib --target-dir target/fs-build filesystem::` **89 passed、0 failed，41.41s**；其中日志协议 19 项已包含在 89 内，不另加到总数。`--test script_language fs_command_tests` **8 passed、0 failed，5.98s**。`--test filesystem_cli --test logging_cli -- --nocapture` 两个实际进程验收各 **1 passed**（4.36s / 5.13s）；双账号/双进程共 10 个不同账号+命令 label 的事件，各出现恰好一次，敏感正文不入日志。
- 本会话严格 Clippy 全目标复验通过：`cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings`，4.59s、退出码 0。默认 `cargo build --offline` 独立通过，9.02s、退出码 0，不运行真实数据根。
- **2026-11-08 历史共享全量未通过（当前结果已由文末 2026-11-09 串行验收更新）**：最后一次 `cargo test --offline --target-dir target/fs-build` 在库阶段为 **148 passed、4 failed、152 项，53.84s**，退出码 101，Cargo 未继续该次外部集成套件。失败均在共享 Shell 统一入口后的旧边界判据：cat 末尾换行、mkdir 参数错误文字、旧“拒绝重定向”断言、旧“日志包含 echo 正文”断言。未替另一会话修改判据或调阈值；既有权限拒绝和原 sleep 1 → ps 已通过。以上独立验收不能冒充当前全量通过。
- 批次边界：本次实际完成 M1 命令桥与 M2 受管日志；普通对象 owner/ACL、任务资源与临时/缓存治理、配额、一般锁、VFS/内存/特殊后端（M3–M5）仍未实施。相关共享入口由另一会话重构，协调消息存在权限模式差异而未全部送达；不绕过边界修改其工作，不以 API 桩提前勾选完成。
- 未测：Linux 原生、真实断电、真实跨卷失败和全部敌对并发场景。完整能力版图的未完成项仍见本文开头账本。

## 修订记录

| 日期 | 修订 |
|---|---|
| 2026-11-07 | 文件系统从单文件拆为职责模块，保持已有 API；写入类别集中配置；加入能力句柄边界、只在 `/tmp` 暂存的原子替换与有归属恢复；测试全部迁至 tests/filesystem。修正不存在路径优先返回 NotFound、无标记事务目录返回专用错误的契约。 |
| 2026-11-08 | 本会话新增 mv/cp/stat/find/du 共用命令桥、逐节点授权遍历、受管日志 V2 限额/提交凭据/有归属暂存与恢复；修正逐记录同步造成的时序回归，保留原判据并复验。FS89、桥8、真实文件/双进程日志各1通过；严格Clippy和默认构建通过。最新共享全量148过4红，明确不勾选普通归属/权限/配额/VFS等后续项。 |
| 2026-11-08 | 新增文件对象/打开模式/代际句柄/随机 IO/同步、目录游标、统一宿主元数据、一般移动与新文件原子复制、有界遍历及逻辑统计；建立完整22项账本。修正只读同步、Windows目录占用、旧cat文本兼容回归，保留安全门并记录平台对照。Shell会话接fd薄适配，本会话独立全量初验188、最终190测试及严格Clippy/默认构建/真实fd进程验收通过。 |

<a id="owned-open-2026-11-09"></a>

## 受管文件句柄收口（2026-11-09）

来源：用户确认先落入 open_file_as、修复七项 Clippy，再将 fd 接入受管权限并做真实进程验收。目录所有权/目录权限 → umask → 组/ACL → 任务临时文件生命周期是后续依赖路线，未实现项继续保留在开头能力账本。

- open_file_as 与裸打开复用 existing_options/classify_open_error；缺失已有文件为 NotFound、目录为 IsDirectory；系统写入类别在旧归属判定前拒绝。创建固定为当前账号、0600；gid==uid 是私有主组模型，不是共享组。
- fd 使用 HandleTable::open_as；owner_uid 是表的会话主体，不等于对象 uid。fd stat 同时显示对象 ObjectId、storage_generation 和授予时 uid/gid/mode。表容量仍先于创建/截断检查。
- chmod 影响未来打开，不撤销已授予能力；rename 不变 ObjectId；replace 递增存储代，旧句柄仍绑定旧代；删除公开名称不使旧句柄重新打开新路径。clone/dup 共用位置，独立 reopen 不共用。
- 账号读门覆盖 cat/wc/输入重定向/流式来源/脚本文件/stat/find/du/glob；任一树节点拒绝不返回冒充完整的树或总量。旧文件保留只读，不按内容、大小或路径猜所有者；宿主硬链接别名及名称被替换无法确认时明确拒绝。
- cp 接账号读取和受管新建，目标由调用者拥有，不克隆源所有者；mv 对受管普通文件走归属事务。包含受管后代的目录一般 rename 目前拒绝，防止公开目录与 catalog 路径脱节；目录归属事务尚未实施。
- 普通临时文件仍为 OwnerRequest，不因 close/drop/启动而删除；任务生命周期、umask、共享组/ACL、目录对象权限没有被本轮能力快照冒充完成。

### 证据与当前状态

- 上轮内存候选不是落盘验收。本轮已落源码，正式 Cargo 首轮库 179 passed、fd 真实进程 2 passed；filesystem/logging/ownership/script 真实进程各 1 passed。
- 首轮默认并行脚本为 143 passed、2 failed，失败在原两秒取消/退出判据；独立并行 job_tests 为 10 passed、2 failed，串行保持原判据为 12 passed、0 failed，12.26s。没有改变时限，也不把串行通过冒充默认并行通过。项目此前稳定快照本就使用 --test-threads=1。
- 既有七项 Clippy 等价修复，不增加 allow；首次全目标 cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings 通过（6.45s）。最终快照全目标严格 Clippy 复验通过：11.85s、exit 0；默认 cargo build --offline 通过：15.17s、exit 0。一次最终链接因中间测试 exe 占用而失败（Permission denied、exit 101）；查明该测试进程已退出后重跑，未删文件、未改测试时限。
- 验收中补充 help 与目录后代守卫测试后，旧中间串行任务主动停止；不以旧任务输出充当最终源码结果。
- 最终命令：cargo test --offline --target-dir target/fs-build -- --test-threads=1。测试根只在 target/test-data，没有操作已有用户数据根。外部集成含真实 elara-host，不仅检查父进程退出码。

### 上一阶段最终落盘验收（历史快照，不代表下节新增源码）

- cargo test --offline --target-dir target/fs-build -- --test-threads=1：**180 个库测试 + 145 个脚本功能测试 + 7 个真实进程/ConPTY 测试 = 332 项非空测试全部通过，exit 0**。库 144.19s，脚本 152.52s；fd 实际进程两项合计 20.06s，filesystem/logging/ownership/script 进程各 1 项，ConPTY 1 项。主 binary 单元和 doc-test 各 0，不计入 332。
- fd 多会话实测：私有新建/其他用户拒绝；cat/wc/输入重定向/流式管道/stat/find/du 同样遵守对象读权限；chmod 收紧拒绝未来打开但旧授予可写；cp 目标归调用者、mv 保持身份；rename 后旧 opened_path、replace 后新旧代隔离；删除名称后旧句柄仍读原代；旧文件截断拒绝且原内容保留，活跃注册对象的宿主别名读取拒绝；身份切换旧 root 令牌失效。
- 最终源码/测试集合 **95 个 .rs 文件**在验收前后摘要一致：`0db777bba8dbdaccbb3c16b0fa2ad3b8006d9b6c934b29d1ab037a4f6620ea53`。摘要按 src、tests 顺序，各目录路径排序，逐文件相对路径 UTF-8、NUL、原始字节流累计 SHA-256。未在 runs/.scratch/TEMP 落自建中间文件；构建与测试夹具使用 target。
- 332 是最终串行快照，不冒充默认并行全绿；初始两项并行时限失败和一次链接占用失败保留在上面。七项 lint 等价修复，无 allow、无降低检查级别；原两秒判据未改。
- 本轮完成受管文件打开/真实 fd 权限链和跨命令集成。**当轮未实现这四项；现已接入，最新实现和未完成验收状态见下节**；没有清空用户现有 /tmp 或接管未知文件，没有操作真实磁盘或宿主挂载。

修订：2026-11-09，旧“最新全量未过/当前 Clippy 未过”保留为历史；332 项通过仅对应上一阶段快照，不累计到下节四项新增源码。

<a id="directory-access-task-2026-11-09"></a>
## 目录权限、umask、共享组/ACL、任务生命周期（当前串行矩阵已验收）

**目录SGID历史快照：四项、default ACL与目录setgid通过完整串行矩阵390/390及严格Clippy，默认程序已构建；验收前后108个Rust文件指纹一致。当前sticky源码验收见文末，不套用旧结果。106文件378项仅保留为前一历史快照；失败与修订不抹除。** 不沿用上一阶段 332 项通过结论。

### 入口与协议

- [权限/目录事务](../src/filesystem/access.rs)、[规范权限模型](../src/filesystem/access_model.rs)、[任务租约](../src/filesystem/scope.rs)、[权限命令](../src/script/access.rs)。新增测试入口：[文件系统不变量](../tests/filesystem/access.rs)、[真实多会话/进程终止/作业 CLI](../tests/fd_cli.rs)。现有 Shell/FD/命令账本应链接本节，不另存生命周期记录。
- `access.catalog` 是 `/var/.elara-objects/` 内的 `ELARA_ACCESS_CATALOG_V1`（无 default ACL）或 `ELARA_ACCESS_CATALOG_V2`（带目录 default ACL），以及下节目录setgid增量的`ELARA_ACCESS_CATALOG_V3`规范 JSON：目录、共享组/成员/主组覆盖、按稳定文件 ObjectId 关联的 access ACL、任务记录及单调计数。严格字段/权限/容量/ID 校验；重新编码必须与输入逐字节相同。该378项快照的`VirtualMode`反序列化单独验证`000..777`；当前目录setgid增量仅额外接受`02000`，普通文件仍只允许`000..777`，sticky/setuid仍拒绝。
- 文件归属 catalog 在有 `TaskExit` 对象时使用 `ELARA_OWNERSHIP_CATALOG_V2`，保留原七列并增加 `owner` 或 `task:ID`。既有 V1 严格往返读取，仍为 `OwnerRequest`；不因升级或启动把旧 /tmp 对象改成可回收对象。
- `access.pending` 持久意图包含旧/新权限 catalog、必要时旧/新文件 catalog，以及目录命名操作。目录发布、退休、树移动及 chmod/ACL 联合更新均走共享命名锁；恢复只接受意图声明的旧或新状态，并检查身份/代际/路径变更关系。所有账号入口、裸入口拒绝门及事务恢复先处理该意图，不无锁降级。

### 四项行为契约

1. **目录所有权/权限**：新目录取得持久 ID、uid/gid/mode/ACL/删除条件。准备和退休对象只在有归属的私有存储；最终目录内 `.elara-dir-owner` 被虚拟路径全局保留，并从 list/cursor/walk 隐藏。标记与私有 owner 的 OS 锁对象绑定是协议归属证明，不是能抵抗恶意宿主移植标记的 inode 证明。目录保留真实宿主布局；祖先需要 x，列举需要 r+x，创建/删除子项需要父目录 w+x。实际标记参与查找，Windows Unicode 名称别名不能只靠 ASCII catalog key 被当作未受管目录。新 home 建立账号目录归属，既有未知 home/文件/目录不自动认领。
2. **会话 umask**：默认 `0077`；无父 default ACL 时，新文件 `0666 & !mask`、新目录 `0777 & !mask`；有 default ACL 时按下节继承规则处理，不再扣 umask。接入 mkdir/touch/owned create/fd open/cp/输出及追加重定向；已有文件不重设模式。零模式新文件可以返回本次创建授予的 FD，但未来重新打开仍受零模式约束。同步 sh、替换及管道作用域隔离 mask，source 保留 mask，后台继承数值但独立任务租约，su 重置。重定向持有打开能力，不在命令副作用后靠名称重新打开。
3. **共享组/access ACL**：`groupadd GID NAME`、`groupmem add|del GID UID`、`groupmem primary GID UID`、`groups`；管理 root-only。`chmod MODE PATH`、`chgrp GID PATH`、`getfacl PATH`、`setfacl ENTRY[,ENTRY...] PATH`、`setfacl -b PATH` 支持受管目录及文件。ENTRY 为 `u:UID:BITS`、`g:GID:BITS`、`g::BITS`、`m::BITS`；BITS 为八进制 0..7。授权顺序 owner → named user → 匹配组并集 → other；named user/匹配组失败不能再退回 other。扩展组类受 mask 限制；chmod 与 mask 通过同一意图提交。目录/文件元数据只允许 owner/root 修改，普通 chgrp 要求成员资格。既有 FD/cursor 不因之后 chmod/ACL 收紧而撤销。
4. **任务临时生命周期**：任务租约由根能力 family 和 uid 绑定，持久 TaskScopeId 不回绕。Context 用共享懒租约承载同步子调用，纯 echo/sleep/取消不会创建租约；后台分离租约。新作用域对象仅在 `/tmp/` 使用 `TaskExit(ID)`，home/root 用户数据与显式持久 API 对象保留 `OwnerRequest`。正常退出、取消、panic 的 Drop、su 和会话结束接回收；FD 结束钩子只关闭该任务创建/dup 的槽位引用，不 close_all 其他任务。共享表中的明确令牌仍是能力，显式 dup 可转交引用，任务退出不撤销其他已授予引用。

### 回收与边界

- 启动只检查精确登记的任务锁：`WouldBlock` 保留活任务；取得锁后经**自己的锁句柄**有界读取/验证租约 magic，避免 Windows 锁内另开句柄读冲突；错误/坏 magic/未知内容保留并报告 deferred。缺锁且确实没有登记对象只退休空记录，不猜测有对象的任务已死亡。
- 清理按登记的 ObjectId/DirectoryId、uid、TaskScopeId、payload/marker 绑定逐项证明，只删除任务名称。目录按深度退役，未知额外子项、宿主替换、损坏标记、无法确认的对象均保留。生命周期回收是系统归属权限，不被用户 chmod 000 阻止；它仍不能删除未知对象或绕过系统写入类别。不递归扫删 /tmp，不自建计划任务/cron，不清理实际用户数据根。
- `TaskRecovery` 给出 files/directories/live/deferred；`cleanup()` 为 `Complete` 或 `Deferred`。Drop 不 panic；可观察边界记录回收失败。Windows 固定目录能力可能令移动/删除 Busy，保持该安全门，不加共享删除或复制删除回退。
- 保留路径上界：ACL/组不授权普通用户写别人 home、/etc 或系统只读分类；共享写入使用获授权的 /tmp 目录。`root` 也不能通过普通入口写系统只读目录。access ACL、default ACL继承及目录setgid在390/390历史快照已验收；后续sticky亦已接入并通过408/408最终矩阵，见文末修订。setuid/file SGID执行身份、虚拟链接与完整inode仍未实现，不实施宿主权限同步。
- 容量上限：权限 catalog 400 KiB、联合意图 1 MiB；目录 4096、file ACL 4096、共享组 256、每组成员 256、每对象 named ACL 64、任务租约 64。超限在公开命名变更前拒绝，不把宿主分配器/目录同步的未确认分支当作自研磁盘或断电保证。

### 已运行证据与修订记录

- 起步 shell heredoc 两次在 shell 解析阶段失败，未生成模块；改用可靠文件工具/短命令。首轮 compile 出现 V2 header 缺变量，及 useradd 将目录提交回执匹配为 `()`；均已改。后续 `cargo check --offline --all-targets --target-dir target/fs-build` 曾通过：9.50s、exit 0，仅对应当时快照。
- 新 12 项库测试首次 **11 passed / 1 failed，72.39s**：活任务被误计为 deferred，根因为 Windows 已锁文件经另一个句柄读取 magic 失败；已改为先 try_lock，再经自己的锁句柄核验。
- 后续 `cargo test --offline --target-dir target/fs-build -- --test-threads=1`：**192 项库测试中 189 passed / 3 failed，201.78s**；其中新增 12 项全部通过。失败分别为：未知 owner store 过早影响根结构打开；会话退出临时文件仍被旧判据要求存活；rm/rmdir 未先分类目录/文件。对应入口/分类已改，临时判据按任务退出契约修订。此批次在库失败后退出，**脚本及真实进程测试未运行**。
- 此后还增加 Windows Unicode 目录别名门、规范事务变更验证、完整 VirtualMode 反序列化门、CLI 用例与旧持久夹具修订，**均未复测**。需要退出后保存字节的旧测试移至 root/home 持久目录；未知源文件需要写入的夹具改为显式受管创建。原字节/身份/权限断言保留，原两秒取消/退出阈值未改。
- 最后试图运行严格 Clippy、fd_cli、script_language、ownership_cli 矩阵时，执行权限明确拒绝运行新 `serde = 1.0.228`、`serde_json = 1.0.149` 及其依赖代码。没有改权限配置、换工具绕过，也没有把旧输出当新验收。
- 2026-11-09 权限修订：用户回复“执行”后自动审批仍拒绝；用户经 /permissions 添加精确 Bash(cargo clippy ...) 白名单后成功启动。首轮严格检查 exit 101，定位 precedence、needless_borrow、测试 field_reassign_with_default 三项，等价修复后复验 **11.91s、exit 0**，无新增 allow、无降低检查级别。编辑脚本曾因重复匹配及 CRLF 精确匹配不成立而中止，未把脚本中止当作 Rust 测试失败。
- 同轮 cargo test 完整串行命令仍在启动前被拒绝：新增白名单只覆盖 clippy，**没有新测试结果**。
- **当时待权限放行后的必跑项（下节已执行矩阵，最终收口中）**：当前全量串行 Cargo、默认构建；真实权限/umask/组/ACL/进程终止恢复/后台 FD 清理；摘祖先搜索、ACL mask、活任务锁门的必要性对照；最后复核引用与当前 src/tests 摘要。未做 commit/push。

### 未验收源码指纹

该阶段 src/tests 共 100 个 .rs，SHA-256：a6bc1ef96cb98f5a5dcece1fb51d8648aa8d3ed51fcf52a9367914562e1ea7f1。最新宿主边界源码见下节。按前文算法读取当前原始字节；**该快照通过编译期严格检查，但不是运行测试通过证明**。本地引用审计另外执行，不替代运行验收。

<a id="host-impact-boundary-2026-11-09"></a>
## 宿主影响硬边界（2026-11-09）

来源：用户要求按“功能对宿主的实际影响”完整表格落实，并确认继续。粘贴中被截断的 VFS/高级能力/网络行按原完整表恢复。本节是边界契约的单一权威；不是将尚未实现的功能勾选完成。

### 实际入口守卫

- [统一根路径门](../src/filesystem/host_boundary.rs)由 Config 解析、DataRoot 打开和初始化共用：拒绝相对路径、卷根、设备/UNC 命名空间、点组件、设备名/ADS/尾点尾空格和歧义字符。保守拒绝 Windows 常规系统/程序目录树及 SystemRoot；Unix 拒绝设备/系统目录树。初始化在创建前另外验证已解析父目录得到的根路径，不因盘符/长路径别名跳过门。
- [初始化](../src/initialization.rs)只允许已存在的父目录；通过该父目录能力创建明确指定的数据根本身，不自动铺根外祖先。标记检查、布局预检查、目录创建和恢复使用固定根能力，不再依靠已检查的宿主绝对路径执行后续写入。标记读限于 magic 长度加一个字节。
- prepare_open 返回 DataRoot 与 InitState；[启动入口](../src/main.rs)直接沿用返回的能力，不在初始化后重新按路径授权。旧 prepare 保留兼容返回值。该策略不改变现有数据根，不认领非空未知目录，不清理失败后未知残留。
- 新建宿主文件仍受宿主默认权限/继承影响；虚拟 uid/gid/mode/ACL 不额外映射成宿主账号、Windows DACL 或 Unix chmod。目录setgid仅改变虚拟出生GID与子目录位；目录sticky仅约束虚拟命名，setuid和普通文件setgid仍拒绝，不能解释成宿主提权。

### 功能与允许的宿主影响

| 功能 | 允许的实际影响与禁止边界 |
|---|---|
| 权限、ACL、特殊位 | 只更新虚拟权限记录；不切换宿主账号、不修改宿主安全策略。 |
| 虚拟链接、inode | 虚拟对象/名称关系；解析必须留在授予的根/后端能力内，不跟随宿主链接逃逸。 |
| 文件锁、rename、unlink | 会占用、移动或删除根内真实文件；不锁宿主整卷，不使用复制删除回退掩盖失败。 |
| 临时/缓存治理 | 只删除账本中已证明归属和删除条件成立的对象；未知、替换、损坏、额外数据保留，不全扫宿主或虚拟 tmp。 |
| 内存 FS、自有缓存 | 消耗进程内存，写回产生根内 IO；未来需独立容量/写回上限，不能冒充整机资源隔离。 |
| 自有格式、分配、日志/COW、修复 | 只接受根内、已授权的普通镜像文件能力；格式化/修复针对镜像内部，不接受 PhysicalDrive、块设备、卷句柄或原始宿主路径作为替代入口。 |
| VFS 挂载/卸载 | 内部后端注册、虚拟挂载点及繁忙检测；不调用宿主卷挂载/卸载，不映射任意宿主目录。 |
| 快照、压缩、加密、去重 | 处理本系统授权的数据/镜像，产生有界协议开销；不自动处理、加密或压缩整个宿主盘。 |
| 网络文件系统 | 需明确端点、访问范围与独立授权；不自动建立连接或接管宿主网络盘。 |

表中尚未实现的镜像/VFS/高级/网络后端，以及一般锁/虚拟链接/内存 FS/全局配额，仍按开头账本推进，不在本增量提供伪格式化或挂载命令。现有 IO、句柄、目录、catalog、事务和日志限额是局部上限，**不等于总磁盘配额、总内存限额或整机不受影响保证**。宿主资源耗尽、其他进程直接修改数据和首次能力授权前的敌对路径竞态不由本增量完全消除。

### 证据、反例与状态

- [新增根/宿主权限/精确回收测试](../tests/filesystem/host_boundary.rs)：纯路径校验不实际打开设备；虚拟 chmod/ACL 后 std::fs::Permissions 不变（Windows 仅验证只读属性，不声称完成 DACL 审计）；未知 tmp 与根外兄弟哨兵保留。
- 初始化新增缺父目录不落盘、点组件根提前拒绝、沿用能力、有界坏标记拒绝，以及父目录已检查后、子目录实际创建前的替换对照。普通宿主路径能读取根外哨兵；能力写入必须拒绝且不得在外部创建 bin/lib；摘掉能力 IO 改用 ambient create 的正向对照在测试中要求合成外部夹具出现 bin；此对照已在后续217/217库矩阵中执行通过（213.43s）；不是只编译结论。所有模拟目标只在 target/test-data。
- [真实进程测试](../tests/host_boundary_cli.rs)：错误根配置不得创建数据根；实际命令/任务退出保留兄弟哨兵、只读配置和未知 tmp。后续真实CLI2/2通过（3.37s）；此前首次缺密码提示空行的夹具失败及修复见default ACL记录。
- 起步编辑出现反斜杠字符字面值错误（编译 E0762）和一次 Python 字符串解码失败；后者未执行写入。改为确定字符编码，移除旧根检查留下的 unused import，未加 lint allow。中间严格检查 12.80s 与 4.57s 通过只属于当时快照。调整替换注入时点，避免父目录后置链接检查让不安全的实现也通过判据；当时运行对照尚待测试权限；后续库矩阵已通过。
- 该阶段全目标 cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings **6.39s、exit 0**；新增 Windows 库测试 9 项、真实 CLI 2 项只通过编译检查，**执行测试和默认构建仍需 Bash test/build 权限，未宣称通过**。
- 该阶段 src/tests 集合 103 个 .rs，SHA-256：f77f7239e2e165cc17b99ac393901f2e1473535382b3e9c49d7ea919a69f4393。按前文相同路径+NUL+原始字节算法，仅定位编译检查快照；没有提交/推送，没有改实际 .env、权限配置、真实数据根或宿主系统配置。
- 最终静态复核：五份入口/原则文档 120 个本地文件与锚点引用全部可解析；git diff --check 无格式错误（仅 LF/CRLF 提示）；编译后 src/tests 指纹未变。静态复核不替代运行验收。


<a id="default-acl-2026-11-09"></a>
## 第一层增量：default ACL 继承（106文件快照378项已验收）

来源：用户确认按九层顺序开发；本增量只完成第一层中的 default ACL 接入，**不把第一层或九层全部标为完成**。宿主影响硬边界保持不变；没有操作实际数据根、权限设置、设备、分区、挂载或网络。

### 官方依据与语义

- 2026-11-09 核对 [acl(5)](https://man7.org/linux/man-pages/man5/acl.5.html) 的 object creation/access check/validity，以及 [setfacl(1)](https://man7.org/linux/man-pages/man1/setfacl.1.html) 的 default entries、显式 mask 与自动 mask 规则。只实现下列字面子集，不声称整个 Linux 工具或 OS 权限模型等价。
- 父目录有 default ACL：复制成新对象 access ACL，owner/other 受 requested mode 对应位限制；有 mask 则限制 mask，无 mask 则限制 owning-group。named entries 保留原值，由 mask 限制有效权限。此分支忽略 umask；普通文件 requested `0666` 不会因此获得执行位。
- 无父 default ACL：会话创建分支继续扣 umask；显式持久 API 的 Exact mode 保留既有契约，不额外扣一次 umask。内部 CreationMode/CreationAccess 分别明确 Requested/Exact 与 Inherited/Umask/Explicit，不能把已扣 mask 的 mode 当作继承输入。
- 新目录另外原样继承父 default ACL，普通文件只有 inherited access ACL。修改/删除父模板只影响未来出生对象，不改变父目录当前 access mode/ACL，也不修改既有子对象或撤销已有 FD。
- default ACL 含独立 owner/other 与完整 owning-group；named entries 要有 mask。模板只属于已验证的受管目录 ID；owner/root 可修改，文件对象拒绝。父模板在命名锁内经实际目录标记身份获取，不按路径拼写猜测 Windows Unicode 别名。

### 字面命令

```text
setfacl -d u::7,g::0,o::0,u:1001:7,m::4 /tmp/shared
getfacl /tmp/shared
setfacl -k /tmp/shared
```

`setfacl -d ENTRY[,ENTRY...] PATH` 创建/修改模板；未提供的基本条目从当前 access 基本条目补齐，未显式给 mask 时按 owning-group/named user/named group 并集重算。`getfacl` 输出 default:u::、default:g::、default:o::、named entries 和可选 default:m::。`-k` 只删除该目录模板。Elara 的 `-b` 只去掉 access 扩展条目并保留实际 owning-group 权限，不将显示 mode 中的 mask 当作 group 原条目，也不删除 default ACL；这里明示本项目子集契约，不外推为整套 setfacl 参数兼容。

### 持久与恢复协议

- [模型](../src/filesystem/access_model.rs)：default_acl 以稳定 DirectoryId 为键；空 map 不序列化，旧 V1 字节严格往返。非空 map 使用 access catalog V2；清空后使用规范 V1。不接受错误版本标签、非规范字段、未知目录 ID、坏权限或缺失必要 mask。
- [目录事务](../src/filesystem/access.rs)：Create 同时登记 access ACL/default ACL；Rename 保持 ID/template；Remove 同时退休模板。意图校验禁止 Create/Remove/Rename 偷改其他目录的模板，Policy 仍受 owner/root 与类别门控制。
- [文件出生事务](../src/filesystem/ownership.rs)：V1 旧意图格式保留；有继承时用 OWNERSHIP_INTENT_V2 保存完整 old/new 权限 catalog，只允许新增当前出生 ObjectId 的 ACL，其他组/目录/任务字段必须保持原样，mask/group 与新 mode 必须一致。
- 顺序：暂存已登记 payload → 持久出生意图 → 安装私有 payload → 提交完整 inherited ACL → 公开名称 → 文件 catalog → 验证绑定 → 退休意图。权限提交还使用原 access.pending 协议；启动/命名锁先恢复 access intent，再恢复 owner intent，旧/新状态幂等完成，第三状态拒绝。没有先公开再临时 setfacl 的窗口，不承诺宿主断电或恶意宿主修改保护。
- 成本单独计：每个继承文件多占一条 file_acl（即使模板仅基本条目），多一次权限事务及 old/new catalog 拷贝/编码；每份权限 catalog 400 KiB，联合意图总上限 1 MiB，file_acl 4096。容量与规范性在暂存 payload 前预检；没有把它写成常量成本，也没有做极端规模性能验收。

### 验收状态与修订

- [15 项库测试](../tests/filesystem/default_acl.rs)：全部 requested mode 扫描、mask 限制对照、V1/V2 往返/拒绝、父/子/孙继承、清空后的 umask、owner/category 门、零权限创建 FD、移除 ACL 记录的必要性对照、目录 ID 退休、文件及嵌套策略故障恢复、任务条件保留、容量拒绝、联合意图篡改与 Windows Unicode 父别名。
- [2 项真实 CLI 测试](../tests/default_acl_cli.rs)：实际 binary 的 -d/getfacl/-k/-b、mask 重算与坏位拒绝、普通文件 mode、子目录模板，以及配置/根外兄弟哨兵/未知 tmp 字节保留。测试目标仅 target/test-data。
- 初始编译快照全目标严格检查 `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings` **2.72s、exit 0**。2026-11-09 用户通过 /permissions 加入 Bash(cargo test *)，并授权缺权限时自行补充本项目必要规则；读取配置发现已有 Bash(cargo *)，没有修改权限配置。当时全量串行测试启动后尚无完成结果；后续执行、失败与修复按下列时间顺序记录。
- 默认目录 `cargo build --offline` 曾完成：**54.08s、exit 0**，对应修复 marker 设置门之前的源码；当前13.83s默认构建已覆盖修复后源码；54.08s仅保留为历史，不替代测试。
- 获授权后的全量首次执行：库 **215 passed / 1 failed，251.00s**（编译49.20s）。默认 ACL 原14项中13项通过；失败判据误把 /etc 的 Configuration 类别当作 SystemReadOnly。此前未知 owner store、临时退出判据和 rm/rmdir 的三项失败本次均通过。Cargo 因库失败退出，CLI/脚本集成测试未运行。
- 复核真实入口后新增 marker 身份必要性测试：未补门时 **0 passed / 1 failed，1.41s**（编译5.61s）；复制相同 magic 的独立 marker 被错误接受，实际返回 Ok(CommitReceipt { bytes: 394, ... })。这是实现缺门，不是放松断言能解决。
- 已补 default ACL setter 的目录绑定复核，并以共享目录记录查找区分物理非目录 NotDirectory 与未登记目录 LegacyOwnershipUnknown；/etc 仍拒绝且不认领，另外保留 /usr 的 ReadOnlySystemPath 断言。新增未知 tmp 目录不落盘断言，marker 修改/删除两条路径均要求失败且 catalog 字节不变。default ACL 专项复验 **15 passed / 0 failed，84.31s**（编译5.37s）；先前错误接受 marker 的反例现在通过，未改拒绝门或阈值。
- 首次全体集成目标：**159项，151 passed / 8 failed**，5个目标失败。FD真实测试5/5、日志1/1、所有权1/1、ConPTY1/1均通过；脚本模块142/145通过。两个default CLI及一个宿主CLI测试缺少首次密码提示的空行；另外filesystem_cli、script_cli及脚本三项旧判据要求任务结束后tmp新对象仍存在。已先读 shell密码门、Boundary::Program.finish_task及shell结束门：密码行问题在夹具补显式跳过空行；持久字节判据转root；仍测试tmp的提示符用例在任务内逐字比对<hello>，结束后断言名称消失；EXIT trap同时证明root持久标记与tmp回收。原字节、拒绝、流式/回退、状态断言与两秒时限不变。
- 集成编译出现一次 incremental session 目录 finalize 的 os error 5 警告，后续命令还回显同一缓存诊断；未清理缓存、修改宿主ACL或关闭incremental。解析器源码正常重编译后，当前CLI、纯解析测试、Clippy与默认build输出不再有该警告；它不是测试失败的原因。
- 七处夹具修复后的全量：**376项，375 passed / 1 failed**；库217/217（213.43s）、脚本145/145（179.99s）、FD5/5、宿主边界2/2、filesystem/logging/ownership/script CLI各1/1、ConPTY1/1通过。剩余default CLI一项因坏位8被共用mode解析器先拒绝，文本不是测试所需的ACL专属提示。未改判据为更宽匹配，而在共享entry解析门检查八进制字节和0..7范围，统一拒绝错误，并增加正负纯解析用例。
- 修复后 [2项真实default CLI](../tests/default_acl_cli.rs) **2/2通过，35.03s**（编译13.79s）；[共享ACL解析器2项](../tests/script/access.rs) **2/2通过，0.00s**（含等待构建锁的编译阶段41.44s）。此计数不拼接成新全量通过数。
- 当前严格 `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings` **7.25s、exit 0**；当前默认目录 `cargo build --offline` **13.83s、exit 0**。当前106个Rust文件的快照SHA-256为 `b1312599066251538a0091fda8c20a27223c0ad7b6244e552eae1ddaa4b399d3`；这一快照的完整no-fail-fast串行复验已完成，详见下列最终矩阵。default ACL增量收口，不代表第一层或整体文件系统完成。



- 首轮模型编辑漏掉 V2 magic 声明，由逐字节复核发现并补齐；14.74s/4.08s 是中间编译快照，不替代最终结果。当时修订上节对ambient正向对照的实测措辞为已写未运行；后续217/217库矩阵已实际执行该判据，当前状态已就地更新。
- 初始编译快照 src/tests：105 个 .rs，SHA-256 `ff4a9bac57b70039707d953030872e40aa84f0db5be387e88857cce3351d07f8`；算法仍为 src/tests 内路径排序、相对路径 UTF-8 + NUL + 原始字节。静态引用审计不替代测试；未 commit/push。

前一编译快照静态复核：五份入口/原则文档 130 个本地文件/锚点引用可解析，Unresolved=[]；git diff --check 无格式错误，仅 LF/CRLF 提示；最终编译后 src/tests 指纹未变。静态复核不替代运行验收。

### 2026-11-09 default ACL快照最终运行矩阵

命令：`cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1`，编译1m54s，**378 passed / 0 failed / 0 ignored，exit 0**。这是一次完整执行，不是拼接专项或历史计数。

| 目标 | passed / failed | 实际运行耗时 |
|---|---|---|
| lib（包括default ACL15项、共享ACL解析2项、宿主/初始化必要性对照） | 219 / 0 | 471.56s |
| default_acl_cli | 2 / 0 | 8.89s |
| fd_cli | 5 / 0 | 34.76s |
| filesystem_cli | 1 / 0 | 3.89s |
| host_boundary_cli | 2 / 0 | 4.25s |
| logging_cli | 1 / 0 | 3.96s |
| ownership_cli | 1 / 0 | 7.93s |
| script_cli | 1 / 0 | 8.53s |
| script_language | 145 / 0 | 176.63s |
| terminal_pty（真实ConPTY） | 1 / 0 | 1.71s |

main及doc-tests均0项。当前输出无Clippy/incremental警告；输出文件存在非UTF-8诊断字节，读取时保留十六进制转义，结果行直接按原始ASCII字节计数并复核hex，未丢诊断或借最终进程状态代替中间断言。

验收后仍为106个Rust文件，SHA-256 `b1312599066251538a0091fda8c20a27223c0ad7b6244e552eae1ddaa4b399d3`。严格Clippy7.25s与默认build13.83s对应同一源码。耗时仅是本机这一执行批次，不当作极端性能或回收延迟保证。初次marker缺门、密码提示、临时生命周期夹具、ACL错误上下文等失败保留为修订记录；原两秒判据未改。

本次没有修改实际数据根/宿主权限/系统设置，没有提交或推送。下一增量仍按下表推进，不外推Linux原生、真实断电、一般配额、虚拟执行身份或自有磁盘已验收。

<a id="directory-setgid-2026-11-09"></a>
## 第一层下一增量：目录setgid（108文件快照390项已验收）

本增量沿用前一106文件/378项验收作为历史基线，**新增源码必须独立复验，不把378标成新增setgid通过**。

- 官方依据（2026-11-09读取）：[mkdir(2)](https://man7.org/linux/man-pages/man2/mkdir.2.html)规定父目录SGID时继承GID且新目录继承SGID；[chmod(2)](https://man7.org/linux/man-pages/man2/chmod.2.html)规定非特权调用者不属于目标组时清除请求的SGID。default ACL仍只决定访问权限，不决定GID。
- [模型](../src/filesystem/access_model.rs)新增`CreationGroup::Primary / SetgidDirectory`，组来源与default ACL/umask分支分开；[目录创建](../src/filesystem/access.rs)在命名锁内验证父marker身份，新目录取得父GID和`02000`，文件仅继承GID、不获得SGID/执行身份。清除父SGID只影响未来对象，原组与已出生对象不变，不把继承GID登记成调用者的组成员身份。
- 显式chmod仍要owner/root；非root不属于最终GID时清除请求的SGID。access ACL/mask调整保留已有目录SGID；Windows Unicode目录别名按已证明的目录ID处理，不借大小写差异绕owner门。
- [模式/文件catalog](../src/filesystem/ownership_model.rs)只额外接受目录`02000`；普通文件创建、修改及catalog解码均拒绝特殊位，以`DirectoryModeOnFile`显式报错，不接受“已保存但无执行语义”的setuid/file SGID。sticky`01000`、setuid`04000`和额外坏位仍拒绝；umask仍仅`000..777`。
- 目录有SGID时access catalog使用V3；没有特殊目录时仍按default ACL存在与否输出原V2/V1字节。旧V1/V2头不可携带SGID冒充兼容；普通文件catalog继续原V1/V2。出生GID/目录SGID直接包含在既有出生意图中，恢复使用保存值，不读取后来修改的父策略重算。
- 多一次父marker身份复核，继续复用锁内有界catalog；没有变成常量成本或全局配额。所有实际/故障/越界夹具只在target/test-data，不改宿主GID、ACL或chmod。
- 测试入口：[目录专项](../tests/filesystem/special_permissions.rs)含组与默认ACL独立继承、摘父SGID对照、owner/成员门、文件位拒绝不落盘、V3及V1/V2往返、ACL位保留、出生恢复、任务条件与未知数据、Windows Unicode别名；[真实CLI](../tests/setgid_cli.rs)直接运行chmod/chgrp/-d/-b/umask/创建/清位并核验模式/GID/字节及外部哨兵。首轮全目标严格Clippy **12.22s、exit 0**；当时新增CLI后的最终矩阵尚未完成，后续完整390项结果见下方。

### 已执行与修订（新源码尚未全量验收）

- 新目录专项首轮 **3 passed / 8 failed，64.02s**（编译2m13s）。8项都先卡在共享夹具设置主组：`primary_group_as`要求目标已登记为该组成员，即使调用者为root也不跳过。读取真实门后只在夹具先加入主组6000，再设primary；没有加入继承组5000，也没有修改生产成员门或降断言。修复后专项**11 passed / 0 failed / 0 ignored，328.37s、exit 0**（编译49.08s），包括2项原owner门保留断言；不是11项sticky功能已完成。
- 真实`setgid_cli` **1 passed / 0 failed，18.38s**（编译/等待构建锁2m09s）。直接验证GID5000、目录2770、清父位后新文件GID0、拒绝文件特殊位、模板仍生效、root持久字节、配置/未知tmp/根外哨兵不变。不仅检查最终进程退出码。

- 最终新增CLI后全目标严格命令 `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings` **5.00s、exit 0**；默认 `cargo build --offline` **47.42s、exit 0**。这两项不替代运行矩阵。
- 当前src/tests **108个Rust文件**，SHA-256 `5d700cbc9c0c7b632c47e9a620d2679a98e6763bee947ed9ba634690a694d1c9`（相对路径UTF-8 + NUL + 原始字节，src后tests、分别排序；hex已复核）。专项修复后复验已通过；这一108文件快照的完整 `cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1` 已完成但失败，见下列修订；不把前一106文件378项结果套用到新增源码。
- 新增引用后五份入口/原则文档 **140个本地文件/锚点引用，Unresolved=[]**；`git diff --check`无格式错误，仅已有LF/CRLF提示。静态结果不冒充Linux原生、真实断电、完整inode/链接、全部Unicode命名语义或极端性能验收。

### 新增全量第一次结果与修复（历史失败，后续已复验通过）

完整批次 **390项，388 passed / 2 failed / 0 ignored，exit 101**（编译1m18s），源码为上述108文件`5d700...`快照。库229/230（1065.47s），FD CLI4/5（168.56s）；default CLI2/2（20.29s）、filesystem CLI1/1（18.81s）、host CLI2/2（15.88s）、logging1/1（20.67s）、ownership1/1（27.75s）、script CLI1/1（40.49s）、script_language145/145（254.06s）、setgid CLI1/1（5.87s）、ConPTY1/1（1.67s）全部通过。main/doc-tests0项；结果从完整原始输出的ASCII行计数并复核hex。

1. 宿主模式旧测试仍把2000和1000/4000一起要求构造拒绝。目录SGID现已实现，因此改为保留1000/4000拒绝，并新增普通文件SGID显式`DirectoryModeOnFile`拒绝、字节与宿主权限不变、目录2700虚拟生效且宿主目录权限不变。没有放宽宿主安全门；旧“所有特殊位都未实现”判据已被实际新范围推翻。
2. `real_job_completion_cancellation_and_fd_cleanup_leave_parent_grants_live`在原`before.elapsed() < 2s`处失败。未修改该测试或时限，未修改取消引擎。随后独立执行同一测试 **1/1通过，5.78s**（编译0.38s），实际输出包含`cancelled 130`、临时名回收与父FD仍可读。**独立未复现不证明根因已解决**；全量时长明显变大也不单独证明环境负载是原因，下一完整批次仍必须通过原两秒门槛。

宿主判据修订后仍108个Rust文件，SHA-256 `5710375cf1152203f61cbb948e83d6f9426f72479c3aac17320a3e752b8a90fc`；FD测试源码及生产源码均未因这两项失败修改。宿主专项 **1/1通过，11.34s**（编译1m57s）；修订后全目标严格Clippy **55.23s、exit 0**（含等待构建目录锁）。已在上述专项/构建任务全部结束后，重新启动完整串行矩阵，不额外并行启动自己的编译或专项任务；仍使用原两秒判据；随后完成结果见下方最终矩阵。

### 2026-11-09 目录setgid最终完整矩阵

`cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1`：**390 passed / 0 failed / 0 ignored，exit 0**，编译13.94s。一次完整执行，不将专项与旧378项拼接。

| 目标 | passed / failed | 实际运行耗时 |
|---|---|---|
| lib（含目录SGID11项及修订后的宿主模式测试） | 230 / 0 | 944.42s |
| default_acl_cli | 2 / 0 | 16.49s |
| fd_cli（原两秒取消断言未改） | 5 / 0 | 43.93s |
| filesystem_cli | 1 / 0 | 4.95s |
| host_boundary_cli | 2 / 0 | 3.33s |
| logging_cli | 1 / 0 | 6.15s |
| ownership_cli | 1 / 0 | 6.03s |
| script_cli | 1 / 0 | 7.17s |
| script_language | 145 / 0 | 175.82s |
| setgid_cli | 1 / 0 | 6.89s |
| terminal_pty | 1 / 0 | 1.55s |

main/doc-tests0项。原始输出结果行按ASCII字节及hex核对，输出无编译/Clippy警告。验收后仍108个Rust文件，SHA-256 `5710375cf1152203f61cbb948e83d6f9426f72479c3aac17320a3e752b8a90fc`，与启动前相同。当前全目标严格Clippy55.23s（含等待构建目录锁）、exit 0；默认build47.42s对应未变生产源码（后来只改宿主测试体），不外推测试编译为默认程序构建。

该批次完整通过原两秒取消判据；此前超时原因仍未确认，不用本批次通过宣称极端负载下性能保证或根因已修复。此前388/390及独立复验保留在上方。目录SGID增量验收收口；不是sticky、setuid、file SGID执行身份、完整inode/链接或整个九层已完成。没有操作实际数据根/宿主权限或系统设置，没有提交/推送。

### sticky命名权限阻塞（历史；当前状态见文末）

- [unlink(2)](https://man7.org/linux/man-pages/man2/unlink.2.html)要求父目录w+x；sticky目录还要求目标owner、目录owner或特权调用者。当前Elara对改名/删除另有**目标owner/root硬门**，即使父目录共享可写，也不允许第三人改名/删除受管子项。未知归属、绑定、类别门另外独立保留。
- 先在当前实现运行未来POSIX非sticky命名反例：**0 passed / 2 failed，2.93s**（编译7.64s）；文件和空目录改名都返回`OwnershipDenied`。这是当前更严格契约与未来父目录授权语义的差异，不是把已实现sticky测成了bug。
- 替换这条硬门的生产编辑被auto审批以`Security Weaken`拒绝。**没有换工具、改权限配置或包装命令重试移除此门**。继续实现了不依赖该变更的目录SGID；将两项未来语义反例明确改判为“尚未实现/审批阻塞”，现有测试验证旧owner门仍成立，不能拿它们绿了宣称sticky完成。
- sticky仍不接受；要落实其真实命名语义，需要明确批准受管非sticky目录由父w+x授权改名/删除，以及sticky目录的owner限制。未登记父目录保留旧owner/root门，未知对象仍拒绝；不改变宿主权限。未获该批准前不进入依赖这一步的后续层。

### 已批准的后续顺序（当前状态，范围不缩减）

修订：此前本表保留了增量前的“待实现”描述，已就地同步；各项仍以实际后端与测试边界区分，不把专项计数当完成率。

| 层 | 按用户指定的工作 | 当前状态 |
|---|---|---|
| 1 | ACL/特权身份/inode/硬链接/符号链接 | 权限、执行身份和清位已验收；内部卷 inode/链接已接；Host受管普通文件已拆inode/目录项并接硬链接，27项专项及真实双进程通过，迁移后573项全量通过；文件/目录已迁移共用InodeTable，35项专项通过；Host符号链接V7、持锁解析及目录观察已通过15项专项、默认实跑和586项完整回归；跨Host/内部卷链接仍拒绝 |
| 2 | 一般文件锁、共享打开与跨后端共享/删除 | 内部卷已接锁/共享/旧 FD 存活；受管Host锁和独占共享已通过560项冻结全量与默认实跑；Host文件/目录覆盖rename已通过37项相关测试、真实双进程、严格Clippy及默认build；596项全量通过；跨卷拒绝 |
| 3 | VFS 注册、解析、内部挂载/卸载、繁忙 | 基础已接；cwd、嵌套、全局枚举已验收；显式symlink-global跨内部挂载已验收 |
| 4 | 内存 FS、虚拟 proc/sys/dev | 内存卷和显式动态 provider 已接；默认系统路径、资源租约与scoped作业目录已通过专项及实际运行；legacy TaskTable计时观察已接；前台解释器执行目录已接，扩展验收中 |
| 5 | 配额、缓存登记回收、应用登记、运行态 | 卷/UID 配额及任务/运行态租约已验收；应用/缓存登记、配额、关联数据位置与整组回收已通过专项；完整应用身份沙箱和压力驱逐未接 |
| 6 | 自有缓存、写回和统计 | ELARABK1显式镜像IO有界LRU、脏淘汰/写回及统计已通过专项；透明挂载缓存未接 |
| 7 | 镜像格式、分配/释放/耗尽/碎片 | ELARAFS1 checkpoint及ELARABK1固定槽/双根/稳定inode页键已接；耗尽、碎片和两代保护复用已通过专项 |
| 8 | 自有日志/持久 COW、提交顺序、孤儿、校验/修复 | 显式disk-save持久COW、提交断点/损坏只读回退、打开孤儿排除与显式repair已通过专项；无透明逐次写入持久化或真实断电证据 |
| 9 | snapshot/clone/compress/encrypt/dedup/network/overlay | 内存共享页快照/克隆、镜像压缩/去重、认证密封、运行时页去重、冻结overlay已接；独立命名镜像保留冻结属性已通过专项；共享块快照管理/live overlay/网络FS未实现；外部网络仍需独立授权 |

<a id="sticky-2026-11-09"></a>
## 第一层当前增量：sticky 与统一命名权限（408项已验收）

来源：用户允许直接修改、按顺序开发并保留既有改动。390/390仍是上一目录SGID快照，不套用到新增源码；后续九层范围不缩减。

- 模式支持受管目录01000及02000组合；普通文件仍仅000..777，setuid/file SGID执行身份未实现。sticky不从父目录继承；目录SGID与default ACL继承保持独立。
- `name_removal_locked`返回`NameAuthority`，区分受管父目录、sticky目标owner、sticky目录owner、虚拟root及未登记父目录的旧目标owner分支。文件/目录rename/remove共享此门：父目录需要w+x；有sticky再要求目标owner、目录owner或虚拟root。数据读写、chmod、ACL的授权不因此改变。
- 未登记父目录保留目标owner/root门；未知目标、标记/对象绑定不确定、系统类别和home范围上界仍拒绝。rename只向新名称发布，不覆盖已有目标，不跨类或复制删除回退。若将来支持覆盖，必须另验被替换目标，本轮未开放覆盖。
- `access.catalog`有sticky时用规范V4，无sticky继续按SGID/default ACL选择原V3/V2/V1；所有版本逐字节往返，旧头部夹带新位拒绝。不扫描或迁移未知对象，不改变宿主chmod/DACL/账号。
- [命名分派](../src/filesystem/namespace_access.rs)把mv的选择后端收回文件系统：枚举区分受管文件、受管目录、旧对象。分派不是能力，最终操作重新取锁并验证当前归属、权限及绑定；旧对象分支仍受裸rename的受管名称/祖先/后代拒绝门限制。`cat/cp/stat`保留读权限。
- 新测试：[sticky库不变量](../tests/filesystem/sticky.rs)、[真实三账号CLI](../tests/sticky_cli.rs)。既有两项等待sticky的owner门测试改为实际sticky目录，保留拒绝及字节断言；模式测试继续拒绝setuid，新增普通文件sticky拒绝，不把已支持目录位当作坏位。
- 修改前非sticky共享目录反例2项全部失败，均OwnershipDenied；编译9.18s，运行2.96s。初轮专项14项为13/14，49.11s（编译15.94s）；失败在Unicode文件catalog精确拼写门、尚未进入sticky。分开断言别名LegacyOwnershipUnknown、已登记名称OwnershipDenied及字节不变，不扩大别名权限。
- 复验库16/16通过，46.37s（编译34.87s）；其中新增14项、既有sticky夹具2项。真实CLI0/1，8.41s：非sticky的mv被桥层ownership_as的目标读权限提前挡住。已改到文件系统命名分派，CLI原标记/拒绝/字节判据未改，复验中。
- 全目标严格Clippy已通过（18.27s、exit 0）；当前完整串行矩阵执行中，默认构建、引用审计和最终指纹尚未完成。未提交/推送；尚不宣称sticky或第一层整体完成。

- 2026-11-10 命名分派修复后复验：`cargo test --offline --lib --test sticky_cli --target-dir target/fs-build sticky -- --test-threads=1 --nocapture`，库18/18（新增16项、既有2项），70.50s；真实三账号CLI1/1，10.27s；编译15.09s，exit 0。原CLI标记、数据读拒绝、改名/删除、宿主权限及未知/根外字节断言未变。新增分派测试同时验证旧FD在第三方合法rename/unlink后仍读取原字节，以及替换绑定、root系统类别门不被旧分支绕过。以上只是专项，不拼接成当前全量通过。

### 后续执行身份的接入前置审计（未实现）

2026-11-14用户要求继续，并强调不得损坏本机系统。本轮边界保持为项目源码与target内新测试根，不启动实际.env数据根，不操作宿主权限/服务/注册表/磁盘/挂载。下一可运行增量先实现受管执行映像加载及虚拟`exec FILE [ARGS]`：在同一命名锁下验证路径上界、祖先x、对象绑定、文件x/ACL并读取有界不可变字节；不按名称二次打开，不调用宿主进程。普通sh/source仍是读取脚本，不借此提权；文件setuid/SGID仍拒绝。先验证执行入口、快照、预算/取消/FD/任务继承，再接入真正的真实/有效/保存身份与特殊位。这是第一层内部前置增量，不是缩减九层范围或宣称执行身份完成。

按既定顺序，sticky收口后的下一项是setuid/普通文件SGID虚拟执行身份，不是先开放模式位。以下为源码门的审计结果，不是已完成能力：

- [账号](../src/users.rs)只有name/uid/home；[凭据](../src/filesystem/access_model.rs)只有uid/gid/groups。文件与目录ACL、创建属组、chmod/chgrp目前从catalog按账号uid重取凭据；只在解释器替换uid或设置gid局部变量，不能让普通文件SGID真正生效。需要统一真实/有效/保存身份与补充组的传递，不修改宿主账号。
- [脚本读取](../src/script/mod.rs)目前是read权限授权，不是execute权限授权；`sh`/`source`复用解释器、共享预算与深度。执行映像必须将对象绑定、模式/ACL与读取到的字节放在同一授权快照里，不能先stat再按名称重开。普通读取脚本不能被悄悄改成提权入口。
- 写入通道清单：[路径append/replace](../src/filesystem/ownership.rs)、[可写打开/截断](../src/filesystem/owned_access.rs)、[旧FD的write/write_at/truncate](../src/filesystem/objects.rs)，以及这些入口上层的重定向/cp/fd命令。执行特殊位开放前必须统一约束“先打开、后chmod、再经旧FD修改”的情况；不能只修命令桥层或撤销既有FD的数据能力。storage_generation替换、同对象rename及unlink后旧FD应分别验证。
- [模块fork](../src/script/shared.rs)检查root/actor匹配，[任务租约](../src/filesystem/scope.rs)绑定uid，[作业](../src/script/jobs.rs)与FD表也绑定身份。新执行上下文必须显式交接这些能力，保留取消、资源预算与回收边界，不能复用不匹配的模块实例。普通文件特殊位仍拒绝，直到端到端实现与必要性对照通过。

### 取消副作用证据审计（已补强，408项完整复验通过）

复核[脚本作业测试](../tests/script/jobs.rs)和[真实FD生命周期测试](../tests/fd_cli.rs)时发现：若用`/tmp`下的TaskExit文件缺失来证明“取消后没有执行touch”，任务结束回收也会让错误执行过的文件消失。因此缺失本身不能区分“从未执行”和“执行后已回收”；这不等于已经发现取消引擎执行了禁止命令。原两秒就绪/取消时限、暂停ACK和status 130仍需保留。发现时先保持正在运行的407项批次源码不变；该批次通过后，已将禁止副作用见证与临时回收见证分开，并运行真实执行对照（结果见文末）。相同根因已定位到11份测试文件：`tests/script/`内的jobs、runtime、nested、case、expansions、declarations、interactive、globbing、fs_commands，以及`tests/fd_cli.rs`、`tests/script_cli.rs`。补强时仅迁移“禁止产生副作用”的见证：root使用自身持久目录，guest使用自己的home，不把guest见证迁到无权写入的/root。真正验证任务回收的`/tmp/job-ready`、`/tmp/job-end`、`/tmp/task-cleaned`等继续保留；已有status/steps/错误文本/字节/两秒时限断言不删改。对照应执行相同创建入口并确认持久见证确实留下，同时证明临时见证确实会被回收，不能仅靠改路径后变绿。

### 当前边界与代价

- 命名分派观测与最终rename是两次锁获取，不是一次原子观测。分派仅选择显式后端；最终后端按当时名称重新验证对象归属/绑定、父权限及类别。分类改变时不能使用旧分类越权；不把它写成“从观测到提交对象必不变”。未受管名称的旧分支仍可能在最终操作前返回受管上下文拒绝，不以复制删除降级。
- 每次受管命名权限检查读取有界access catalog、搜索祖先、验证父目录marker绑定，再检查w+x及sticky主体。mv分派增加一次取锁/观测，最终命名事务仍有既有catalog提交和同步代价。专项耗时不是单次IO基准；未做极端目录/ACL规模性能验收。
- 尚未实现完整inode及Unicode文件别名解析；其他Unicode拼写可能在文件catalog门先返回LegacyOwnershipUnknown。目录marker别名验证已覆盖，但不能外推完整Unicode命名等价。
- 尚未实现setuid及普通文件SGID执行身份、虚拟硬链接/符号链接、覆盖式rename、跨后端语义、Linux原生及真实断电。sticky不修改宿主文件权限，未知文件不会被新命名权限认领。

- 2026-11-10 全目标严格命令 `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings`，18.27s、exit 0，无新增allow。当前111个Rust文件，SHA-256 `39862f4673bf94dfad7d0a0ee2505b48318f3b913b83923cff027fe47b4c582a`；完整串行矩阵以该源码启动。五份入口/原则文档147个本地文件/锚点引用可解析，Unresolved=[]；`git diff --check`无格式错误，只有既有LF/CRLF提示。静态结果不替代正在运行的完整矩阵。

### 完整矩阵首轮与修订（2026-11-10）

- 完整`cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1`为407项：406 passed/1 failed/0 ignored，exit 101，编译42.98s。库245/246，320.91s；default CLI2/2（8.51s）、FD5/5（38.44s）、filesystem1/1（3.80s）、host2/2（3.01s）、logging1/1（4.86s）、ownership1/1（5.84s）、script CLI1/1（8.29s）、script_language145/145（176.55s）、setgid1/1（5.40s）、sticky1/1（7.14s）、ConPTY1/1（1.51s）全部通过；main/doc-tests0项不计总量。结果行直接按原始ASCII字节计数并复核hex。
- 唯一失败是旧`serde_json::from_str::<VirtualMode>("512").is_err()`：512为01000，已由本增量支持。已改为精确正向bits==01000；同一测试保留V1头夹带该位的拒绝与setuid2048拒绝。没有降低门或放宽错误匹配，生产源码未因此修改。该修订后的专项、严格检查和完整矩阵需重新执行；不把406/407写成全量通过。

- 修订后仍111个Rust文件，SHA-256 `b0557684bd369ccb3561f956f1f7f05e8a1b9422e1edff136d289d47c1656bb6`，只修改旧反序列化测试，生产源码未变。已启动一个串行验收批次：先精确复验该测试，再严格Clippy，再完整no-fail-fast串行矩阵，全部成功才执行默认`cargo build --offline`；任一步失败即停止后续步骤。未额外并行启动编译/专项，不修改原两秒门槛。该批次已结束：模式专项1/1（2.00s）、严格Clippy（3.69s）通过；完整矩阵406/407，唯一失败为脚本嵌套取消测试的两秒就绪名称前置断言，非取消ACK断言。lib246/246（318.91s）、script_language144/145（456.88s），其他CLI及ConPTY全部通过。完整测试exit 101，默认build未执行。

- 2026-11-10 精确复现嵌套取消测试：1项、0 passed/1 failed，5.12s（编译0.55s），同样在两秒内未观察到`/tmp/cancel-ready`；保留原时限。实际`touch`走运行时直接文件系统入口，先创建持久任务租约再进行受管文件事务，并非SharedModules回调。尚未证明是取消引擎、持久IO成本还是判据错；继续收集作业状态与报告证据，不归因宿主负载。

- 状态诊断首编译失败E0277：`Snapshot`/`Waited`没有Debug实现。诊断改为现有可Debug的id/state元组及解构后的Report；不修改生产类型或两秒阈值。

- 状态诊断修订后专项1/1通过，2.33s（编译3.65s），原两秒就绪门、暂停ACK、取消130及三个禁止副作用断言均未改。失败分支只增加snapshot和cancel/join后的Report；生产源码未修改。未再次触发失败分支，因此仍不能确定此前超时根因，不能把诊断称作生产修复。当前111个Rust文件SHA-256 `bcf3b0ed9eaf062c2167ef6edef85212d266dd52b97576753c9680598f2ae9d9`；以此快照重新严格Clippy、完整串行矩阵，全部成功才默认build，不并行编译。

### 407项快照完整通过（2026-11-14核对）

`cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings`：exit 0，5.85s。
`cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1`：**407 passed / 0 failed / 0 ignored，exit 0**，编译0.81s。各目标为lib246（536.36s）、default_acl_cli2（26.34s）、fd_cli5（85.99s）、filesystem_cli1（7.55s）、host_boundary_cli2（6.05s）、logging_cli1（8.39s）、ownership_cli1（12.77s）、script_cli1（12.92s）、script_language145（249.28s）、setgid_cli1（5.82s）、sticky_cli1（8.47s）、terminal_pty1（1.56s）；main/doc-tests0项。结果行按原始ASCII字节及hex核对，不以任务通知代替输出。
随后实际执行默认`cargo build --offline`：**exit 0，28.47s**。验收结束时仍111个Rust文件，SHA-256 `bcf3b0ed9eaf062c2167ef6edef85212d266dd52b97576753c9680598f2ae9d9`，与开始一致。原两秒门通过，但此前偶发就绪超时根因仍未确认。

该结果只属于上述407项快照。审计发现TaskExit会掩盖“未执行”的缺失见证，因此继续补强测试，不用407通过掩盖证据盲点。首先新增真实前台/后台执行对照：同一嵌套脚本实际执行EXIT trap、失败条件右支与后续命令；临时见证被回收，持久见证必须仍为受管OwnerRequest对象。生产源码未变。真实前台/后台副作用对照专项 **1 passed / 0 failed，12.51s**（编译12.09s）；实际执行的临时trap/后续文件和目录在退出后消失，而同次执行的持久trap、条件右支、后续文件与目录仍为OwnerRequest。由此确认旧缺失判据的证据盲点，而不是发现了取消引擎越过检查。

随后精确补强上述11份测试：禁止副作用见证改为root自己的持久目录，两个guest用自己的home，并新增guest同一目标可被合法touch/cp创建的正向对照。真正验证回收的/tmp见证保留；未删改原status、steps、错误、字节或两秒时限断言。当前111个Rust文件SHA-256 `365b0f3a62accd9462d86c7b6e9f18737d187a9bd5cf63053a530d44f6c4f5db`，测试总量预期408（新增1个前台/后台组合对照）。该快照随后完成严格Clippy、完整串行矩阵及默认build，结果如下。后续特殊执行身份仍未实现。

<a id="sticky-final-2026-11-14"></a>
### 2026-11-14 sticky与见证补强最终矩阵

| 命令 | 结果 |
|---|---|
| `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings` | exit 0，1.74s |
| `cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1` | **408 passed / 0 failed / 0 ignored，exit 0**；编译9.73s |
| `cargo build --offline` | exit 0，0.53s |

一次完整执行：lib246（372.46s）、default_acl_cli2（9.87s）、fd_cli5（44.04s）、filesystem_cli1（3.68s）、host_boundary_cli2（2.71s）、logging_cli1（4.11s）、ownership_cli1（8.09s）、script_cli1（8.14s）、script_language146（220.27s）、setgid_cli1（8.93s）、sticky_cli1（19.36s）、terminal_pty1（1.53s）；main/doc-tests0项。后台输出本身已读取，结果行按ASCII及hex复核，不拼接旧专项计数。

验收结束仍111个Rust文件，SHA-256 `365b0f3a62accd9462d86c7b6e9f18737d187a9bd5cf63053a530d44f6c4f5db`，与启动前一致。原两秒就绪/取消判据及暂停ACK通过；持久禁止副作用见证通过，前台/后台真实执行对照也通过。此前偶发就绪超时原因仍未确认，此次通过不等于性能根因已修复。

**本增量收口：sticky与统一命名权限，以及任务回收后仍有效的禁止副作用见证。第一层整体及其余八层未完成。** 下一项为setuid/普通文件SGID虚拟执行身份，再完整inode与虚拟链接。未修改宿主权限、真实数据根或系统配置；未提交/推送；没有还原或覆盖既有改动。修订记录：保留378/390/407历史快照和全部失败，将旧“sticky未实现”当前表述改为本最终结果，执行特殊位仍标未实现。

<a id="virtual-execution-2026-11-14"></a>
## 第一层下一增量：虚拟执行映像入口（421项已验收）

来源：用户要求继续按顺序，并强调不得损坏本机操作系统。仅项目源码及target内新测试根；不启动实际.env，不碰宿主账号/权限/注册表/服务/启动配置/真实磁盘/宿主挂载。

- [执行映像](../src/filesystem/execution.rs)：同一namespace锁下检查路径上界、祖先搜索、受管普通文件、mode/ACL的x和公私绑定，再从已核验私有payload读取有界字节。虚拟root也要求至少一个x位；未知对象不自动认领，无宿主执行回退。映像复制到内存，因此旧FD修改、替换、rename/unlink不能改变已加载字节；这不是宿主进程沙箱，也不对敌对宿主直接写盘提供完整隔离。
- `ExecutableImage`对外不公开内容字节，Debug只打印元数据/字节数；内容只供crate内部解释器消费。`authorized_as`是加载时凭据快照，不是未来IO的身份切换凭证；当前没有提权。
- [解释器入口](../src/script/mod.rs)：`exec FILE [ARGS]`仅接受受管UTF-8 Elara脚本，拒绝宿主路径/未知文件/坏编码/坏语法，预检失败保留调用环境。成功替换当前虚拟脚本边界，原EXIT trap丢弃，映像自己的trap生效；后续原脚本不执行。sh/source继续按read规则，不悄悄升级为特殊执行入口。实际边界共享预算、深度、取消控制、FD及任务租约；没有创建宿主命令子进程。
- 当前仍拒绝setuid/普通文件SGID位。完整真实/有效/保存身份、组传递及特殊位写入清除仍待后续接入，不将本入口视为特殊身份已实现。
- 代价：每次执行在全局命名锁内读取catalog、证明绑定并复制最多64KiB脚本，随后解析；这是有界内存快照，不是零复制/mmap，不是自有inode或跨后端exec。执行权限不授予普通cat读取内容。
- 首轮专项：库4/4（6.72s）、脚本筛选6/6（含1项既有测试，14.11s），编译39.35s。必要性对照仅在cfg(test)摘除文件x门；祖先搜索与绑定检查保持，分别继续拒绝。
- 随后新增输入/输出重定向用例真实失败：0/1，4.06s，编译5.16s。实际stdout为prefixsuffix、预期prefix，输入payload未进入映像：新exec分支没有经原command输入/输出边界。已按sh/source同一模式保存/安装输入、隔离/返回输出修复；未改判据。修复后专项库4/4（12.59s）、脚本筛选7/7（含1项既有测试，25.16s）、真实进程1/1（18.45s）通过，编译31.81s；严格Clippy21.65s、exit 0。上述结果来自真实输出，不外推完整矩阵。
- [库验收](../tests/filesystem/execution.rs)、[脚本验收](../tests/script/execution.rs)、[真实进程验收](../tests/execution_cli.rs)。CLI显式使用新fixture.env，核查根外哨兵、配置字节及宿主permissions不变；不使用真实配置。

- 追加仅x权限的guest实际执行/读拒绝/Debug不泄露内容/身份不变，以及exec后台真实FD副作用→暂停ACK→取消130→持久trap/条件右支/后续见证不产生的守卫。就绪仍为2s；新测试预先打开持久FD，在映像内写入ready证明能力真正继承，不靠临时文件退出缺失判定。
- 当前115个Rust文件SHA-256 `577722fb235e7f5b1a30277cd1e1ee1f2d1697217009e26bb34772d46d4decda`；新增13项，预期完整421项。串行批次先8项execution_tests，再严格Clippy，再完整no-fail-fast测试，全部成功才默认build；任一步失败停止后续，不并行运行编译。该批次已完成，最终结果见下节；不把上轮408通过套用新源码。`git diff --check`通过，只有LF/CRLF提示。

### 虚拟执行映像最终验收（2026-11-14）

- 精确脚本专项：8/8，27.83s（编译6.17s）。
- `cargo clippy --offline --all-targets --target-dir target/fs-build -- -D warnings`：exit 0，3.77s。
- `cargo test --offline --no-fail-fast --target-dir target/fs-build -- --test-threads=1`：**421 passed / 0 failed / 0 ignored，exit 0**，编译43.48s。一次完整执行，未拼接专项。
- 各目标：lib250（379.04s）、default_acl_cli2（9.26s）、execution_cli1（4.48s）、fd_cli5（45.86s）、filesystem_cli1（4.39s）、host_boundary_cli2（3.60s）、logging_cli1（7.85s）、ownership_cli1（7.45s）、script_cli1（10.05s）、script_language154（232.51s）、setgid_cli1（5.86s）、sticky_cli1（7.81s）、terminal_pty1（1.67s）；main/doc-tests0项。
- 随后默认`cargo build --offline`：exit 0，10.62s。原始结果行已按ASCII/hex复核。
- 结束仍115个Rust文件，SHA-256 `577722fb235e7f5b1a30277cd1e1ee1f2d1697217009e26bb34772d46d4decda`，与开始一致。执行前置门、重定向、仅x权限、共享FD/预算/取消、持久副作用见证及真实进程边界通过。没有降低原两秒判据。
- 本增量仅完成虚拟执行映像与exec入口；**setuid/file SGID及真实/有效/保存身份仍未完成**，第一层整体未完成。未操作宿主权限/系统配置/真实设备或实际.env数据根，未提交/推送。旧失败和修订记录保留。

<a id="execution-identity-2026-11-14"></a>
## 执行身份与内容写入清位（完整回归进行中）

本批合并实现：类型化真实/有效/保存UID/GID及补充组；凭据统一进入ACL/目录/创建/命令模块；exec只改变虚拟身份，不改变宿主；会话FD和作业控制仍绑定原会话，文件访问使用有效身份；身份切换使用独立任务租约并精确回收，嵌套边界不泄露身份。写入门覆盖路径追加/替换、打开截断与旧FD写入/定位写/截断，先持久清除特殊位再写字节，失败不带特权新内容。开发中只跑相关测试，整条链完成后一次全量验收；暂不将后续inode/链接或九层标完成。


### 行为与接口

- [Account身份](../src/users.rs)采用`Identity::Login / Executed(ExecutionIdentity)`；登录账号持久文件格式不变。`Account.uid`为有效UID，`session_uid()`为真实会话UID。`id`输出`ruid/euid/suid/rgid/egid/sgid/groups`；`whoami`输出有效账号名。这里都是虚拟凭据，不改变宿主进程身份。
- [执行映像](../src/filesystem/execution.rs)仍先用调用者凭据检查祖先和文件x/ACL/绑定，再从同一快照派生身份。文件04000取owner UID，02000取owner GID；真实UID/GID及补充组保留，保存UID/GID更新为执行结果。目标owner账号不存在则拒绝执行。`sh/source`不应用特殊身份；没有任何x位仍拒绝。
- [运行时](../src/script/runtime.rs)在exec内建立新任务租约，并清空函数命名空间/本地调用帧；返回外层边界时恢复调用者身份、调用栈及租约。保留预算、取消、已授予FD与流式管道；新映像自身定义的函数可被其后台子作业继承，调用者函数不能借映像获得权限。变量/参数仍是输入，不把脚本解释器当作敌对宿主安全沙箱。
- [共享模块](../src/script/shared.rs)、[FD](../src/script/fd.rs)、[JobTable](../src/script/jobs.rs)以真实会话UID隔离资源，数据访问用有效凭据；普通`su`仍关闭旧表并join作业。第三方`ModuleCommands`若未实现`execute_as`，收到Executed身份时显式拒绝，不静默以旧身份执行。
- [清位门](../src/filesystem/privilege.rs)按`ObjectId + storage_generation`定位当前绑定，`PrivilegeClear::Unchanged / Cleared / DetachedGeneration`区分无改动、已清位与已分离存储代。非空write/write_at/append、truncate、打开截断、replace在改字节前持久清除06000，包括虚拟root写入。读、sync、空写不清位；已拒绝的只读FD写不清位。rename后的旧FD仍清同对象；替换/unlink后的旧FD不能清新对象或新存储代。
- 清位是独立access联合事务：故障可留下“位已清、内容未变”，不得留下“新内容仍有执行特权”；恢复后可重试内容操作。该策略有catalog读取、克隆、编码及有特权时的持久事务开销，不声称零成本或真实断电已经验证。`OpenFile.info().ownership`仍是打开时快照，不冒充实时权限。
- `ELARA_OWNERSHIP_CATALOG_V3`用于带文件06000的规范记录；V1/V2旧数据仍可读/写，旧头夹带新位拒绝。目录04000、文件01000仍拒绝。非组成员不能通过继承目录GID后创建/chmod安装文件SGID；单独chgrp清除文件06000。不同步Windows DACL或Unix chmod。

### 本批证据与修订

- 新增[文件系统专项](../tests/filesystem/privilege.rs)、[脚本专项](../tests/script/privilege.rs)，扩展[真实CLI](../tests/execution_cli.rs)。六条内容修改路径断言实际字节和清位；三处事务故障验证字节未改及恢复；V3往返、代际隔离、补充组及非成员SGID均有断言。
- setuid对照先证明同一映像能创建持久root文件，再删除该测试见证并仅摘04000，断言无法重建；SGID对照摘02000后无法读组文件。没有使用任务退出后/tmp消失来冒充未执行。
- 函数隔离测试先真实失败0/1：调用者`helper`经映像调用获得root写入能力。修复后专项库4/4、脚本5/5通过（18.24s/26.33s）；之后又补充字节、补充组与非成员SGID断言，纳入本次完整回归。首次精确筛选漏模块名导致0项执行，已纠正，不算通过证据。
- 真实CLI2/2、既有目录SGID CLI1/1通过（8.60s/5.67s）。此前CLI夹具试图chmod未登记布局目录`/root`，被`LegacyOwnershipUnknown`拒绝；改用库明确登记的`/tmp/suid`映像，不改生产权限门。后台专项初用无作业环境API、后误wait自身编号，均修正测试接线，不放宽断言或时间阈值。
- 开发编译曾报告4处账号借用不匹配，以及新测试误用`FileMode::read()`和缺少`fs::read(...).unwrap()`，均已修正；一次Windows增量目录收尾报AccessDenied警告，但当次测试正常退出。此处保留失败记录，不把历史421项套用本批源码。
- 历史378/390/408/421节描述各自源码快照；其中“setuid/file SGID未实现”已由本节替代。第一层完整inode/虚拟链接以及后续八层仍未完成；本批没有修改实际.env数据根、宿主权限/账号/系统设置，也没有提交或推送。

<a id="internal-vfs-2026-11-14"></a>
## 内部 VFS、镜像与伪文件系统（2026-11-14）

### 源码和实际命令

- `src/filesystem/vfs/model.rs`：inode/目录项、权限、限额、镜像结构校验；`volume.rs`：内存事务、文件/目录租约、锁和打开能力；`registry.rs`：内部挂载。
- `src/filesystem/vfs/image.rs`：ELARAFS1 校验 checkpoint；`pseudo.rs`：生成信息/受限设备。
- `src/filesystem/vfs_bridge.rs`：DataRoot 账号门；`src/script/vfs.rs`：控制命令。普通 IO、ACL、find/du、exec、目录游标和 FD 使用同一后端接口。
- `CommitObject::Host(ObjectId)` 与 `CommitObject::Virtual { filesystem, inode }` 分离提交身份；`ExecutableFile` 同样区分身份域，不能拿两个卷的 inode=2 当同一对象。
- 可运行的命令串见[命令账本](ubuntu-style-commands.md#internal-vfs-commands)；锁和共享打开见[FD 账本](file-descriptors.md#vfs-fd-2026-11-14)。

### 已执行的证据（非九层完成率）

专项命令：`cargo test --offline --lib --test script_language --test execution_cli --target-dir target/fs-build vfs -- --test-threads=1 --nocapture`。

- 核心 14 passed / 0 failed；脚本 6 passed / 0 failed；真实进程 1 passed / 0 failed；exit 0。
- `tests/execution_cli.rs` 的 `vfs_real_process_checkpoint_restart_and_pseudo_devices_preserve_host_boundary` 启动两个独立 binary 进程：第一进程保存镜像，第二进程加载并经硬链接修改同一 inode；配置与根外 sentinel 不变，宿主没有 mnt/work 等挂载目录。
- 该专项之后新增 CommitObject 身份域、4096 目录项/配额表限制、受管读写/追加/改名/删除路由以及原子 replace 测试；当前完整回归单独记录，不能用上面的 21 项代替。
- 专项曾两次失败：ACL 删除扩展后 owning-group=5，应为 750 非 740；exec 替换当前解释器，后续测试必须放外层并用子解释器 exec。均修正测试上下文/精确判据，未放宽权限或改变 exec 语义。

### 明确边界与代价

1. **2026-11-15 修订**：挂载位于该 DataRoot 能力域内的 /mnt/NAME，现支持已有内部卷目录下的新名称作为嵌套挂载点；退出不保留挂载表；普通用户原路径写入限制不改变。默认 /proc /sys /dev 现已接不可卸载虚拟 provider（专项通过）；显式 /mnt provider 仍保留，未知 backing 对象导致启动冲突，绝不自动接管。
2. **2026-11-15 修订**：原实现每次复制整棵 Tree 和文件 Vec；现已改为复制元数据／页索引、共享不可变 4KiB 数据页，写入时只 detach 受影响页。不是磁盘 COW；复杂度及实测见下节。
3. 配额统计 payload/链接目标字节和 inode；硬链接不重复计 payload，但目录项另限 4096。BTreeMap/Vec 实际分配与元数据物理大小未计量。
4. 镜像最大 1MiB，解码 payload 最大 8MiB；卷 payload 最大 64MiB 不表示每个合法卷都能保存。4KiB 内容块去重与 RLE 只发生在镜像编码，不是运行时压缩。
5. 普通写入仅内存可见；FD sync 明确 Unsupported。vfs save/seal/disk-create/disk-save显式生成或更新根内受管镜像，并原样报告目录 durability（Windows 可能 Unconfirmed），不假称断电持久。
6. 校验不是加密认证。两份块中一份校验失败则只读加载，两份坏或头/结构坏则拒绝；不能直接取消只读。显式 clone 或导出新镜像是新对象，不是原地 repair。
7. 快照只保存具名对象，打开后已 unlink 的对象随最后租约释放；动态 provider 不允许 snapshot/image，不能把动态节点序列化成普通持久文件。
8. **2026-11-15 修订**：运行态任务登记与精确回收已接；应用/缓存登记、配额及关联回收专项通过；显式块镜像缓存/分配/持久COW/修复、认证密封与冻结overlay已接。透明持久挂载、live overlay与网络FS仍未实现。仍按九层计划推进，不将上述跨层基础记为九层全部完成。

### 历史记录修订

执行身份阶段实际完整回归为 432 passed、严格 Clippy 和默认 build 通过；源码为 118 个 Rust 文件，SHA-256 `5e3873d3abbecdda146bc38636a9fba1e588ce533ab0c25ac68e0d6f3ff419f1`。此前 README/本页写“进行中”未同步，现改判为该历史快照已通过；新 VFS 源码与该指纹不同。

<a id="runtime-cow-2026-11-15"></a>
## 运行态生命周期与共享数据页（2026-11-15）

### 运行态：真实进程链路

- 新 `/run`、`/var/run` 文件及目录必须传入同 DataRoot 能力域、同 UID、仍有效的 TaskScope；无 scope 新建拒绝。Shell 的 mkdir/touch/重定向/owned 复用现有任务 scope，不增加独立回收命令或定时器。
- `OwnedStorageClass::RuntimeState` 使用文件 catalog V4；带 TaskExit 的运行态目录使用 access catalog V5。旧版本 OwnerRequest 运行目录保留原编码和归属，不升级为自动删除对象。文件、目录和任务元数据均有编码往返测试。
- 正常退出释放租约后精确回收；启动检查登记锁的存活证据，回收已死任务的确切对象；活任务保留，未知名称不扫描，绑定被替换或混入未知子项时 deferred。跨类别 rename 拒绝，不能把运行态改名成永久用户数据来绕生命周期。
- 已存在文件的普通 open 是已有能力授权，不要求创建新 scope；创建新文件仍强制 scope。内部卷的 `/mnt/x/run` 按全局路径分类，不能误套宿主 `/run` 生命周期。
- `cargo test --offline --lib --test host_boundary_cli --target-dir target/fs-build runtime_state -- --test-threads=1 --nocapture` 是本批运行态入口。
- 已执行核心 2/2（6.39s）；真实进程 1/1（10.41s）。CLI 测试先验证正常退出，再只终止测试自己启动的 binary，确认运行文件遗留，最后启动新 binary 触发恢复；未知 `/run/unknown`、根外 sentinel 和 fixture.env 字节不变。最新重跑核心 3/3（6.96s），真实进程 1/1（7.24s），含旧 catalog 兼容性测试，均通过。

### 数据页：消除整份 payload 克隆

- `src/filesystem/vfs/data.rs` 提供有界稀疏 4KiB 数据页，零页不分配，事务／快照／克隆通过 Arc 共享不可变页，修改只复制受影响页。元数据和 BTreeMap 页索引仍复制，因此不是 O(1) 全卷快照，也不称零成本。
- resize 缩短会清除尾页残留，重新增长读到零；释放页索引遵守 Arc 引用生命周期，快照／旧引用仍在时不会提前释放对应页。
- 保留 ELARAFS1 的旧字节序列 serde 格式：新编码仍可反序列化为 Vec<u8>，旧镜像可读；没有悄悄改变磁盘格式。镜像编解码仍是全量 checkpoint，不能把内存页 COW 叫作磁盘写时复制。
- `vfs stats` 新增 `allocated_data_bytes`（该卷引用的非零数据页分配量，不含元数据）、`shared_data_pages`（与本卷其他页或其他卷／快照共享的页引用）、`cow_copied_bytes`（成功变更时 detach 的数据页字节数）。跨卷相加会重复计共享页，不能当宿主进程 RSS 或磁盘物理用量。
- 对照测试直接检查 Arc::ptr_eq：克隆共享所有页，改一个字节只打断对应页共享，其他页继续相同；序列化再解码的独立副本作为不共享对照。卷级测试检查 16KiB 四页文件跨页写两字节仅复制 8192 字节，快照原文不变；本轮已实跑通过；包含 20 项 VFS 核心、6 项脚本和 1 项真实进程测试，均为 0 failed。

### 全量失败修订记录

上一批完整回归：449 passed / 6 failed，严格 Clippy 通过；因测试失败默认 build 未执行。根因为原始接口在路径校验之前调用 VFS route，把 InvalidPath 包成 Vfs(InvalidPath)。统一调整 ownership_requires_actor 校验顺序，六项旧测试逐项通过，不改预期或放宽路径门。随后为补旧运行目录兼容性主动停止一次未完成验收批次；该批次只确认 Clippy 通过，未完成测试，不能算完整验收。

### 本批专项结果

- VFS/COW：20 核心 + 6 脚本 + 1 真实进程 = **27 passed / 0 failed**；运行态：3 核心 + 1 真实进程 = **4 passed / 0 failed**；顺序执行批次 exit 0。
- 共享必要性对照、只复制受影响页、尾页清零、镜像字节往返、运行目录旧记录保留、异常进程重启恢复均在本批实跑。
- 修订后的严格 Clippy → 完整 no-fail-fast 测试 → 默认 build 已重新启动；执行期间冻结 Rust 源码并核对前后指纹，结果未返回前仍不标完整验收通过。

<a id="namespace-2026-11-15"></a>
## 命名空间增量（469 项完整回归通过）

- 工作目录用 search-only 租约，不要求目录 read；clone 共享租约，最后释放后才解除 Busy。内部目录使用物理规范路径，删除原 symlink 不改变 cwd。与 POSIX 可重命名 cwd 不同，本实现明确拒绝重命名/删除仍作为 cwd 的目录及祖先。
- 内部挂载可位于 /mnt/NAME 下的新名称；嵌套父目录必须已存在且不经过 symlink。注册时持有父目录租约；子卷存在时父卷不能卸载。解析检查每一级父卷 search 权限，不能绕过私有父目录进入公开子卷。
- 账号目录游标合并底层目录项与直属挂载项，有总条目上限；快照持有其列出的子卷。refresh 成功才更换快照；被删除的旧目录不会附着到同名新目录的挂载项。ls/find/du/glob 共用此目录门。
- 此节历史快照的Host cwd仍为路径语义；当前改动见[Host cwd生命周期](#host-cwd-2026-11-16)，尚未完成新全量验收。卷内 symlink 仍不跨挂载，父卷 snapshot/save 不包含另行挂载的子卷。
- 新测试覆盖：无租约可改名/删除的负对照、克隆最后释放、search-only 目录、symlink cwd、嵌套父权限拒绝/放行、目录快照 Busy、find/glob/CLI 与根外哨兵。

命名空间专项命令：`cargo test --offline --lib --test script_language --test execution_cli --target-dir target/fs-build vfs -- --test-threads=1 --nocapture`。实际 23 核心 + 2 真实进程 + 8 脚本，共 33 passed / 0 failed；编译 39.80s，测试分别 1.22s、16.72s、21.07s。编译出现一次既有 Windows incremental finalize AccessDenied 警告，不是测试失败；没有清缓存或修改宿主权限。随后统一四种挂载入口的命名锁内最终检查/发布，已启动严格 Clippy → 完整回归 → 默认构建及源码指纹复核。

命名空间最终完整结果：严格 Clippy 15.24s；281 核心 + 167 脚本 + 21 其他真实进程/PTY = 469 passed / 0 failed；默认 build 27.16s。129 个 Rust 文件 SHA-256 `c0425d640dedba2b14256f776d52f9cd06aaa3e984b4349cdefdaefd19fe4b85` 前后相同。随后才开始以下默认 provider 改动。

<a id="system-providers-2026-11-15"></a>
## 默认特殊后端（474 项完整回归通过）

- DataRoot 启动检查 /proc、/sys、/dev 的固定能力目录；三处都必须为空。任何既有对象导致 SystemProviderConflict，保留原数据并拒绝启动，不自动认领、删除或隐藏未知对象。
- 默认挂载返回明确 System(Proc)/System(Sys)/System(Dev) 类型，用户卷为 User；vfs mounts 如实展示。系统 provider 不允许 unmount，不能借卸载退回宿主目录。
- /proc/identity 按打开者有效凭据生成，/sys 提供版本和配置上限；不枚举宿主任务。默认 /dev/null、zero、full 只提供受限虚拟字节 IO。
- 设备写只开放已确认的系统设备句柄，包括 guest；不改变全局 permissions::check，不开放系统目录命名、普通创建、替换、chmod、删除。设备打开仍验证 mode/ACL；full 非空写 NoSpace，零长度写成功；sync 不冒充持久化。
- Windows 系统前缀 ASCII 大小写别名统一路由到 provider，不能用 /PROC 绕回宿主 backing。无 actor 的 raw 读拒绝伪造身份；raw /sys 写保持 ReadOnlySystemPath 旧契约。
- 本批新增核心身份/设备/未知对象保留对照、guest 脚本和真实进程测试；完整任务目录尚未接入，不声称 proc 已覆盖完整进程模型。

默认 provider 专项：`cargo test --offline --lib --test script_language --test execution_cli --target-dir target/fs-build system_providers -- --test-threads=1 --nocapture`，3 核心 + 1 CLI + 1 脚本通过；随后 VFS 23 核心 + 2 CLI + 9 脚本以及原 policy 3 项通过。去掉两轮重复执行的一项脚本，共 41 个不同测试全绿。之后补强普通权限门仍拒绝、同名宿主设备文件不变及 /PROC 不读取宿主秘密的对照；已冻结并启动全量 Clippy/test/build，未提前报告最终通过。

默认后端最终验收：严格 Clippy 14.30s；284 核心 + 168 脚本 + 22 真实进程/PTY = **474 passed / 0 failed**；默认 build 22.69s。131 个 Rust 文件 SHA-256 `734fdb66786d0744b96c3defac3ee94d5eb80d9a3d50d25bd222e195a0611f05` 前后相同。

<a id="resources-2026-11-15"></a>
## 应用与缓存资源链（独立资源489项全回归通过，关联增量专项通过）

源码入口：[资源模型](../src/filesystem/resource_model.rs)、[持久资源服务](../src/filesystem/resources.rs)、[命令接口](../src/script/resources.rs)。测试：[核心](../tests/filesystem/resources.rs)、[脚本](../tests/script/resources.rs)、[真实进程](../tests/execution_cli.rs)。

- root 在 `/opt`、`/srv` 下登记 Application，或在 `/var/cache` 下登记 Cache。必须创建全新目录，已有内容不认领；同一资源不能嵌套。ResourceId 取不可复用目录 ID，与目录出生在同一个持久事务提交。AccessCatalog V6、文件 catalog V5 保留旧编码往返，不全量迁移已有数据。
- DeleteCondition::ResourceDelete(ID) 让文件/目录归属于资源，不因发起 shell 的任务退出而删除。逻辑字节和注册对象配额包含根目录；最多64个资源，每资源64MiB逻辑数据、4096对象，既有单次IO/事务1MiB限制不变。
- 创建、目录新增、路径append/replace、open截断、FD write/append/write_at/truncate 在已有跨进程命名锁下检查；先检查额度和状态，再清特权位、写字节。拒绝不留新名字/部分数据；FD按ObjectId+generation验证当前记录，不按旧打开路径重新认领。
- 旧代资源FD即使宿主仍保留打开数据，也不能再写，返回 ResourceDetached；Removing/Evicting 中返回 ResourceInactive。UserData旧FD语义不变。同资源文件/子目录可改名，禁止跨资源移动和资源根改名。
- 卸载先持久进入 Removing，再只遍历登记对象，核验原对象绑定并逐项回收；未知对象、额外内容、替换/无法确认绑定保留并计deferred，登记保留供下次启动恢复。命令遇deferred返回状态1，不冒充卸载成功。
- cache clear 先持久进入 Evicting，回收已登记子对象，完成后回Active且保留原资源ID/根目录；未知cache文件从不自动纳入额度或删除。这是明确登记缓存的生命周期操作，不是全盘扫描清理工具。
- 配额是**当前已登记payload的逻辑字节**，不计暂存峰值、catalog/标记、未知对象、宿主分配块和已删除但仍打开的旧代；不是物理磁盘上限、全系统配额或应用身份沙箱。每次资源写会有界扫描登记对象和读取大小，尚无增量空间账本或性能保证。
- 当前 Application/Cache/ApplicationData 是管理员资源记录，不是软件包安装器；已新增应用到缓存/数据位置的不可变ID关联及整组卸载，未提供依赖管理、每应用独立执行身份和压力驱逐。自有读缓存/写回属于另一层，不能把这里的磁盘缓存生命周期记作页缓存已完成。

首次专项6/7通过；唯一失败是测试把Windows平台名等同于deny-delete共享模式。读取实际打开门确认未要求该模式，实际旧FD存在时也可删除。修订为先确定性进入Removing验证拒绝，再按实际删除结果分别验证Inactive/Detached，仍断言旧字节不变和最终登记清空。额外加入旧代替换、同名再注册ID不复用、宿主替换保留、跨DataRoot并发预算、缓存驱逐重启测试。


### 应用关联位置增量

- 独立资源链最终完整验收：严格Clippy16.36s；295核心 + 171脚本 + 23其他 = **489 passed / 0 failed**；默认build30.59s。136个Rust文件SHA-256 `3aaf424586c0f265546d83b5d4360839e1e4fe16c935fec07f01df757107e513` 前后相同。
- `app cache APP PATH BYTES OBJECTS`、`app data APP PATH BYTES OBJECTS` 将全新缓存或home/root下的新数据目录，在同一创建intent中绑定应用ResourceId；`app locations APP`查询明确位置。ResourceAssociation是Independent/Application(ID)枚举，旧独立记录保持V6字节往返，带关联使用AccessCatalog V7。关系不能重新指向同名新应用，不认领已存在目录。
- `app uninstall`一次策略事务将应用及所有关联位置置Removing，包含尚未完成Evicting的关联缓存。然后先回收子资源；任何子项deferred则父应用保留、全部拒绝新写；重启按组续办，避免重复回收/计数。无关home文件和未关联缓存不受影响。关联位置仍分别计配额，不伪称应用总额度合并限流。
- ApplicationData使用UserData存储类别、ResourceDelete删除条件；普通UserData保持OwnerRequest。裸写和跨资源改名不能跳过登记、额度或生命周期。当前登记/回收仍管理员控制、资源所有者为root，不是普通用户应用沙箱；应用身份、指定`/var/lib/elara/apps`视图、依赖管理和完整包安装仍未完成，不将P4改判全部完成。
- 关联首批验收：15核心、4脚本、1既有真实进程 = **20 passed / 0 failed**。额外加入关联真实进程跨重启、数据裸写/跨边界改名/Windows别名负对照；需下一批实际输出才能计为通过。


<a id="block-store-2026-11-15"></a>
## 自有块存储、缓存与持久COW

入口：[块存储](../src/filesystem/vfs/block_store.rs)、[inode分页](../src/filesystem/vfs/block_volume.rs)、[受管镜像桥](../src/filesystem/block_images.rs)、[测试](../tests/filesystem/block_store.rs)。只操作授权根内受管普通UserData/OwnerRequest文件，不操作裸盘、分区或宿主挂载。

- ELARABK1固定4KiB槽，8..65536槽（32KiB..256MiB）。逻辑键为类别/inode/page，文件页不受其他文件长度改变影响；inode、目录和卷级索引单独分页。清单最大32768条，两代根及其全部引用保留；删除后须等旧代退出才复用槽，允许非连续孔洞分配。
- 写入按数据/清单→sync→另一超级块→sync发布。未变记录逐字节核对后复用；单页数据变化专项验证恰好3×4096字节实际写入（数据、清单、根），不是整镜像rewrite。清单每次整体重写，未宣称B树级增量元数据。
- 缓存预算1..512页，LRU+脏页写回。stats报告命中/未命中、实际读写字节、writebacks、cached/dirty页、allocated/free槽、largest_free_run、generation。槽占用不是宿主文件物理分配块；记录集/解码Tree另占内存，不计为该页缓存预算。
- 两根和清单/数据均SHA-256校验；坏最新根/数据回退为RecoveredReadOnly，所有可选根失效拒绝；共享数据损坏可令两根都失效，COW不是备份。disk-repair仅验证并重发幸存代，不恢复损坏代已丢失的修改。
- 命令：`vfs disk-create MOUNT IMAGE SLOTS CACHE_PAGES`、`disk-save MOUNT IMAGE CACHE_PAGES`、`disk-load IMAGE MOUNT CACHE_PAGES`、`disk-stat IMAGE CACHE_PAGES`、`disk-repair IMAGE CACHE_PAGES`。默认不自动写回；disk-load完整载入内存卷，退出/卸载前未显式保存的修改会丢失。
- create只接受全新路径；容量预检在新对象创建前。未知镜像不认领，不覆盖已有文件；运行中底层IO失败可能保留已登记但未完成的镜像供诊断，不承诺任意IO故障完全无残留。调用期间使用已有跨进程命名锁，受管对象绑定仍验证，写前清虚拟06000。
- 9核心专项全部通过（7.02s），包括碎片复用、容量拒绝、缓存1页、每个写入/同步断点、根与数据损坏、显式修复、稀疏长度/打开孤儿排除、跨重启往返。同批脚本1项和真实进程1项通过；包含容量预检失败不创建空镜像的断言。真实断电、Linux原生、透明持久挂载与外部设备均未验证/实现。

<a id="sealed-images-2026-11-15"></a>
## 认证加密checkpoint

入口：[AEAD封装](../src/filesystem/vfs/sealed.rs)、[密钥与镜像门](../src/filesystem/sealed_images.rs)。使用离线已有ring0.17.14的ChaCha20-Poly1305、系统随机256bit密钥与96bit随机nonce；不是自制密码算法。

- `vfs keygen KEYFILE`仅新建root受管0600密钥文件；`vfs seal MOUNT IMAGE KEYFILE`；`vfs unseal IMAGE MOUNT KEYFILE`。不通过命令文本传密钥，不输出密钥。seal/unseal前核对密钥归属、OwnerRequest、权限和绑定；错误密钥或篡改在加载/挂载前返回Crypto。
- 加密现有ELARAFS1压缩/去重checkpoint，整个信封≤1MiB，随机nonce每次不同；相关缓冲使用Zeroizing。不宣称所有序列化中间副本都已安全擦除，也不抵御恶意宿主/内存读取；密钥和镜像保存在同一根不是离线密钥隔离。
- 此能力不是透明块加密，不支持密码派生、自动轮换、回滚防护或加密网络FS。认证成功仍须验证内部格式；失败不附加挂载、不输出解密明文。
- 1核心+1脚本+1真实进程专项通过；覆盖正确往返、不同nonce、错误密钥、头/nonce/密文/tag篡改和截断、权限拒绝、失败挂载不存在与密钥不出现在输出。真实进程跨重启回读且stdout/stderr/log均不含原始密钥。

<a id="dedup-overlay-2026-11-15"></a>
## 运行时去重与冻结叠加视图

入口：[去重](../src/filesystem/vfs/dedup.rs)、[稀疏页](../src/filesystem/vfs/data.rs)、[叠加合并](../src/filesystem/vfs/overlay.rs)、[核心测试](../tests/filesystem/vfs.rs)、[命令测试](../tests/script/vfs.rs)。本节取代此前“overlay完全未实现”的描述，不取代尚未实现的live overlay承诺。

- `vfs dedup MOUNT`：管理员显式处理一个可写普通卷。SHA256仅分桶，完整4096字节相等才合并Arc引用；无后台任务、无跨卷全局池、无磁盘数据改写。硬链接按inode计一次；尚被FD持有的孤儿仍参与；全零稀疏页不占实体页。
- `DedupStats`区分页引用、前后独特页数、重定向引用数及独特引用字节减少。其他快照仍可能持有原Arc，因此数字不等于实际释放内存、RSS下降或磁盘空间回收。逻辑配额/内容/元数据/时钟不变；后续写入仍按页COW。处理时锁住卷，临时桶和指针集合与实体页数成正比；不是恒定额外内存算法。
- `vfs overlay LOWER UPPER DEST [WHITEOUT...]`：分别冻结两个卷根，upper同名覆盖，目录递归合并，硬链接组保留；whiteout仅隐藏lower。路径须卷内绝对路径，拒绝根、空组件、`..`、反斜杠/冒号/NUL，不跟随symlink。使用upper限额和UID配额，合并后图验证失败不挂载。
- 新视图拥有独立元数据、共享不可变页，修改不写回源卷；源卷后续修改不传入新视图。两个源快照不是联合原子快照；没有长期upper写回、动态whiteout或live overlayfs语义。保存/重载的是flattened tree，只保留合并结果，不保存可变层关系。
- bi6f8lxpj：去重2核心+1脚本、overlay2核心+1脚本，6 passed / 0 failed。去重包含伪造hash桶负对照与真实Arc指针断言、幂等、旧FD、快照COW隔离；overlay包含重载whiteout精确NotFound。随后新增稀疏尾页/硬链接/打开孤儿和密钥准入专项另行验收，不挪用旧结果。

<a id="persistent-snapshot-2026-11-15"></a>
## 快照只读属性持久化

入口：[归档标记与访问状态](../src/filesystem/vfs/archive.rs)、[卷序列化](../src/filesystem/vfs/volume.rs)、[块记录](../src/filesystem/vfs/block_volume.rs)、[实际进程测试](../tests/execution_cli.rs)。

- 旧实现的明确缺口：snapshot只读状态仅存在内存，保存Tree后重载会变可写。b8vrjwupe两个真实进程分别验证checkpoint与块镜像重载后能写入，配置和根外哨兵不变；该对照是缺口证据，不是快照验收通过。
- 新增ELARASN1前缀，只用于显式冻结快照。普通卷沿用原Tree JSON字节；旧无标记档案按普通checkpoint读取，不能推断为冻结快照。未知版本拒绝，旧读取器无法将带标记档案误读为可写Tree。
- checkpoint payload和块镜像卷元数据共用该规则；标记纳入现有校验，密封checkpoint还受AEAD认证。`vfs snapshot`得到的卷经`save/load`、`disk-create/disk-load`、`seal/unseal`保存恢复后保留冻结状态。
- `vfs stats`的access为ReadWrite、FrozenSnapshot、RecoveryReadOnly、FrozenRecoveredReadOnly或VirtualProvider。用户冻结不伪装成损坏恢复；虚拟设备仍只开放原先定义的字节IO，不因状态类型化新增宿主操作。
- 修改冻结内容须显式`vfs clone SNAPSHOT DEST`，副本可写，源保持不变。一个持久快照名字就是调用者选定的根内镜像路径；不同镜像不共享磁盘块，不提供自动快照目录、保留策略或跨进程常驻挂载。管理员仍可按原归属规则显式删除/替换镜像文件，不声称宿主不可变属性。
- basetogdg严格Clippy15.19s通过；27项VFS核心、3项页存储、14项VFS脚本、11次块存储执行、5次provider执行和1项快照双进程均通过，共61次通过、59项不同测试（2项脚本重复执行）。新增快照2核心+1双进程已包含；b20ceb8u0补充块镜像repair冻结守卫，3核心+1双进程通过；两批共60项不同测试通过，严格Clippy16.55s与默认构建28.26s通过，前后源码/Cargo指纹一致。一次Windows incremental finalize AccessDenied仅为编译警告，未修改宿主权限或清缓存。

<a id="cross-mount-links-2026-11-15"></a>
## 跨内部挂载链接

入口：[逐组件解析](../src/filesystem/vfs/resolution.rs)、[账号桥接](../src/filesystem/vfs_bridge.rs)、[命令](../src/script/vfs.rs)。

- 保留`vfs symlink TARGET DEST`的卷根相对语义；新增`vfs symlink-global TARGET DEST`，TARGET必须绝对虚拟路径。GlobalSymlink与旧Symlink在镜像中是不同变体，旧解码器拒绝新变体，不靠字符串前缀猜测链接种类。
- 仅跨内部已注册挂载解析；目标落到Host或目标挂载不存在时返回CrossDevice，不打开Host对象、不跟随宿主链接。目标挂载重新出现后可重新解析，路径链接不是固定卷身份引用。
- 普通读写、FD打开、exec、ACL与cd跟随最终链接；unlink、rename、readlink、lstat、独占创建只解析父目录。跨卷rename和硬链接仍拒绝；普通卷内链接也能经过嵌套内部挂载。全链最多40次链接展开。
- 每个目录先验证search再处理`..`；重新检查展开目标的账号路径门与写类别，不能借/mnt别名写/sys。已访问卷在解析期间保留Arc，最终目标在返回路由中继续被pin。解析和后续单卷操作不是跨卷原子事务，不宣称跨卷一致快照。
- 修订记录：首批脚本遗漏mv的两条durability警告，已改为逐字完整stderr；独占创建的最终单卷门曾错误跟随链接，已修复并覆盖普通悬空/循环/全局链接。bj33xeqvt跨挂载4核心+1真实进程+2脚本通过；含既有VFS/provider/快照共60项不同相关测试通过，严格Clippy与默认构建通过，源码前后指纹一致。b9kqefv0r另覆盖命名、执行权限及资源通道。默认cargo run实跑读写、cd、删链接保留目标、Host拒绝成功，根外哨兵与配置不变。

<a id="proc-task-leases-2026-11-15"></a>
## proc任务租约目录（专项及默认实跑已验收）

入口：[租约只读观察](../src/filesystem/task_status.rs)、[动态目录发布](../src/filesystem/vfs/volume.rs)、[核心测试](../tests/filesystem/system_providers.rs)。

- `/proc/tasks/ID/status`使用不复用的TaskScopeId，不是宿主PID或脚本JobId。字段为`task_scope_id`、`owner_uid`、`lease_state`、`kind: resource-lease`。不读取宿主进程、环境变量、命令行或密钥。
- `LeaseHeld`仅表示协作租约锁被持有，不等于任务Running；`Recoverable`表示正确marker且锁当前可取得；`MissingLease`表示登记存在但marker缺失；`Unconfirmed`表示读取/锁/格式不能确认。只读观察不调用recover_tasks、不清理、不自动认领孤立marker。
- 列表可见登记ID；各任务目录0500、status0400，仅owner/root可读。目录及文件都是内部节点，不在Host的proc目录创建对应文件。任务结束后新查找看不到旧名称；打开status的FD保留打开时字节，已打开目录保留被引用后代，关闭后可回收。
- 默认proc命名空间访问时刷新，登记快照与视图发布处于同一命名锁区间，避免旧快照覆盖新快照；一次最多64个登记，动态树上限4096 inode/1MiB payload。旧打开引用占用预算，耗尽明确拒绝刷新；不是无限缓存。独立显式pseudo proc不自动绑定某个DataRoot的任务登记。
- 这是资源租约视图，不是完整运行任务模型：暂停/恢复/取消/退出码仍由脚本jobs接口管理，尚未投影到proc。首批测试因OsString/String比较编译失败，已修正测试类型；bg2bjzouc严格Clippy与3项核心通过，但真实进程失败：测试误将纯读取脚本当作已有资源租约。已读Context::creation的惰性登记门，改为创建临时对象后观察，并增加纯读取不产生登记的反例。b8h79ru19严格Clippy、76次测试执行（去重64项）、默认构建通过，前后源码指纹一致。默认cargo run两进程实跑成功，生成LeaseHeld视图、会话退出后临时对象回收、重启无租约条目，配置与根外哨兵不变。

- 生命周期细化：独立script::run结束释放本次资源租约；交互shell的嵌套sh可共享外层Context，因此子sh返回后租约仍可见，直到最后持有者结束。默认实跑在子sh返回后观察到`1/`，会话退出并重启后列表为空；不将子sh边界误写为独立租约边界。

### 全量回归异常与复验（2026-11-15）

- bpfe51g21：严格Clippy通过，核心测试326通过/1失败。失败是缓存注册的目录原子发布返回Windows PermissionDenied（os5），包装为AccessCommitPending；并非proc状态或缓存驱逐断言失败。该批后续集成测试和默认构建未执行，该批不能标全量通过；后续完整结果见本节末。
- 同一测试在三个独立新夹具中复验均通过，尚不能稳定复现，根因未确认；没有添加盲重试、复制覆盖回退或修改宿主权限。原失败夹具保留pending intent与精确owner marker，公开目标未出现。
- 新增测试专用BeforeDirectoryPublish注入，覆盖发布被拒后由原持久intent恢复同一资源ID、登记与可写目录。bficeg0h1已完成严格Clippy（8.61s）、该守卫（1.93s）、全目标538项（328核心+181脚本+29其他，0失败）及默认构建（9.32s）。守卫额外先跑1次，不计为第539项。前后源码指纹均为0d60dfca0fbdaeb76de9c0a235570b190e4b5e6225e1c1f0a04703b14e345251。旧520项为历史完整基线；一次全绿不等于已定位先前偶发os5的根因。

<a id="proc-runtime-jobs-2026-11-15"></a>
## proc作用域作业目录（全量及默认实跑通过）

入口：[弱引用观察源](../src/filesystem/runtime_jobs.rs)、[真实作业状态](../src/script/jobs.rs)、[状态与生命周期测试](../tests/filesystem/system_providers.rs)。

- `/proc/jobs/OBSERVATION_ID/status`独立于`/proc/tasks`资源租约。目录ID在同一DataRoot观察域内单调分配，跨JobTable同号`%1`不会覆盖；字段`job_id`仍是所属JobTable的局部ID，不能拿观察ID直接发`kill %ID`。重启后观察域重新建立，不是持久任务ID或宿主PID。
- 状态来自实际作业Control和完成记录：Running、PauseRequested、Stopped、Resuming、Cancelling、Cancelled、Exited(code)、Panicked。Stopped要求全部已登记worker确认暂停；发出暂停请求不等于已经暂停。worker异常展开用退出守卫记录Panicked；不通过持锁租约猜状态。
- legacy `TaskTable` 的前台/后台 `sleep` 使用独立 `AtomicI64` 观察源，发布 `task_id` 与 `TaskExited(i32)`，不抢表锁、不消费 Receiver；身份切换在 `tasks → files` 锁序下退休旧任务记录，ID 不复用。它不是宿主进程观察，也不代表一般前台命令已全部纳入。新增兼容/隔离/容量守卫正在验收，不挪用scoped历史全量。
- root/所属会话UID可读status（目录0500、文件0400），列表只公开观察ID；不输出命令正文、参数、环境变量或私密输出。身份归属使用session_uid，保留既有setuid与共享会话的作业授权语义。
- DataRoot持Weak，不延长JobTable/worker生命周期；每域最多256条活观察登记，死源在登记或读取时移除。scoped作业wait/reap后新路径消失；legacy终态保留有界历史，身份切换或表释放时消失；旧FD保留打开时字节，旧目录引用沿用proc退休引用规则。克隆文件域共享视图；独立DataRoot即使打开同一磁盘根也不假装拥有另一个进程的线程视图。
- 这是只读投影：控制仍由jobs、kill、bg、fg、wait处理；不会读取宿主进程或发送宿主信号。当前接scoped脚本作业与legacy TaskTable计时任务；前台解释器调用现由独立/proc/executions观察，尚非宿主进程目录。没有通过proc写入控制接口。
- bwdwziz7g严格Clippy通过，核心2通过/1失败：同ID状态改变后旧FD错误读到新字节。open已经捕获generated且stat使用快照，read_at的Content::File分支却绕过快照。已修共同读取入口并补定位/分段/clone偏移/EOF/长度守卫；btx1x39ni严格Clippy8.90s、98次执行/83项不同测试及默认构建26.43s通过，前后源码指纹f7afb0d492f77c54dc064763a5635d1566cafe81074c32ab0364ab49706dc558一致。538项完整基线属于本增量之前的源码；blt0r8j6f当前源码全量已通过，详见本节末。
- 修订先前proc租约快照证据边界：旧测试验证退出移除名称后FD保留内容，但未验证同ID状态变化；不能据此前测试声称动态内容更新时快照始终正确。本次失败作为修复前反例保留，不修改Running预期去迁就Panicked。

- 默认cargo run实跑已完成：sleep内部worker确认Stopped，打开/proc/jobs/1/status，取消并wait后列表为空，旧FD仍读出Stopped；配置和根外sentinel不变。输出含既有取消诊断`Error: execution cancelled`，对应主动取消，不声称本次stdout零Error。返回DEFAULT_PROC_JOBS_SNAPSHOT_OK，不读取真实.env、不创建宿主proc文件。

- 完整验收blt0r8j6f：严格Clippy0.47s；all-targets --no-fail-fast共543 passed / 0 failed / 0 ignored（331核心、182脚本、30其他），默认构建0.58s。前后源码指纹均为f7afb0d492f77c54dc064763a5635d1566cafe81074c32ab0364ab49706dc558，FAILURES []、PROC_JOBS_FULL_VALIDATED。完整作业测试模块已包含，不再依赖jobs子串命中数。该结果不等于九层全部完成，也不消除历史偶发Windows os5的未定位根因。

<a id="opened-identity-2026-11-15"></a>
## 已打开对象身份（专项及默认程序通过）

入口：[对象实现](../src/filesystem/objects.rs)、[短时身份探针](../src/filesystem/binding.rs)、[核心守卫](../tests/filesystem/binding.rs)、[FD命令守卫](../tests/script/fd.rs)。

- `fd same LEFT RIGHT`经过真实表和代际门，不重开opened_path。dup同描述为SameObject；独立Host FD使用正/冲突/释放锁对照，读权限或外部锁使结果不确定时返回Unconfirmed，绝不按路径/同字节猜同一对象。内部卷比较volume_id+inode；跨后端DifferentObject。
- bksabmgxl：严格Clippy11.23s、3核心+1脚本新测试、9项binding回归全部通过（10项不同测试），默认构建14.22s。增量编译收尾曾报告Windows os5警告，不称宿主问题已解决。
- bi3vv0v2j默认cargo run实跑：改名保持同一身份，替换后旧新generation不同；unlink后FD仍分别读old/new，stale token拒绝。3次SameObject、2次DifferentObject、DEFAULT_FD_IDENTITY_OK，配置和根外哨兵不变。目录同步PermissionDenied警告原样保留。
- 这不是Host硬链接创建或统一Host inode模型。当前正在接入的长期锁使用独立租约，不复用此payload探针；543项全量属于之前源码。

<a id="host-locks-2026-11-15"></a>
## 受管Host协作锁（V3全目标560项与默认实跑通过）

入口：[持久租约](../src/filesystem/host_locks.rs)、[全通道守卫](../tests/filesystem/host_locks.rs)。

- 以ObjectId+storage_generation登记独立打开；dup共享同一租约。跨DataRoot/进程以namespace锁串行协调，独立OS文件锁只证明租约存活，不占用payload/owner marker，不改宿主ACL。
- 根内私有open.catalog为有界256条、128KiB文本协议。V3按Pending→Held→Retiring登记：先登记未授予的出生，再在最终私有位置创建容器、原子发布owner/payload并链接witness，最后加锁提交Held，不依赖目录rename。Held租约payload/witness是同一OS对象的两个名称；活锁必须两见证均冲突，失活必须通过正/冲突/释放身份对照及精确字节核验才退休。缺失Held、同字节单见证替换不认领、不删除；旧V1/V2非空拒绝自动升级，空catalog保留next升级。没有cron或清理命令。
- 受管FD读/定位读/写/定位写/截断/同步、路径读/追加/替换、执行映像、复制源、块镜像及密钥读取接同一锁门。普通open不读字节，锁冲突由实际IO检查；truncate-open在截断前检查。独占共享与锁分别表达Busy和LockConflict。名称rename/unlink不撤销旧FD；新generation不继承旧generation锁。
- 代价修订：原实现每次namespace扫描全部租约，实测0/16/64活租约下每次cat约20.066/46.188/127.868ms。现改为All/One/Io/Open显式扫描选择：IO读取权威catalog并只探测自身/相关冲突，Drop仅自身，启动及容量满时全量；无mtime缓存或进程内授权替代。新版默认程序0/16/64活租约实测15.850/27.578/17.218ms每次cat（每组20次，存在波动，不作延迟保证）；登记/锁变化/退休仍有原子文本发布及同步成本。不是OS级安全沙箱，不宣称真实断电/Linux验证完成。
- bgoyk0v0a：严格Clippy16.77s；6项Host锁核心279.49s（含256租约耗尽）、真实双进程1项10.72s、既有owned_open10项68.08s、身份比较4项均通过，共21项不同测试；默认build33.29s。前后源码指纹0f1748643cff3aa61430997649fd3f0855d9be5cfe01f19433d620e0dcdb769f一致，HOST_LOCKS_RECOVERY_TARGETED_OK。该批属于V1历史实现，后续发现同字节替换缺陷，不能据此声称完整替换保护；全目标批次已停止，不据此把第二层标完成。
- 真实进程守卫：第二进程的路径读、追加、FD读精确LockConflict；只终止夹具内第一子进程后，后续进程恢复登记、独占打开并成功写入。配置/根外哨兵保持不变，正常释放后无lease残留；这是进程终止恢复，不是真实断电。

- 默认cargo run批次bvxdm9m8u已读：DEFAULT_HOST_LOCKS_OK；4次预期LockConflict、1次预期Busy，dup在原FD关闭后仍持锁，unlock后读到original，独占描述写后读到changed!；SameObject探针不受长期锁影响。退出后无open-*.lease，配置和根外sentinel不变；目录durability PermissionDenied警告保留。

### 租约身份缺陷与性能修订

- bcvncva4s真实反例：持锁进程仍活着，将原lease移走，在原路径写同字节新文件，另一进程读到protected，替代物还被误删。原因是V1仅验证内容与当前路径锁，不证明仍是原来的锁对象。之前的未知内容测试不覆盖此情况；已就地收紧上方结论。原全量b1dhcqs7l已停止，不能报通过。
- V2修复bxurhhexy：严格Clippy14.93s，同字节替换payload/witness（活/死两态）、Pending/Retiring及未知/缺失恢复3核心通过，真实双进程1项通过18.22s，默认build25.18s；HOST_LEASE_WITNESSES_VALIDATED。双见证不能抵抗能重写整套私有协议的恶意宿主管理员，不外推OS级防护。
- 后续按冲突恢复新增结构性对照：共享锁不会阻止读取，因此其损坏见证不应导致无关全域扫描；同对象写入、自身解锁和显式启动恢复仍必须检查并fail-closed。新增V1升级/容器暂存/退休部分删除测试，由bgst48bl7单独验收，不能挪用上一批计数。

- bgst48bl7：Clippy24.04s通过，11核心10通过/1失败；容量组在新租约容器目录rename处os5，未进入容量断言，后续CLI/默认build未执行。bxpwq97gb独立同门再次失败，不把它解释成容量阈值问题，不靠重复跑到绿。V3从协议上取消该目录移动依赖，保留一般目录rename故障未定位事实。
- V3的Pending从未授予FD，允许逐步恢复空容器/精确owner/owner+payload/完整pair；存在文件必须逐项验证，非空时必须有owner，witness不能单独存在。Held必须完整，Retiring允许已验证退休后部分删除。所有文件暂存仍经/tmp原子发布；五个出生边界的确定性PermissionDenied故障守卫检查原payload未截断、next不回退及重启恢复。basmt6xhc已通过独立验收，详见下条。

- basmt6xhc当前V3验收：严格Clippy23.81s；Host锁12核心291.64s、真实双进程1项11.59s、recovery命中16项28.66s全部通过（2项重复，共27项不同测试），默认build23.64s。前后源码指纹8ea268dcbe63cadfccd55a705332cb038bcb2bed6e3bec21a0460371e60693ae一致，HOST_LOCK_BIRTH_AND_SELECTIVE_RECOVERY_OK。容量断言未改；出生五边界、部分退休、未知内容/同字节替换、独占共享、全IO门及按冲突扫描对照均包含。V3默认程序成本/替换对照bax5kv2dh已通过；完整回归buutghn5h已通过，见下述冻结证据。

- bax5kv2dh实际默认程序：0/16/64活租约20次cat分别15.850/27.578/17.218ms每命令，exit0、ERRORS=[]，正常退出无lease残留。64组比旧127.868ms下降约86.5%；16组高于64组，不据单次测量承诺稳定延迟，也不把压力测试总时长当单次IO成本。
- V3默认双进程同字节替换对照：payload、witness分别替换，持锁进程仍活；第二进程均exit2并拒绝启动，未读出protected；catalog、替代物和移走原见证保留，配置/根外sentinel不变。DEFAULT_HOST_V3_COST_AND_IDENTITY_OK。这是相关身份门的实测，仍不抵抗整个私有协议被宿主管理员重写。

- V3完整验收buutghn5h：严格Clippy0.37s；全目标560 passed / 0 failed / 0 ignored（核心346项883.86s，脚本183项375.62s，其他31项），默认build0.53s。SOURCE_BEFORE与SOURCE_AFTER均为8ea268dcbe63cadfccd55a705332cb038bcb2bed6e3bec21a0460371e60693ae；FAILURES []、HOST_LOCK_V3_FULL_VALIDATED、exit0。该冻结证据不代表Host硬链接/符号链接/覆盖rename或九层整体完成。

<a id="host-hardlinks-2026-11-15"></a>
## Host受管普通文件目录项与硬链接（27项专项及573项全量通过）

- 名称表只存path→ObjectId；OwnedInode保存唯一owner/mode/代际/生命周期，OwnedFileInfo仅查询投影，不是另一个授权来源。V1–V5单名称记录保持规范字节往返；多名称使用V6单份inode和名称表，拒绝悬空引用、孤儿、非法ID及不规范编码。
- 命令入口沿用`vfs link SOURCE DEST`。已接同Host受管普通文件；未知归属不认领，创建目标不覆盖，只允许对象所有者或虚拟root扩展名称。源与目标必须同存储类别、同资源域；跨Host/内部卷仍CrossDevice。
- 普通FD写入共享payload；atomic replace按既有旧FD保留旧代契约，将该对象全部登记名称切到新代，不仅替换一个别名。协作调用者由namespace事务隔离；不承诺外部宿主原始观察者看到多名称同时变化。
- 所有别名共享ACL、特权清位、锁和独占打开门；资源字节/对象按inode计一次，名称表仍限4096。删除最后名称才回收私有payload及ACL；旧FD能力不因unlink撤销。
- 模型首步严格Clippy及4项契约已通过。Host入口缺失负对照b9en8dbdm为0通过/3失败，均在hardlink_as返回CrossDevice；后续V6、完整名称事务及Host分派已接，专项bo86al6be严格Clippy12.69s、4项模型0.01s、3项端到端12.35s全通过，HOST_HARDLINKS_INITIAL_VALIDATED。此前560项是修改前冻结证据，不能覆盖本节。

- 扩展批次bfwirr81z：link三阶段、replace五阶段（含一新一旧名称混合）恢复、陌生目标保留、共享ACL清位/目录rename/TaskExit、授权/跨后端、V6畸形图及真实双进程测试已通过。严格Clippy9.36s；7项核心38.15s、真实双进程1项9.98s、ownership19项18.60s（含5模型），共27项不同测试；默认build36.21s。前后源码指纹0a5ae20814ccfffc24d9b9826253d41e6a293f3333c7c68f2530df486b26a5d9一致，HOST_HARDLINKS_RECOVERY_VALIDATED、exit0。全目标b678239dm已通过573项，详见下述冻结证据；默认程序独立实跑bjwctj7sv已通过。Host目录元数据仍在既有AccessCatalog，不将普通文件inode拆分称为目录/符号链接统一完成。

- 默认程序bjwctj7sv：target/debug/elara-host.exe在全新显式fixture.env根启动两次，均exit0。第一进程输出SameObject、OLD=original、NEW=changed!；重启后删除/改名/最后unlink，旧FD仍输出UNLINKED=changed!。直接检查V6中两个名称引用同一ID，关闭后无租约残留，fixture.env和根外sentinel逐字不变；DEFAULT_HOST_HARDLINKS_ROUNDTRIP_OK。保留Warning: committed; directory durability unconfirmed: PermissionDenied，不将其表述为已确认宿主目录同步或断电持久性。

- 完整冻结验收b678239dm：严格Clippy0.27s；all-targets --no-fail-fast共573 passed / 0 failed / 0 ignored（358核心858.76s、183脚本369.22s、32其他），默认build0.50s。SOURCE_BEFORE、SOURCE_AFTER与完成后重新计算的当前源码指纹均为0a5ae20814ccfffc24d9b9826253d41e6a293f3333c7c68f2530df486b26a5d9；FAILURES []、HOST_HARDLINKS_FULL_VALIDATED、exit0。目录inode统一、Host符号链接、Host覆盖rename及九层其余缺项未被本验收宣布完成。

<a id="host-symlinks-2026-11-15"></a>
## Host虚拟符号链接与目录共用模型（15项专项及默认程序实跑通过）

- 修订：此前“Host目录统一模型未接”已被共用InodeTable迁移取代。文件与目录仍有不同ID域和持久catalog；目录只允许一个名称，不将其描述为全部对象持久协议合并。byxaq1hn2严格Clippy及35项相关测试通过，旧目录JSON字节兼容。
- Host链接是V7 inode的typed Symlink与字面target，物理载体仍为虚拟根内普通文件；不创建或放行宿主原生symlink/reparse point。普通单名V1–V5、多名V6字节兼容；V7可规范往返，链接不允许硬链接别名。
- 读、写、FD、执行捕获、ACL与目录路径使用同一namespace锁内逐段解析。rename/unlink/readlink/stat不跟随最终名称，目录父路径仍解析；Exclusive open不跟随最终链接。相对target以链接所在目录为基准，绝对target以虚拟根为基准；dot在展开过程中处理，预算40次。本节历史版本Host与内部挂载之间仍CrossDevice；后续跨后端增量见[共享解析与统一元数据](#cross-backend-2026-11-16)，不把后续能力追记为本节旧验收结果。
- 目录观察区分Symlink与BlockedLink；固定目录游标刷新在自身根能力持锁期间核验普通载体与私有payload身份，不凭相同字节或路径认领链接。镜像及密钥直读也在既有锁内解析并重验资源、策略与协作锁。
- 已实测：原未实现版本3项均在Vfs(Unsupported)处红；接入后首轮3项通过15.56s。扩展批次严格Clippy通过、7项6通过/1失败，失败为初次目录投影拿原根guard验证克隆根导致InvalidRoot，现已按实际guard门修复，未放松根身份检查。
- bbrk9hop5最终验收：严格Clippy10.78s，8项Host行为78.60s、真实双进程1项12.31s、6项编码契约0.01s全部通过，默认build34.50s；前后源码指纹2115a79cd1bbcb8521d0c54f58b92b98bf17b64f5c2caa23cffc2e052347c86a一致，HOST_SYMLINKS_RECOVERY_VALIDATED、exit0。编译增量目录曾报PermissionDenied/os5警告但编译成功，不隐去该环境问题。
- bc4fpwdgz默认程序独立实跑：两次启动均exit0，OLD=old/NEW=new、TARGET=dir/data、RESTART=new、SURVIVES=new、LINK_EXEC_OK成立；目录链接cwd显示/root/d，配置和根外sentinel不变，链接载体不是原生symlink。DEFAULT_HOST_SYMLINKS_ROUNDTRIP_OK；directory durability PermissionDenied仍明确报告，不声称真实断电持久性。
- bi5fvsqhd完整回归通过586项（370核心905.26s、183脚本368.79s、33其他），0失败/0忽略；Clippy0.43s、默认build0.55s。前后及完成后重算源码指纹均为2115a79cd1bbcb8521d0c54f58b92b98bf17b64f5c2caa23cffc2e052347c86a，FAILURES []、HOST_SYMLINKS_FULL_VALIDATED。解除冻结进入Host覆盖rename；该586项基线不覆盖后续源码。

<a id="host-rename-2026-11-15"></a>
## Host覆盖rename（37项相关测试及真实双进程通过）

- `vfs rename SOURCE DEST` 接入受管Host文件、虚拟符号链接与目录；普通 `mv` 仍不覆盖。最终名称不跟随链接，父路径解析；源/目标同inode为经验证的no-op。未知目标、不同对象类型、非空目标目录、目录自包含、跨后端、跨存储类别或资源域仍拒绝。
- 文件用V3双对象intent精确描述源对象移名与受害对象失去一个名称，禁止附带修改第三对象/元数据/计数器。单次宿主文件rename后提交catalog；只有受害对象最后名称消失才退休payload/ACL。双方其余硬链接和已打开旧目标FD保留，不拼接独立unlink事务。
- 目录使用显式RenameReplace联合事务：先将有身份见证的空目标移入私有retired位置，再移动整个源子树，提交文件与目录catalog，最后回收退役目标。协作观察受namespace锁及恢复门隔离；不承诺外部原始宿主观察不到中间空名，也不是已证明的断电原子性。操作不修改宿主权限、挂载、磁盘或实际数据根。
- 负对照：文件2项在CrossDevice处失败；目录1项在LegacyOwnershipUnknown处失败。首轮严格Clippy13.68s、7项通过37.16s，覆盖两侧别名/旧FD、no-op/链接、四点文件恢复、同字节未知目标、sticky/资源/跨后端拒绝、V3精确图验证及目录基本覆盖。
- bg2maq4wl完整输出已核实：严格Clippy9.43s；覆盖rename9核心55.22s、真实CLI双进程1项11.66s、access兼容27项111.78s全部通过，共37项不同测试；默认build21.14s。目录退休间隙/双catalog四点恢复及陌生目录保留均包含在内。前后及完成后源码指纹3e9bef2975a244bbe7f9eeb242961154b6f024bcc146142ce372074fbd93154d一致，HOST_RENAME_RECOVERY_VALIDATED、exit0。
- 随后仅更新程序help的Host link/symlink/rename说明，再冻结源码启动b4oqm7h1i：严格Clippy→默认build→默认程序独立双进程实跑→all-targets完整回归。该批已完成：596 passed / 0 failed / 0 ignored（379核心、183脚本、34其他），Clippy10.71s、默认build9.48s；默认双进程验证NEW=new、OLD_FD=old、OLD_ALIAS=old、TREE=payload及重启数据保持。前后/完成后指纹f4a7aa564bf50fa72f9976a9ff50414a55efddeb1683e6ffc57f09184d1e1d46一致；HOST_RENAME_FULL_VALIDATED。后续legacy任务观察改动不由此基线覆盖。


<a id="proc-foreground-2026-11-15"></a>
## legacy任务与一般前台执行观察（专项通过，全量测试通过但冻结失败）

入口：[legacy任务表](../src/tasks.rs)、[前台观察源](../src/filesystem/runtime_executions.rs)、[解释器入口](../src/script/runtime.rs)、[真实进程守卫](../tests/execution_cli.rs)。

- `/proc/jobs`新增`kind: legacy-task`，局部编号为`task_id`，终态为`TaskExited(i32)`。proc直接读独立原子状态，不拿TaskTable锁、不抢完成Receiver；scoped作业仍用job_id及原有控制状态。公开观察ID不等于这两种局部ID。
- 计时任务在开始前登记Running；结束/异常Drop更新状态。成功与Failure(0)内部编码分离，保留旧API枚举语义。历史有界；身份切换与会话结束退休旧记录并通过取消通道唤醒长计时线程，不让反复切换累积3600秒线程；编号checked_add拒绝耗尽，不回绕复用。独立前台计时保留1席，不与后台capacity抢同一槽；这不是完整QoS调度器。
- `/proc/executions/ID/status`是当前活跃前台解释器调用。普通交互Program与独立脚本共用execute_entry；同步sh/source/exec沿用当前调用，scoped后台不重复注册。显示execution_id、owner_uid（session_uid）、Running、foreground-execution；不泄露命令/参数/环境，不查询宿主PID。认证/UI会话控制不是解释器调用，不伪造对应执行记录。
- 前台观察有独立32项容量，后台256条作业满额不占此池。登记失败在执行副作用之前返回status2/steps0；正常完成、错误和Rust展开均释放强引用。这里不留退出历史；旧status FD仍是打开时快照。下一次读刷新移除死行，目录权限0500/status0400，owner/root可读。
- 三类proc动态目录用stride8的互斥inode对，避免新执行ID与资源租约/作业ID碰撞。克隆文件域共享观察，独立DataRoot/重启不共享进程内执行状态；只在内部卷发布，不生成宿主proc文件。
- legacy专项b6exoev2b：严格Clippy15.37s；9任务核心、3观察核心、1真实双进程、2脚本及整个16项job_tests通过（去重29项）。前台缺失对照bz66mg4ku因NotFound失败；首轮b3sgl4x3c严格Clippy16.44s及1脚本1.45s通过。扩展bzld09lwe严格Clippy7.35s；前台2核心、1真实双进程、2脚本通过，全部provider13项与全部job_tests18项通过，去重32项。默认程序独立实跑成功（DEFAULT_RUNTIME_OBSERVATIONS_OK）；brxnlaiyq全目标611 passed / 0 failed / 0 ignored（388核心、187脚本、36其他），但末尾前后源码原始字节指纹不一致，最终exit1，冻结验收失败。期间切分支重写文件，不能据此升级完整冻结基线；复验bpddn18zb为继续开发主动停止，本次Host cwd增量后重新冻结。

<a id="host-cwd-2026-11-16"></a>
## Host当前目录生命周期（620项全目标冻结回归及默认实跑通过）

入口：[Host锚点](../src/filesystem/host_cwd.rs)、[租约协议](../src/filesystem/host_locks.rs)、[事务命名门](../src/filesystem/access.rs)、[核心守卫](../tests/filesystem/access.rs)、[脚本守卫](../tests/script/vfs.rs)、[真实双进程](../tests/execution_cli.rs)。

- 受管目录按search-only授权，固定目录ID、已开marker和跨进程租约；clone共享同一租约，最后引用释放后才可删除或重命名。与内部卷一致，活跃cwd阻止自身/祖先改名、删除与覆盖，不承诺目录移动后仍按旧文本路径继续访问。
- 在命名空间锁内比较候选事务的名称→ID，统一覆盖一般目录操作、TaskExit及资源回收。Busy在发布access.pending之前返回，不留下等待当前进程自行释放的pending死锁。
- open.catalog V4区分File/Directory ID域，复用原双见证和Pending→Held→Retiring恢复；旧V3文件记录仍可读，旧实现不识别V4应拒绝，不能无条件降级。原文件payload锁不作cwd长期锁。目录和文件共用256项登记上限，满额明确失败。
- Protected分支只用于既有不可删除/改名的布局目录；未知普通Host目录明确LegacyOwnershipUnknown，不接管、不伪造身份。该分支没有额外cwd租约持久写入；受管分支承担原租约登记/退休IO成本。
- 解释器每次进入pipeline时复验同路径锚的身份与search权限，防止旧快路径把权限撤销或不同marker当作原目录。此保护依赖协作协议，不抵抗宿主管理员整体篡改私有catalog/移植marker，不是宿主OS沙箱。
- 负对照bxvz4qf6r：1项失败，独立DataRoot删除被钉住目录没有返回Busy。扩展专项bhdw4870r已通过：Clippy19.15s，4核心24.26s、1真实双进程9.88s、1脚本5.41s、12项旧Host锁267.46s，共18项。随后补V3活跃租约升级、TaskExit延期回收及共享满额守卫，并将后代检查收敛为一次相关集合扫描；收口专项bzxqr3r9c通过：Clippy7.41s，6核心27.88s、1真实双进程10.46s、1脚本6.42s、1共享容量470.64s，共9项，0 failed/0 ignored。全量bqnmhrxmt：默认build40.38s及默认两进程占用/释放/重启实跑通过；618 passed / 1 failed，唯一失败为tilde语法测试以宿主API创建未知目录而被新cwd门拒绝。修改前指纹与启动一致；修订夹具为受管创建，原stdout断言保留并增强status/stderr，新增未知目录拒绝且不接管守卫。复验b5laf7dv6已通过，见下方独立验收记录；保留本次失败和夹具修订依据。

- 最终冻结复验b5laf7dv6：严格Clippy1.48s；全部glob专项8项11.29s；all-targets共620 passed / 0 failed / 0 ignored（394核心938.66s、189脚本374.18s、37其他）。前后166个src/tests/Cargo文件指纹均为e516688bb017b442b89b09ec893a6744a6c486f2a9abc37ecb1b9b5aba425fd4，FAILURES []、HOST_CWD_FINAL_ALL_TARGETS_VALIDATED、exit0。默认程序双进程已在bqnmhrxmt验证相同生产代码，输出DEFAULT_HOST_CWD_VALIDATED；后续仅修订测试。此620项为当前完整冻结基线，不代表跨后端链接、live overlay、网络FS或其余九层缺项已完成。Windows目录同步PermissionDenied与Linux/真实断电验证缺口仍保留。

<a id="cross-backend-2026-11-16"></a>
## 跨后端解析与统一元数据（627项全目标冻结回归及默认实跑通过）

入口：[共享逐段解析](../src/filesystem/namespace_resolution.rs)、[Host解析门](../src/filesystem/host_symlinks.rs)、[统一元数据](../src/filesystem/metadata.rs)、[行为守卫](../tests/filesystem/host_symlinks.rs)、[元数据守卫](../tests/filesystem/metadata.rs)、[真实进程重启](../tests/execution_cli.rs)。

- DataRoot门面现在共用逐段walker；Host虚拟链接和内部全局链接可双向跳转，整个解析共享40次预算。普通内部链接仍以卷根解释绝对目标，不通过`..`越过卷根；Host绝对目标以虚拟根解释。dot先检查当前目录/搜索权限再归约，禁止file/../x或denied/../x绕过。
- 原始Host路径及落到Host的跨卷别名，均在真正读写/改名的namespace guard内重新解析、复验身份；先前分派观察不授予修改权限。纯Registry没有Host能力，仍拒绝Host目标，不偷偷打开宿主路径。挂载对象在本次walk期间保持引用，目标卷由返回Route持有。
- 最终名称nofollow语义保留：unlink/readlink/rename/Exclusive create操作链接本身，普通读写与cwd跟随。hardlink/rename使用解析后的同卷路径，Host分支再走原事务；不同卷或Host↔卷拒绝CrossDevice，绝不复制删除。
- proc任务刷新改为接收已有guard，避免共享walker下递归获取命名锁；jobs/executions仍使用原观察源。内置系统写策略与身份搜索门保持，跨后端不授予额外权限、不放行宿主原生链接。
- `vfs inode PATH`返回InodeMetadata：身份为HostFile(ObjectId)、HostDirectory(ID)、Virtual(filesystem,inode)、ProtectedLayout或Unknown。Host数字ID仅在同一数据根协议内可比，不是OS inode、不是跨数据根全局ID；Unknown/ProtectedLayout不是对象相同的证据。
- 元数据权限为Managed(owner,mode)、ProtectedLayout或Unknown；名称计数Known仅统计登记的公开名称，不把payload/marker私有硬链接计入。Host时间为真实Timestamp/Unavailable，内部时间为Logical mutation ticks；不伪造宿主uid、nlink或统一wall-clock。Host元数据投影复用原stat身份/未知别名门，在同一guard内完成，不接管未知对象。
- 证据：b9zr1i6iw旧实现两项行为负对照失败；接入后b4866h4zy严格Clippy与两项跨后端测试通过，脚本1通过/1失败（系统写拒绝错误类型变更，已恢复原Denied映射，未降低断言）。bvnhnf390的Host inode负对照在Unsupported处失败。扩展专项bhugbemf1通过：严格Clippy14.72s；cross_backend核心4项22.13s（含原跨后端硬链接拒绝测试）、真实CLI重启1项9.92s、统一inode3项4.41s、跨挂载脚本2项5.46s、原Host链接8项63.32s、系统provider13项8.22s，共31项，0失败，CROSS_BACKEND_METADATA_TARGETED_OK、exit0。Windows incremental finalize os5警告仍保留。b2simxdmq已启动默认构建、默认程序两进程和all-targets前后源码指纹冻结验收，完整结果为626通过/1失败，167文件前后指纹一致；唯一失败为旧真实进程测试仍期待未挂载目标CrossDevice，新行为为NotFound，后续挂载读写正常。已修订该历史预期，保留错误数量和精确数据断言并增加无Host影子目录检查；生产代码未改，复验前不升级620项完整基线。
- 本节验收时未完成文件/目录持久catalog迁移；后续实现与证据见[联合账本](#joint-catalog-2026-11-16)。跨数据根持久统一身份协议仍未完成，不宣称原第2项全部完成。设备写例外仍仅限既有内置dev入口，不将路径链接当作新增设备写授权。其余应用隔离、自动缓存驱逐、透明写回、空间总账、共享磁盘快照、live overlay、透明块加密/全局去重和网络FS继续在原计划内。

- 最终复验bjlbvnect：Clippy1.55s、修订的真实重启守卫1项4.23s；all-targets627 passed / 0 failed / 0 ignored（400核心1079.86s、189脚本387.37s、38其他），前后167文件指纹f8649b1e812880ec47aff4177cd8b56e8867dff6727949d15de9f7bbc6e5384d一致，FAILURES []、CROSS_BACKEND_FINAL_FULL_VALIDATED、exit0。默认程序在b2simxdmq已验证相同生产代码双进程往返，DEFAULT_CROSS_BACKEND_VALIDATED；只修测试预期后全量复验通过，保留历史626/1失败。目录同步PermissionDenied和incremental os5警告未宣称解决。

<a id="joint-catalog-2026-11-16"></a>
## Host持久联合账本（专项通过，最新完整回归645通过/3失败）

入口：[迁移与联合发布](../src/filesystem/namespace_catalog.rs)、[文件账本门](../src/filesystem/ownership.rs)、[权限事务](../src/filesystem/access.rs)、[核心守卫](../tests/filesystem/namespace_catalog.rs)、[真实双进程](../tests/execution_cli.rs)。

- root执行`vfs catalog-upgrade`，在原跨进程命名锁内恢复旧pending后迁移。返回`CatalogUpgrade::Migrated`或`AlreadyCurrent`，都携带`DirectoryDurability::Synced/Unconfirmed`，CLI直接打印；不能把原子可见当作断电耐久。V1保持兼容读取，不自动接管未知文件；V2以`namespace.catalog`为文件/目录/ACL/资源/任务信息的单一持久权威。`store.owner`升级为`ELARA_OWNERSHIP_STORE_V2`，旧版本按原V1门拒绝继续操作；不支持手工改回marker降级。
- 迁移日志保留旧文件、权限账本的原始字节，并明确区分权限账本缺失与存在。联合封套按有界长度前缀保存两个规范分量；文件、权限各400KiB，联合与迁移日志都受1MiB上限。新内容由旧内容确定性派生，不把同一快照重复存两遍导致有效容量减半。原revision、文件ID、目录ID和策略不重编号；身份仍带File/Directory域，不是跨数据根全局ID。
- 发布次序为迁移日志→联合账本→V2 marker→精确退休旧文件账本→精确退休旧权限账本→移除迁移日志。已发布且字节匹配的联合账本可复用；切换前必须存在全部原有文件，切换后允许已完成的退休步骤。每次先验证所有残留旧文件，再删除任何一个；未知marker、未知账本、篡改和不匹配状态保留并拒绝，不覆盖、不吞IO错误。缺失的V2主账本不能回退重建V1。
- 文件创建/替换、覆盖rename、目录联合事务、policy和初始化全部通过显式`CatalogComponent`发布门；通用协议发布函数不按文件名截流。旧AfterFiles/AfterCatalog检查点保留，未完成事务仍需日志恢复，不能把“单文件权威”误称为所有公共名称与策略一次物理写入。普通命名门和直接启动恢复门都先恢复迁移。
- 升级不关闭或撤销活跃FD、cwd、任务lease。其独立open.catalog、任务锁和对象payload继续由原协议管理；不是把所有协议文件打成一个大文件。旧进程按V1 marker门拒绝新操作；升级不替宿主撤销既有OS文件句柄。
- 证据：命令缺失负对照bxeneuuat失败；接线后b2jt0btlu编译27.94s成功，但原测试把两次cat输出拼接，受到既有rename目录同步PermissionDenied告警干扰。改为两次独立读取标记并断言最终文件原始字节，保留无Error与告警。bdrk5cp13在新测试缺少文件内容参数处编译失败，已补实参。bk972ecpj专项12核心90.04s及真实双进程1项10.75s全部通过（编译81s），包括V1～V7往返、迁移五断点、篡改拒绝、旧pending/V2事务恢复、活跃FD/cwd及同号跨域。扩展b2bod3kbp已验证：摘除直接恢复门后1项按预期失败（NotFound，1.32s），finally恢复源码原字节；严格Clippy23.66s通过，16项核心130.51s、1项真实CLI重启9.83s通过，JOINT_CATALOG_TARGETED_VALIDATED、exit0。16项核心含1个子进程入口；父测试分别在五个发布阶段骤退子进程并从独立DataRoot恢复。容量拒绝及V2缺账本无回退守卫通过。bthoxixep实测Clippy0.63s、默认build25.54s、默认程序双进程DEFAULT_JOINT_CATALOG_VALIDATED通过；all-targets在416核心、2 default_acl_cli、22 execution_cli和4 fd_cli通过后，于既有fd_cli取消+清理小于2秒断言失败，后续目标未运行。169文件前后指纹394feb920cc7a7059cdf9f1eff4675f4a89151039615960ab4778e1cfcd436e0一致。原测试隔离复验bjo6qmpit通过（7.43s），不据此抹去全量失败；只补实际elapsed诊断，2秒阈值及清理断言不变，后续并发矩阵及归因修订见下文。完整冻结基线暂仍627项。
- 边界：这是同一虚拟根内Host协议升级，不提供跨根全局身份、不迁移内部卷镜像格式，不是完整应用隔离或宿主OS沙箱。只写测试/配置指定虚拟根，不改宿主磁盘、挂载、账号或权限。Windows目录同步PermissionDenied、Linux原生及真实断电验证缺口仍保留；进程中断恢复不等于断电耐久。

- 取消清理性能修订记录：默认并发3轮稳定失败（2.164/2.610/2.139s）；上一提交同机2轮通过（1.795/1.613s），临时对照后当前源码原字节恢复。合并marker读取后仍有2轮超时（2.103/2.147/1.935s），未降低2秒阈值。现将同锁migration/access/owner恢复合并为一次目录验证，保持检查顺序与失败保留，后续复验见下文；该读取优化不能单独解释或消除全部时限失败。

- 取消测试准备门修订的实证：分段计时显示等待命名锁仅约1.8ms，持久回收/退休约645/931ms；不能把所有延迟归为目录重复打开。原测试用宿主exists观察公共名字，三轮均实测此时owner pending仍存在。改为经程序自身读取确认提交，再计时TERM+wait+完整清理，保留原2秒门；三轮1.433/1.608/1.445s全部满足，证明未完成创建曾污染取消计时。该批第一轮另一个进程恢复测试初始化遇Error但旧断言未输出文本，后两轮全过；已补错误诊断并复验，不把该独立失败标为已定位。

- 最新FD复验bk4og0k97：三轮均5 passed / 0 failed，取消+完整清理1.799/1.535/1.438s，原2秒门保留，准备同步前owner_pending三轮均true。独立初始化错误未再复现，保留历史记录而不宣称已根治。随后完成的bes6zemrr完整冻结结果见下一条，不用本批三轮结果替代全量结果。

- 最新完整冻结bes6zemrr：Clippy9.64s、默认程序双进程DEFAULT_JOINT_CATALOG_FINAL_VALIDATED通过；all-targets --no-fail-fast共640 passed / 4 failed（416核心，186/189脚本，38/39其余）。169文件前后指纹d30d12f6835c8139473487bac444c10225054f806bb9d8c5f34e0323c892b569一致，耗时1224.86s。失败为FD取消清理2.0775208s及三个脚本作业时限守卫；准备信号修订和读取去重不足以保证完整负载下通过。保留原2秒门及默认并发。后续b8stugzd2完成上一提交完整脚本/FD目标两轮对照：FD两轮均5/5通过（1.4631957s、1.8053931s），脚本两轮均186/189，同三个时限守卫失败；原工作区按字节恢复。因此三个脚本时限失败在旧版当前环境也可复现，不能全部归为联合账本新增回归；当前FD负载下偶发超时仍未收口。未提交、未推送，不升级627项历史完整通过基线。


### 持久发布固定标记复用（专项通过，完整回归645通过/3失败）

入口：[事务发布](../src/filesystem/transaction.rs)、[恢复](../src/filesystem/recovery.rs)、[守卫](../tests/filesystem/transaction.rs)。

- 分段插桩bgwckkxx0：18项作业专项15通过/3时限失败；归属标记创建、写入、同步约56–644ms/次，数据同步另有成本。插桩已原字节恢复；这不是正式吞吐基准，不宣称所有耗时都来自标记。
- 事务私有目录增加不可变`replace.owner`。首次先在普通stage写入并`sync_all`，再用硬链接发布完整种子；后续逐次核验普通文件类型及精确字节，以硬链接建立`transaction.owner`。不跨锁缓存、不共享可变payload、不减少数据文件同步，也不改变目标原子发布协议。
- `MarkerPublication`显式区分`Seeded`、`Shared`、`Independent { link_error }`。链接不可用时独立创建、写入和同步标记，不能把回退路径算作已节省同步。损坏/目录/链接种子拒绝使用且不覆盖；恢复只略过已验证种子，未知事务内容仍保留。
- 专项包含真实别名验证（复制相同字节不能通过）、两种种子状态下的强制回退、损坏种子与旧payload保护、未知stage归属禁止覆盖；负对照摘除共享分支。bx0m8ercp已验证：新增4项0.76s通过；摘掉共享分支后别名守卫按预期失败（AlreadyExists，0.42s），原源码逐字节恢复；严格Clippy22.21s通过，事务10项2.50s、恢复4项0.80s通过。bmmc1nowf已完成默认程序双进程及完整冻结回归，结果见下一条；不宣称全部时限已解决。

- bmmc1nowf最终证据：默认build25.09s、两次默认程序启动均exit0/stderr空，DEFAULT_JOINT_SHARED_MARKER_VALIDATED；全目标645 passed / 3 failed（420核心、186/189脚本、39/39其余），包括execution_cli22/22、fd_cli5/5通过。169个源码文件前后SHA-256均为e5038d19e11f8631d81423b876776fac27ab0d40aeabe306f01d48746499cca0。失败仍为job_tests下cancellation_prevents_later_effects_and_wait_reports_observed_cancellation、exit_cancels_and_joins_unfinished_jobs_instead_of_leaving_detached_work、cancellation_crosses_nested_sh_source_and_conditionals_without_running_traps；上版同环境两轮亦失败，保留问题，不提高阈值、不隐藏或跳过测试。627项仍是历史全绿基线。
- 修订：上一条“FD负载下未收口”是bes6zemrr时点的判断；本批FD全过只能证明本批满足原门，不能证明所有负载下保证两秒。联合账本及固定标记协议按专项和真实程序证据独立保存，三个脚本时限、Windows目录同步、Linux原生及断电验收继续列为未完成。
