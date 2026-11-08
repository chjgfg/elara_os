# 文件句柄命令

> `fd` 是 Shell/模块桥的薄适配。打开模式、文件对象、IO、代际表均由文件系统实现；这里不复制文件系统业务。

## 1. 入口与模块边界

- 日期：2026-11-08。
- [README](../README.md)、[命令账本](ubuntu-style-commands.md)、[脚本专题](shell-scripting.md)、[文件系统专题](filesystem.md)。
- [fd.rs](../src/script/fd.rs)：参数、权限检查、会话所有者、文本令牌与结果报告。
- [modules.rs](../src/script/modules.rs)：把 `fd` 分派给同一个会话的适配器。
- [shell.rs](../src/shell.rs)：会话创建表、`su` 关闭旧能力、交互与脚本入口。
- [功能测试](../tests/script/fd.rs)、[真实进程测试](../tests/fd_cli.rs)；所有新增测试在 `tests/`。

初始化后，在ElaraOS提示符输入`fd help`。fd不执行宿主命令，不操作真实分区/设备或修改宿主系统配置；它确实通过FS后端读写授权数据根里的宿主文件，不能称“完全不碰磁盘”。

## 2. 命令与字面参数

| 命令 | 行为 |
|---|---|
| `fd open PATH MODE` | 检查当前用户及虚拟路径，调用 `HandleTable::open`；成功输出 `SLOT:GEN` |
| `fd read SLOT:GEN COUNT` | 一次顺序读取，返回原始字节，不添加换行；EOF 返回空输出 |
| `fd readat SLOT:GEN OFFSET COUNT` | 从绝对偏移读取，不改变逻辑位置 |
| `fd write SLOT:GEN TEXT` | 一次流式写入，输出实际写入字节数；不是完整文件原子替换 |
| `fd write SLOT:GEN` | TEXT 省略时写入脚本管道/重定向的缓冲输入；只消费实际写入的字节 |
| `fd writeat SLOT:GEN OFFSET [TEXT]` | 定位写入，TEXT 省略时用缓冲输入；不改变逻辑位置，append 模式拒绝 |
| `fd seek SLOT:GEN OFFSET start\|current\|end` | start 为非负 u64，current/end 为有符号 i64；输出新位置 |
| `fd truncate SLOT:GEN LENGTH` | 改文件长度；不改变逻辑位置；只读句柄拒绝 |
| `fd stat SLOT:GEN` | 输出打开时路径、打开模式、当前位置、长度和会话 owner_uid |
| `fd dup SLOT:GEN` | 新建一个句柄，引用同一文件对象及位置；返回新令牌 |
| `fd sync SLOT:GEN data\|all` | 调用文件系统同步 API；要求可写句柄 |
| `fd close SLOT:GEN` | 关闭本槽；其代际变化，原令牌立即失效 |
| `fd closeall` | 关闭当前表的所有句柄，输出关闭数，不删除任何文件 |

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

权限与文件系统用途策略都要通过。read 检查读权限，write 检查写权限，rw/create 检查读与写权限；管理员也不能绕过文件系统的系统只读策略。

## 3. 会话、脚本与身份

- 一次 Shell 运行持有一个 `FdSession`，含 `owner_uid` 和真正的 `HandleTable`。
- 交互fd、sh/source、缓冲管道及作用域`&` worker共用同一真实表，经`SharedModules`的Arc/Mutex序列化；sh和后台环境仍隔离变量/cwd，但不隔离FD能力。子脚本close会关闭外层可见槽，不冒充宿主fork。
- su在改变实际Account前先取消并join所有旧作业、丢弃其私有输出，再关闭旧FD和更新owner_uid。表不重建，代际不回到1；仅改owner标签不能替代关旧能力。
- 即使新用户复用原 root 的槽，原 `SLOT:GEN` 也拒绝；每条 fd 请求还检查 owner_uid 与实际 `Account.uid`。
- 账户转换同时清空原账户脚本变量与函数。修改 `$USER` 不改变实际凭据。
- Shell exit、EOF 或对象 drop 释放拥有的句柄，不删除用户数据；dup 的最后一个引用关闭后才释放文件对象。
- 打开句柄是已授权的能力。以后路径改名，不把旧名字当成新文件重新打开；能力的有效性由文件系统对象和表维护。

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

本轮完成：`tests/script/fd.rs` 的 **14 项功能测试全部通过**，`tests/fd_cli.rs` 的真实进程测试通过；动态获取 token 后验证共享位置、0:1→0:2 同槽拒旧代、su 关闭 3 个旧能力、读权限拒绝及无写入正文泄漏。文档示例增加连续执行回归，closeall 只在最后使用之后。

必要性对照：仅在内存移除 `switch_owner` 的 close_all，改 uid 但保留 root 能力，`owner_reset_closes_capabilities_instead_of_only_changing_the_uid_label` 按预期失败；生产源码未更改。对照编译曾缺少测试包装模块和 Windows 原生链接路径，修复验证环境后成功执行，不把编译失败当作守卫有效证据。

FD阶段历史190、M4历史275当时通过且严格Clippy通过。最新稳定共享快照为**169+145+6=320测试通过**，FD14包含于145；M5另验证后台/嵌套共用真实FD与身份切换先join旧作业。**当前严格Clippy未通过，FS三处lint仍未修**。统一计数/源码哈希/质量门由[脚本验收账本](shell-scripting.md#8-验收记录)维护，不叠加旧批次，不声称Linux、真实断电或全部敌对并发已验。

## 8. 修订记录

| 日期 | 修订 |
|---|---|
| 2026-11-08 | M5补充真实共享FD资源与作用域worker、身份切换先cancel/join再关旧能力；FD管道明确64KiB回退，不外推所有命令流式。纠正“不操作真实磁盘”为不操作分区/设备；320测试通过，当前FS三处Clippy未过，统一账本注明。 |
| 2026-11-08 | 接入 fd 薄适配与 owner 绑定表；共享交互/脚本能力，身份转换关闭旧句柄保留代际；明确 IO 上限、部分写报告、token 局部作用域和不删除数据；新增 tests 功能与真实进程验收。 |
