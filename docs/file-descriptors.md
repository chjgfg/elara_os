# 文件句柄命令

> `fd` 是 Shell/模块桥的薄适配。打开模式、文件对象、IO、代际表均由文件系统实现；这里不复制文件系统业务。

## 1. 入口与模块边界

- 核对日期：2026-11-15。最新完整冻结基线为[596项](filesystem.md#host-rename-2026-11-15)；打开对象身份、Host协作锁与硬链接专项、默认实跑、全目标回归均通过。下文390/408/489项为历史快照，不套用到当前增量。
- [README](../README.md)、[命令账本](ubuntu-style-commands.md)、[脚本专题](shell-scripting.md)、[文件系统专题](filesystem.md)。
- [fd.rs](../src/script/fd.rs)：参数、权限检查、会话所有者、文本令牌与结果报告。
- [modules.rs](../src/script/modules.rs)：把 `fd` 分派给同一个会话的适配器。
- [shell.rs](../src/shell.rs)：会话创建表、`su` 关闭旧能力、交互与脚本入口。
- [功能测试](../tests/script/fd.rs)、[真实进程测试](../tests/fd_cli.rs)；所有新增测试在 `tests/`。

初始化后，在ElaraOS提示符输入`fd help`。fd不执行宿主命令，不操作真实分区/设备或修改宿主系统配置；它确实通过FS后端读写授权数据根里的宿主文件，不能称“完全不碰磁盘”。

## 2. 命令与字面参数

| 命令 | 行为 |
|---|---|
| `fd open PATH MODE [shared\|exclusive]` | 检查当前用户及虚拟路径，调用共享打开入口，通过路径授权、对象权限和归属绑定；成功输出 `SLOT:GEN` |
| `fd read SLOT:GEN COUNT` | 一次顺序读取，返回原始字节，不添加换行；EOF 返回空输出 |
| `fd readat SLOT:GEN OFFSET COUNT` | 从绝对偏移读取，不改变逻辑位置 |
| `fd write SLOT:GEN TEXT` | 一次流式写入，输出实际写入字节数；不是完整文件原子替换 |
| `fd write SLOT:GEN` | TEXT 省略时写入脚本管道/重定向的缓冲输入；只消费实际写入的字节 |
| `fd writeat SLOT:GEN OFFSET [TEXT]` | 定位写入，TEXT 省略时用缓冲输入；不改变逻辑位置，append 模式拒绝 |
| `fd seek SLOT:GEN OFFSET start\|current\|end` | start 为非负 u64，current/end 为有符号 i64；输出新位置 |
| `fd truncate SLOT:GEN LENGTH` | 改文件长度；不改变逻辑位置；只读句柄拒绝 |
| `fd lock SLOT:GEN shared\|exclusive` | 对当前打开描述加非阻塞协作锁，冲突返回LockConflict |
| `fd unlock SLOT:GEN` | 释放该打开描述的锁，dup共享此状态 |
| `fd same LEFT RIGHT` | 比较两个已打开对象，输出SameObject / DifferentObject / Unconfirmed；不重新打开路径 |
| `fd stat SLOT:GEN` | 输出打开时路径、打开模式、位置、长度、会话 owner_uid；受管对象另有 object/generation/uid/gid/mode，旧对象为 LegacyUnknown |
| `fd dup SLOT:GEN` | 新建一个句柄，引用同一文件对象及位置；返回新令牌 |
| `fd sync SLOT:GEN data\|all` | 调用文件系统同步 API；要求可写句柄 |
| `fd close SLOT:GEN` | 关闭本槽；其代际变化，原令牌立即失效 |
| `fd closeall` | 关闭当前任务创建/dup 的句柄引用，输出关闭数；本命令不删除文件；会话/任务结束另走生命周期回收 |

裸 token、多冒号、非 ASCII 数字、负数、溢出均拒绝。`SLOT:GEN` 为本会话表内的定位符，不是可携带到其他会话的授权凭据：不同新表可能恰好同槽同代际。文件系统的完整 `FileHandle` 内部另含表身份，适配器不会从用户输入重造该身份。

### 2.1 打开模式

| MODE | FileAccess | FileCreation | FileBehavior | 注意 |
|---|---|---|---|---|
| `read` | ReadOnly | Existing | Preserve | 不存在报错 |
| `write` | WriteOnly | Existing | Preserve | 不截断、不创建 |
| `rw` | ReadWrite | Existing | Preserve | 不创建 |
| `create` | ReadWrite | Exclusive | Preserve | 只创建新文件；已存在不覆盖 |
| `append` | WriteOnly | IfMissing | Append | 可创建；写入追加；不能 writeat |
| `truncate` | WriteOnly | Existing | Truncate | 已存在文件清零；不存在报错 |

权限与文件系统用途策略都要通过。新文件为当前账号所有；无default ACL时模式为0666 & !会话umask（默认0600），有模板按requested 0666裁剪。gid默认来自持久主组；父目录setgid时改为继承父GID，文件不获得SGID执行身份；目录搜索/共享组/access ACL已接入，全目标严格 Clippy 已通过，当前目录setgid快照390/390通过。旧文件只允许 read，write/rw/append/truncate 不自动认领。read 检查读权限，write 检查写权限，rw/create 检查读与写权限；管理员也不能绕过文件系统的系统只读策略。

## 3. 会话、脚本与身份

- 一次 Shell 运行持有一个 `FdSession`，含 `owner_uid` 和真正的 `HandleTable`。
- 交互fd、sh/source、缓冲管道及作用域`&` worker共用同一真实表，经`SharedModules`的Arc/Mutex序列化；sh和后台环境仍隔离变量/cwd，但不隔离FD能力。子脚本close会关闭外层可见槽，不冒充宿主fork。
- su在改变实际Account前先取消并join所有旧作业、丢弃其私有输出，再关闭旧FD和更新owner_uid。表不重建，代际不回到1；仅改owner标签不能替代关旧能力。
- 即使新用户复用原 root 的槽，原 `SLOT:GEN` 也拒绝；每条 fd 请求还检查 owner_uid 与实际 `Account.uid`。
- 账户转换同时清空原账户脚本变量与函数。修改 `$USER` 不改变实际凭据。
- Shell exit、EOF 或对象 drop 释放拥有的句柄，不删除用户数据；dup 的最后一个引用关闭后才释放文件对象。
- 打开句柄是已授权的能力。以后路径改名，不把旧名字当成新文件重新打开；能力的有效性由文件系统对象和表维护。

### 3.1 受管对象能力（2026-11-09）

- 对象权限在打开时授权。chmod 只影响未来打开；已授予句柄保留打开时的元数据快照和能力，不用当前路径重授予。
- rename 保持 ObjectId，句柄的 opened_path 保留旧名称。replace 保持 ObjectId、递增 storage_generation；普通 UserData 的旧句柄仍读写旧代，不会写进新代，删除名称也不撤销其已经打开的对象。
- **应用/缓存资源另有持久生命周期约束**：每次写入、append、writeat、truncate 都在命名锁下按 ObjectId+generation 检查当前资源和配额；超额在字节和06000特权位改变前拒绝。资源处于 Removing/Evicting 返回 ResourceInactive；资源已退休或FD指向替换前旧代返回 ResourceDetached。旧FD仍可读取原打开对象，但不能以其继续消耗已释放资源的写入额度。该例外不改变普通 UserData 的旧代写入语义。
- cat、wc、输入重定向、流式来源、脚本文件、stat/find/du 和 glob 的受管读取使用账号入口；不能让普通用户经另一个命令绕开对象权限。
- cp 的目标由发起者拥有（应用会话 umask），不克隆源所有者；mv 对受管普通文件调用归属事务。裸入口仍拒绝受管目录整体移动；账号目录 mv 经联合目录/文件 catalog 意图提交，不先改宿主目录留下失效归属。当前验收状态见[四项文件系统增量](filesystem.md#directory-access-task-2026-11-09)。
- 多会话验证由 tests/fd_cli.rs 的 real_fd_ownership_modes_generations_and_legacy_guards_survive_multiple_sessions 覆盖，结果以文件系统单一账本为准。

## 4. 读写限制与错误结果

- read/readat 的桥接上限为 **65536 字节**，低于文件系统单次 IO 的 1 MiB 上限。请求超限在 IO 前拒绝，不推进位置后再丢弃输出。
- TEXT写入由FS单次IO限额检查；FD管道当前用显式64KiB缓冲回退，不把适用cat/wc阶段的流式能力外推到fd。输出重定向也保留原FS受管接口，不自动改成大文件流式提交。
- 流式 write/writeat 只调用一次，报告实际 count，短写不自行重试，也不冒充原子 replace。
- 若文件系统报 `WritePositionUnconfirmed { bytes, error }`：stdout 仍报告实际 bytes，状态非零，stderr 说明位置不确定且没有重试。该能力仍存在，可用 stat 查看；相对/顺序 IO 被文件系统拒绝，直到 start/end seek 恢复位置。
- 同步要求可写，是文件系统约定；不因 Windows 只读 flush 失败而伪造成功。
- 错误后外层 Shell 继续可用。关闭、满表、模式错误、陈旧令牌均给出实际错误，不回退成另一个文件或重开能力。
- 满表检查由文件系统在 open 的创建/截断副作用前实施。
- 系统日志记录 `fd` 操作名，不写入 TEXT、管道内容或读取内容。

## 5. 可运行示例

在新的会话中：

```text
fd open /tmp/example create
```

按打印出的实际 token（以下假设为 `0:1`，不要在已有表中盲用）：

```text
fd write 0:1 "hello world"
fd seek 0:1 0 start
fd read 0:1 11
fd stat 0:1
fd dup 0:1
```

read 返回原始数据，没有自动换行；直接终端显示时下一行可能接在字节后。若在脚本中处理：

```text
sh -c 'fd seek 0:1 0 start; fd read 0:1 11 | wc -c'
```

注意 seek 自身输出 `0`，其后的管道输出 `11`，不是把 seek 输出也传入 read 管道。所有使用完成后再关闭：

```text
fd closeall
```

关闭之后这些 token 已失效；下一次使用必须重新 open 并读取新 token。

## 6. 功能 → 测试

| 功能 | 测试 |
|---|---|
| 开/读/写/seek/stat/truncate/sync | `open_stream_io_seek_stat_truncate_and_sync_delegate_to_real_handles` |
| dup 共享位置、槽复用和 closeall | `duplicate_shares_position_and_generation_reuse_rejects_stale_tokens` |
| 定位 IO、append 拒绝定位写 | `positioned_io_preserves_position_and_append_rejects_writeat` |
| 模式与权限失败不回退 | `readonly_writeonly_and_open_modes_have_no_silent_permission_fallback` |
| 满表不截断、超限 read 不推进 | `full_table_does_not_truncate_target_and_read_limits_do_not_consume_data` |
| 参数与令牌校验 | `malformed_tokens_offsets_and_requests_are_rejected` |
| root 能力关闭、普通用户同槽旧 GEN 拒绝 | `open_authorization_and_owner_transition_prevent_root_capability_inheritance` |
| 文档示例按顺序连续照敲 | `documented_command_sequence_keeps_handles_live_until_last_use`；所有读/脚本使用之后才 closeall，不复用已关闭 token |
| 只改 owner 标签而不关闭旧能力必须失败 | `owner_reset_closes_capabilities_instead_of_only_changing_the_uid_label`；在内存摘门后测试按预期红，生产源码未修改 |
| 部分已写结果不丢失、不重试 | `partial_write_reporting_preserves_progress_and_consumption_without_retry` |
| sh/source 共用表 | `shell_scripts_share_the_table_and_fd_write_does_not_leak_into_logs` |
| 写入正文不进系统日志 | `fd_write_text_is_not_copied_into_system_logs` |
| 超 1 MiB 写入无副作用 | `oversized_stream_writes_are_rejected_before_any_bytes_are_written` |
| close/drop 不删除用户文件 | `close_and_drop_release_handles_but_do_not_delete_user_data` |
| 动态获取 token 的真实进程、su 同槽防继承 | `real_fd_commands_share_handles_and_user_switch_closes_root_capabilities` |

部分写入错误的适配测试构造文件系统已公开的错误结果，只验证报告映射；文件系统的内部位置失败故障注入仍由文件系统测试负责，不宣称适配器独自复现全部 IO 故障。

## 7. 验收

验证命令：

```powershell
cargo test --offline --target-dir target/script-build --test script_language fd_tests
cargo test --offline --target-dir target/script-build --test fd_cli -- --nocapture
```

2026-11-08 历史完成：`tests/script/fd.rs` 的 **14 项功能测试全部通过**，`tests/fd_cli.rs` 的真实进程测试通过；动态获取 token 后验证共享位置、0:1→0:2 同槽拒旧代、su 关闭 3 个旧能力、读权限拒绝及无写入正文泄漏。文档示例增加连续执行回归，closeall 只在最后使用之后。

必要性对照：仅在内存移除 `switch_owner` 的 close_all，改 uid 但保留 root 能力，`owner_reset_closes_capabilities_instead_of_only_changing_the_uid_label` 按预期失败；生产源码未更改。对照编译曾缺少测试包装模块和 Windows 原生链接路径，修复验证环境后成功执行，不把编译失败当作守卫有效证据。

FD阶段历史190、M4历史275当时通过且严格Clippy通过。2026-11-08 稳定共享历史快照为**169+145+6=320测试通过**，FD14包含于145；M5另验证后台/嵌套共用真实FD与身份切换先join旧作业。**这是 2026-11-08 的历史 Clippy 结论，2026-11-09 已等价修复既有七项诊断；最新全量及严格检查结果转由[文件系统账本](filesystem.md#owned-open-2026-11-09)维护。**统一计数/源码哈希/质量门由[脚本验收账本](shell-scripting.md#8-验收记录)维护，不叠加旧批次，不声称Linux、真实断电或全部敌对并发已验。

## 8. 修订记录

| 日期 | 修订 |
|---|---|
| 2026-11-09 | fd 改接 open_as 并显示受管对象/存储代/uid/gid/mode；补充跨命令读权限、chmod 授权快照、rename/replace/删除后旧句柄、多会话与旧文件拒绝认领；包含受管后代的目录整体移动仍拒绝，不提前宣称目录/ACL/任务生命周期完成。 |
| 2026-11-08 | M5补充真实共享FD资源与作用域worker、身份切换先cancel/join再关旧能力；FD管道明确64KiB回退，不外推所有命令流式。纠正“不操作真实磁盘”为不操作分区/设备；320测试通过，当前FS三处Clippy未过，统一账本注明。 |
| 2026-11-08 | 接入 fd 薄适配与 owner 绑定表；共享交互/脚本能力，身份转换关闭旧句柄保留代际；明确 IO 上限、部分写报告、token 局部作用域和不删除数据；新增 tests 功能与真实进程验收。 |

## 本轮任务作用域引用（严格 Clippy 已通过，当前目录setgid快照390/390通过）

宿主文件锁/名称删除只影响根内授予的对象；虚拟权限不映射成宿主提权或权限修改。完整边界见[宿主影响硬边界](filesystem.md#host-impact-boundary-2026-11-09)。

fd open/dup 使用会话 umask 与根/账号绑定的任务租约；结束钩子只释放该 TaskScopeId 创建的引用，明确转交到别的任务的引用仍是有效能力。su/会话退出先 join，再关闭旧表及回收新 /tmp 名称；home/root 数据与既有 OwnerRequest 对象不自动删除。纯 fd close 不触发文件删除。零模式新建可授予本次 FD，未来重开仍受权限限制。权威状态及尚未验证边界见[文件系统四项增量](filesystem.md#directory-access-task-2026-11-09)，不沿用历史测试计数。

FD 新建的 default ACL 规则见[第一层继承增量](filesystem.md#default-acl-2026-11-09)：有父模板时用 requested 0666 限制 inherited ACL，不额外扣会话 umask；完整 ACL 在名称公开前持久化且可恢复。无模板保留原 umask 规则，既有对象不重设权限，零权限创建 FD 仍是本次已授予能力。default ACL专项15/15、真实CLI2/2通过，当前目录setgid快照390/390通过；完整证据只引用权威账本。

目录setgid语义及验收、历史sticky阻塞引用[目录SGID增量](filesystem.md#directory-setgid-2026-11-09)；新增sticky及命名后旧FD存活见[当前增量](filesystem.md#sticky-2026-11-09)：父GID继承与组成员资格分开，已有FD能力及TaskExit条件不变。不是setuid/file SGID执行实现，不修改宿主账号或权限。


## 虚拟执行身份与内容修改

`exec`与登录账号切换`su`不同：FD表仍绑定真实会话UID，保留既有能力；新打开使用映像的有效凭据，exec任务结束只释放该租约创建的引用。普通`su`仍关闭旧表并保持token代际隔离。非空`write/writeat`、`truncate`、打开截断会先持久清文件06000；旧代FD不影响替换后的新代。`info`中的归属是打开快照，不是实时权限查询。协议、代价、故障恢复和本批验收统一见[执行身份与写入清位](filesystem.md#execution-identity-2026-11-14)；上文390项和目录SGID说明属于历史快照。

<a id="vfs-fd-2026-11-14"></a>
## 内部卷 FD：共享打开、锁与目录租约

```text
fd open /mnt/work/file rw shared
fd open /mnt/work/file rw
fd lock 0:1 exclusive
fd read 1:1 4
fd unlock 0:1
fd read 1:1 4
fd closeall
```

上例第一处 read 明确 LockConflict，unlock 后 read 才可成功；示例 token 仅适用于此前没有打开 FD 的表，实际必须使用 open 返回值。

- `fd open PATH MODE [shared|exclusive]`：shared 默认；exclusive 与任何其他独立 open 互斥。dup 共享原打开描述，不是新的独立 open。
- `fd lock TOKEN shared|exclusive` / `fd unlock TOKEN`：共享锁允许其他读但禁止其他写；独占锁阻止其他描述读写。独占锁要求可写 FD；冲突非阻塞返回 LockConflict。
- dup 的最后一个引用关闭才释放锁；unlink/覆盖 rename 后旧 FD 仍指向旧 inode。
- 2026-11-15修订：受管Host已接持久协作租约和exclusive共享策略，原21项专项、真实双进程及默认程序实跑通过；后续同字节租约替换对照发现缺陷，已升级双见证身份和V3出生协议，连同按冲突扫描27项专项通过，默认程序复测亦通过，当前全目标560项通过，严格Clippy与默认构建通过；未知归属Host仍Unsupported。租约不占用payload身份探针，不是阻止外部宿主程序原始IO的OS强制锁。
- 内存卷 sync_data/sync_all 返回 Unsupported；vfs save 才是显式镜像持久化入口，且必须检查 durability。
- DirectoryCursor 在内存卷同样 pin inode；删除目录后同名重建不改变旧 cursor，refresh 仍观察原对象。最后游标关闭前卸载 Busy。
- 卷内生成信息每次 open 捕获正文，dup 共享该正文和 cursor；zero/full 每次读的数量受 IO 上限约束，不分配无界缓冲。

源码及完整证据统一见[文件系统账本](filesystem.md#internal-vfs-2026-11-14)。

### 2026-11-15 增量

- 挂载卷 FD 写入使用 4KiB 稀疏数据页，快照共享页但修改互不影响；跨页写只 detach 涉及的页，失败配额检查不提交新页或权限位变化。
- `vfs stats` 的 allocated_data_bytes 不含页表和其他元数据；shared_data_pages 是引用计数观测，不能跨卷直接相加作为总物理内存。
- 已存在的受管 RuntimeState 文件可通过普通账号 open 获取句柄；新建必须具备有效任务 scope，scope 不能跨 DataRoot 或账号复用。
- RuntimeState 仍复用 Host 后端，打开对象阻塞删除时回收明确 deferred；不声称宿主文件拥有内存卷的完全 unlink 语义。

### 挂载目录引用补充（2026-11-15）

文件 FD 之外，工作目录 search-only 租约、嵌套挂载父目录租约，以及列出子卷的目录快照也会阻止相关卷卸载。目录读游标仍可观察被 unlink 的旧 inode；cwd 则选择明确 Busy 的命名保护，不允许被同名目录替换重定向。两种能力不混同。具体边界与专项证据见[命名空间账本](filesystem.md#namespace-2026-11-15)。

### 已打开对象身份

`fd same LEFT RIGHT`使用实际输出的SLOT:GEN；结果为SameObject、DifferentObject或Unconfirmed。旧FD在rename/replace/unlink后不重新绑定到原路径；同字节不是同一对象，两个不同内部卷的同号inode也不是同一对象。Host只读独立FD或外部锁可能使探针Unconfirmed，此时不能作相同/不同断言。见[身份验收](filesystem.md#opened-identity-2026-11-15)；不是硬链接创建命令。

- 2026-11-15 Host硬链接增量：同一受管对象的登记名称共享inode元数据/ACL/协作锁。FD写修改共享payload；atomic replace切换全部登记名称到新代，旧FD保持旧代；最后unlink不撤销已打开能力。27项专项及真实双进程通过，迁移后573项全量通过，详见[硬链接账本](filesystem.md#host-hardlinks-2026-11-15)。

- Host虚拟链接增量[15项专项及默认程序实跑通过](filesystem.md#host-symlinks-2026-11-15)，586项完整回归通过：普通open在同一命名锁内解析并固定实际目标对象，Exclusive保留最终名称不跟随，旧FD不随链接重命名或目标replace迁移；先前573项冻结证据不覆盖本次源码。

- Host覆盖rename已通过[37项相关测试和真实双进程](filesystem.md#host-rename-2026-11-15)：目标名称转指源inode，旧目标FD与其他硬链接保持原对象；同inode操作不增修订。目录仅覆盖受管空目标，子树已开文件FD保持能力。596项完整回归和默认实跑通过，未声称所有Host cwd生命周期已转为inode固定。
