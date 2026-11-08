# ElaraOS Shell 脚本

> 宿主原型的语言与执行子集：词法 → AST → 受限求值 → 实际模块接口。不是完整 Bash，不启动宿主 Shell 或外部可执行程序。

## 1. 入口与职责

- 建立日期：2026-11-07；最近核对：2026-11-08。
- [README](../README.md)：工程入口；[命令账本](ubuntu-style-commands.md)：命令可用性单一账本；[模块总览](operating-system-modules.md)：§19 Shell、§9 程序接口、§10 IPC、§13 文件系统。
- [script/mod.rs](../src/script/mod.rs)：选项、授权读取、UTF-8 验证和脚本入口。
- [lexer.rs](../src/script/lexer.rs)、[parser.rs](../src/script/parser.rs)：保留引用/转义信息、完整程序预解析、递归下降 AST。提示符与脚本共用前端。
- [arithmetic.rs](../src/script/arithmetic.rs)、[expansion.rs](../src/script/expansion.rs)：检查算术与命令/参数展开；解析深度显式传递，不使用可变线程全局计数。
- [pattern.rs](../src/script/pattern.rs)、[globbing.rs](../src/script/globbing.rs)：有界模式匹配、实际目录游标、逐路径授权。
- [runtime.rs](../src/script/runtime.rs)：环境、控制流、展开、输入消费、状态、流式策略与显式缓冲回退。
- [pipe.rs](../src/script/pipe.rs)、[streaming.rs](../src/script/streaming.rs)、[count.rs](../src/script/count.rs)：有界字节队列、并发阶段、逐块文件读取；流式/缓冲 wc 共用计数器，printf 共用格式器。
- [control.rs](../src/script/control.rs)、[jobs.rs](../src/script/jobs.rs)：合作式暂停/取消、实际 ACK、作用域作业、等待与 join。不是宿主信号或内核调度器。
- [modules.rs](../src/script/modules.rs)、[shared.rs](../src/script/shared.rs)：模块桥与真实资源共享；不建立空的假任务表或假 FD 表。
- [fd.rs](../src/script/fd.rs)：句柄薄适配；模式、代际、IO 进度与身份边界见[句柄专题](file-descriptors.md)。
- [shell.rs](../src/shell.rs)：提示符、认证 UI、身份切换与结束作用域；[src/lib.rs](../src/lib.rs)：真实二进制和测试共用模块库。
- [功能测试入口](../tests/script_language.rs)、[真实进程测试](../tests/script_cli.rs)：测试调用实际实现；所有新增功能测试在 `tests/`。

**分工边界**：Shell 不重写文件系统业务。访问经过 `VirtualPath`、实际 `Account` 的权限规则和 `DataRoot`；写入分类、对象权限、原子提交、目录持久性与恢复以[文件系统专题](filesystem.md)为准。共享的是已授予的同一个 DataRoot，不重新 ambient 打开目录冒充继承能力。

## 2. 初始化后可运行的入口

先在项目目录运行 `cargo run`。以下输入到 ElaraOS，不是宿主 PowerShell：

```text
sh -c 'echo hello; false || echo recovered'
sh /tmp/demo.sh world
sh -n /tmp/demo.sh
source /tmp/context.sh
. /tmp/context.sh
```

- `sh FILE [ARGS...]`：授权读取虚拟根内 UTF-8 文件；`$0` 是传入路径，`$1` 起为参数。
- `sh -c TEXT [NAME [ARGS...]]`：NAME 是 `$0`，缺省 `sh`。
- `sh -n FILE`：解析整份程序，不执行、不改变变量、目录或文件。
- `source` / `.` 也接受 `-c/-n`；变量、函数和目录变化保留，包括错误前已经成功的变化。无参数继承调用者位置参数；有参数临时替换 `$1...`，保留调用者 `$0`；结束后恢复参数。
- `sh` 隔离变量、函数、参数和目录变化；真实 FS、任务、FD、日志资源仍共享。子脚本 `fd close` 会关闭外层可见句柄。
- 提示符可直接输入赋值、if/for/while/case、函数、重定向、管道和 `&`。目前每次仍要求完整单行程序，未实现多行续行 UI。
- 提示符 `exit [N]` 结束程序并返回 0–255；省略 N 使用 `$?`。管道阶段的 exit 不关闭调用者；sh 的 exit 只结束子脚本；source 的 exit 结束当前调用环境，return 只结束 source/函数调用。
- `#!/bin/sh` 按注释处理，不启动宿主 `/bin/sh`，也不支持直接执行 `./script.sh`。

## 3. 已实现的语言子集

| 类别 | 可用语法 | 边界 |
|---|---|---|
| 分词 | 空格、Tab、LF、单/双引号、反斜杠、相邻词段、空引号 | `#` 仅在词首开始注释；单引号不展开；保留转义字面的属性 |
| 文件编码 | UTF-8、LF/CRLF | CRLF 归一为 LF；诊断偏移按归一文本的 UTF-8 字节，无效 UTF-8/控制字符拒绝 |
| 变量 | `X=value`、`export X=value`、`unset X`、`local X=value` | 赋值不分词；local 仅函数可用，结束后恢复；set -u 区分未定义 |
| 参数 | `$X`、`${X}`、位置参数、`$#/$?`、`"$@"`、`${#X}` | 长度按 Unicode 字符；`$@` 必须独占整词 |
| 参数修饰 | `${X-word}`、`${X:-word}`，以及 `= / + / ?` | 区分 unset/空值、有无冒号；未用分支不求值 |
| 截取/替换 | `${X#pattern}`、`## / % / %%`、`${X/old/new}`、`${X//old/new}` | 前后缀支持引用感知 wildcard；替换仅字面模式，不是完整 Bash 模式替换 |
| 命令/算术展开 | `$(commands)`、`$((n + 1))` | 实际解释器，不是宿主 eval；命令替换去尾部 LF，输出要求 UTF-8 且无 NUL；算术为检查溢出的 i64 十进制、变量、单目、四则/余数、比较与逻辑短路 |
| 字段/路径展开 | IFS、`* / ? / [a-z] / [!a]`、`~ / ~/path` | 仅裸展开分词/glob；隐藏项须显式点开头，未匹配保留字面；不支持 ~user |
| 顺序/条件 | 换行、`;`、`&&`、`||` | 条件按状态 0/非零；管道/连接符后可换行 |
| 分支 | `if ...; then ...; elif ...; else ...; fi` | 关键字仅在语法位置有效 |
| 循环 | for（有/无 in）、while、until | 无 in 遍历参数；共用执行预算 |
| 函数 | `name() { ...; }`、调用、return、local | 参数/局部变量恢复；内建/系统命令保留，递归有限 |
| case | `case WORD in p1\|p2) commands ;; *) commands ;; esac` | 首个匹配分支；引用影响模式；无 `;& / ;;&` |
| 选项/退出 | `set -e/-u/+e/+u`、`trap 'commands' EXIT`、`trap - EXIT` | -e 区分条件位置；正常 exit/EOF 执行 EXIT 一次；已观察的合作式取消不再执行 handler 的模块效果 |
| 控制 | break、continue、exit/return `[0..255]` | 无层数参数；错误上下文失败 |
| 重定向 | `< / > / >>` | 一输入一输出；目标先验证/打开；输出重定向使用缓冲回退，非多文件事务 |
| 管道 | `a | b` | 适用阶段真实并发流式，其余显式有界缓冲；见 §3.2 |
| 后台/作业 | `program &`、jobs、bg、fg、wait、kill | 解释器作用域 worker，%ID；非宿主进程组/信号；见 §3.3 |

### 3.1 内建与模块命令

语言内建：`:`、true、false、echo、printf、pwd、cd、whoami、cat、wc、ls、mkdir、touch、rm、rmdir、export、unset、local、set、trap、test、`[`、exit、return、break、continue、sh/source/`.`、jobs/fg/bg/kill，以及用户函数。模块桥接入 ps、sleep、wait、users、log、fd、mv/cp/stat/find/du；对象归属新增桥的进度以文件系统账本为准，不把它算作 Shell 自行实现的 FS 业务。

- printf：`%s/%d/%%`、`\n/\t/\\`，按参数重复格式；无宽度、精度等扩展。
- wc：`-c/-l/-w`；缺省字节数，只有计数输出，不是 GNU 全格式。逐块保持跨块/文件的单词状态。
- cat：原始字节，不补换行；无文件参数使用当前阶段输入，不读取外层提示符/密码。最终 stdout 与缓冲路径仍限 64 KiB；适用流式管道的中间文件数据可超过此值。
- cd 无参数使用 HOME 初值，仍按实际账户授权；修改 USER 不改变身份。
- touch 已存在报错，不更新时间戳；多路径按顺序执行，不是事务；没有递归删除。
- test：空/非空、`! / -n / -z / -f / -d`、字符串 `= / !=`、整数 `-eq/-ne/-lt/-le/-gt/-ge`。
- `help/info/clear/su/passwd/useradd` 仍是独立提示符 UI 控制，静态复合调用整体预拒绝；动态名字也不能取得认证输入。

### 3.2 真正流式管道与回退

**流式阶段仅限** echo、printf、cat、wc、pwd、whoami、true、false、`:`。名字必须静态可识别，参数为允许的简单展开；命令替换/复杂参数展开、模块命令/函数、输出重定向等不走此路径。输入重定向可以流式。

```text
cat /tmp/large | wc -c
printf '%s\n' a b | wc -l
```

- 每阶段真正并发，队列有背压、EOF 和读端关闭唤醒；不是先收集整个中间 stdout 再执行下一阶段。
- 文件源调用真实 `OpenFile`，以 READ 模式逐次最多 4096 字节读取；打开时 length 是该次读取上限，后续追加不会导致永不 EOF。这不是内容一致性快照，已有范围内并发修改仍按后端语义。
- 末阶段状态为管道状态；`StageOutcome::Exited/Panicked/NotStarted` 保留每阶段结果。上游读端早关兼容状态 141，取消兼容 130，但不表示发送了真实 SIGPIPE/信号。
- `Report.mode` 明确 Buffered / Streaming / Mixed；每条多阶段管道的 `Trace` 保存 `Strategy`、阶段状态、队列真实统计和继承输入实际消费量。
- 失败返回 `Completion::Failed { error, partial }`，保留已收集输出、阶段状态与消费进度；**不重放已经消费的父 stdin**。队列统计不可读使用 `QueueSnapshot::Unavailable`，不伪造零进度。

| BufferedReason | 回退原因 | 实际代价/边界 |
|---|---|---|
| InterpreterCommand | 模块命令、函数或非静态内建名 | 顺序解释；中间数据最多 64 KiB；仍使用真实模块 |
| ComplexExpansion | 命令替换或复杂参数展开 | 不预先求值后再重放，保持一次性效果；顺序缓冲 |
| ExpansionPreflight | 简单参数准备失败 | 转原解释器给出既有诊断；未启动流式 worker |
| AtomicOutputRedirect | `>` 或 `>>` | 保留现有 FS 接口与 64 KiB 子集；不伪装为大文件流式原子提交 |
| StageQuota | 超过 16 阶段 | 原有顺序有界解释，不超额启动线程 |
| WorkerQuota | 进程总池/Utility 名额不足 | 不等待抢占前台名额，显式回退；大输出可能因 64 KiB 上限失败 |
| WorkerPoolPoisoned | 池锁不可用 | 明确回退原因，不宣称资源预留成功 |

流式取消返回 **ConnectionAborted** 等不可重试中止；不能使用 Interrupted，`Write::write_all` 会自动重试 Interrupted 并可能永远不退出。

### 3.3 作用域作业与身份生命周期

```text
sh -c 'echo background; exit 3' &
jobs
wait %1
echo $?
sleep 1 &
kill -STOP %2
jobs
bg %2
fg %2
```

这里 %1/%2 只适用于新会话，实际使用按打印 ID：

- `&` 可包裹一条连接链/复合节点；worker 环境副本隔离变量、函数、参数和 cwd，真实模块资源共享。后台 stdin 为 EOF，不抢提示符或密码输入。
- stdout/stderr 有界收集，wait/fg 合并返回；未 wait 的输出保留在作业记录，不自动丢弃或打印完整脚本文本。
- jobs 显示 Running/PauseRequested/Stopped/Resuming/Cancelling/Cancelled/Exited/Panicked。Stopped 必须所有已注册 worker 实际 ACK；Cancelled 必须 worker 实际观察取消，晚到请求不改写已完成成功。
- `bg %ID` / `kill -CONT %ID` 请求恢复；`fg %ID` 恢复并等待；`wait %ID` 等待、取结果、移除记录并 join。wait/fg 回收后旧 ID 失败；不能等待自身。
- `kill [-STOP|-CONT|-TERM|-INT] %ID` 仅合作式控制本解释环境，不操作宿主 PID。暂停时间不计入 sleep 的活动等待时间。
- 完成而未回收也占槽，总记录最多 16；容量/身份/closing/ID 溢出门在 module fork 与 worker 创建前。ID checked_add，不 wrap，不在身份切换时复用。
- `ModuleFork::Ready/Unsupported` 明确是否支持共享工作上下文。fork 只能非阻塞克隆能力，不执行脚本/IO、不重入 registry；拒绝不发布假作业。
- 一个提示符程序中的同步调用和 `&` 后代共用同一个 `Arc<Budget>`，总计 4096 步；新提示符程序创建新 invocation。不能用递归 `&` 重置预算。
- Shell exit、EOF、IO 错误先取消并 join 全部作用域作业。su 在改变实际 Account 前先 join 旧作业、关闭旧 FD（保留代际）、丢弃旧私有输出并清 Context；不能只改 owner 标签。
- 旧 `sleep N bg / ps / wait ID` 仍是 TaskTable 接口；数字 ID 与作用域 `%ID` 是两个命名空间。后台 worker 不允许创建 detached 旧计时器或阻塞等待旧 TaskTable；应使用作用域 `& / wait %ID`。这不等于旧任务模块已经拥有一般进程调度能力。

## 4. 跨模块命令契约

`ModuleCommands::execute(Request)` 是稳定入口，后续模块不需要改词法和 AST：

- Request：命令名、已展开参数、虚拟 cwd、实际缓冲 stdin，不传宿主 Shell 指令字符串。
- Dispatch：Handled(CommandOutput) / Unsupported，执行失败另用 Result；不以 Option 混掉失败与未注册。
- CommandOutput：status、stdout、stderr、InputUse::Untouched/Consumed(N)。解释器验证消费量并真正推进输入。
- SystemModules 直接调用真实模块；SharedModules 使用同一任务/FD/log 资源的 Arc/Mutex，共用授予的 DataRoot 和实际 Account。固定 tasks→files 锁顺序；受控 sleep/旧 wait 不持有 FD 锁。
- 文件写入仍受 FS 类别/对象权限/事务规则约束，运行时错误不是整个脚本回滚。

## 5. 明确未支持

完整 POSIX/Bash、heredoc、子Shell分组、stderr管道、数字 FD 重定向、多重同方向重定向、数组、算术赋值/自增/位运算、完整模式替换、非 EXIT trap、eval/exec、外部程序/ELF 装载、内核 ABI/进程组/抢占式调度、多行续行、history/Tab/行编辑、完整文件名行列诊断。

- 不支持的语法整份预解析拒绝，前面命令不会先执行。未注册命令在求值时状态 127；默认无 errexit，后续仍可执行。
- IFS 空值不切分，未设为默认空格/Tab/LF；非空白分隔符保留中间空字段；不影响语法操作符或安全身份。
- 真实并发仅限 §3.2 的阶段；作用域线程不是独立进程地址隔离或 OS 安全沙箱。

## 6. 资源与安全契约

| 项目 | 上限/规则 |
|---|---|
| 文本 / token | 64 KiB / 4096 |
| 解析/展开/总调用深度 | 各 32 层；函数、sh、source 共用调用深度 |
| 执行预算 | 每 invocation 4096 步，同步及异步后代共享 |
| 最终 stdout / stderr / 缓冲管道 / 展开词 | 各 64 KiB；不是流式中间数据总量上限 |
| 流式队列 | 块 4096 字节、排队 8 块，每队列 payload 最多 32768 字节；端点 pending/argv/输出等另外有界，**不是整个进程 RSS 上限** |
| stream worker 池 | 单宿主进程最多 16，Utility 最多 8；Foreground/Utility 名额策略，不是宿主线程优先级或内核 QoS |
| 作业记录 | 每 Shell 作用域最多 16，完成未 wait 也占槽 |
| 参数 / 提示符输入 | 参数最多 1024/64 KiB；输入 64 KiB，超长/非法 UTF-8 整行排空后拒绝 |
| 变量与保存绑定 | 最多 256，键值及保留 local/临时旧值合计 64 KiB；恢复不能越过预算 |
| 函数 | 最多 64 |
| glob | 快照 4096 项、页 256；总枚举 4096，结果 1024/64 KiB |
| 模式 | 4096 字节，匹配最多 100000 步；截取/替换另计共享执行预算 |
| 权限/审计 | 实际 Account；修改 USER 无效；日志只记操作/状态/步数/模式，不记脚本、参数/FD 正文或密码 |

语法错误/-n 无求值副作用；运行时错误不会回滚已经成功的效果。原子可见性、目录持久性和提交后清理不能混称“已持久化”。

## 7. 示例与复现

[shell-demo.sh](examples/shell-demo.sh) 为文档示例。真实 CLI 测试将它部署到独立测试根，不修改 `.env` 指向的数据。参数 world 的输出：

```text
星澜脚本
hello Alice
hello Bob
arg=world
3
```

项目目录验收命令：

```powershell
cargo test --offline --target-dir target/shell-build --test script_language -- --test-threads=1
cargo test --offline --target-dir target/shell-build --test script_cli -- --nocapture
cargo test --offline --target-dir target/shell-build -- --test-threads=1 --nocapture
cargo clippy --offline --target-dir target/shell-build --all-targets -- -D warnings
cargo run --offline --target-dir target/shell-build
```

隔离构建目录避免与用户运行的旧 exe 冲突；测试根在 `target/test-data`，不终止用户程序。全量采用显式单线程测试次序，避免对进程全局 worker 配额的验收互相干扰，不等于禁止产品内部并发。

## 8. 验收记录

### 8.1 功能 → 测试

| 功能 | 测试文件 | 代表性证据 |
|---|---|---|
| 共同前端/UI预拒绝/输入预算/身份 | [interactive.rs](../tests/script/interactive.rs) | 直接语法、状态、字节精确输出、超长/UTF-8 整行排空 |
| 展开/算术/引用 | [expansions.rs](../tests/script/expansions.rs) | 实际命令替换、未用分支无效果、检查算术与整体预解析 |
| local/选项/IFS/EXIT/累计预算 | [declarations.rs](../tests/script/declarations.rs) | 保存绑定门摘掉后实际长度错误，不只是缺错误文字 |
| glob/tilde/授权/限额 | [globbing.rs](../tests/script/globbing.rs) | 实际有界目录游标、禁止目录不枚举、未匹配枚举 |
| case | [case.rs](../tests/script/case.rs) | 首个分支、引用模式、命令替换括号/注释 |
| 嵌套 sh/source | [nested.rs](../tests/script/nested.rs) | 参数/输入/状态恢复、FD 共享、总调用预算 |
| 词法/AST | [lexer.rs](../tests/script/lexer.rs)、[parser.rs](../tests/script/parser.rs) | 引号/转义元数据、整体预检查、嵌套上限 |
| 求值/模块/原Shell | [runtime.rs](../tests/script/runtime.rs)、[shell.rs](../tests/script/shell.rs) | 控制流、文件效果、模块输入消费、权限、日志隐私 |
| 有界 pipe / cooperation | [pipe.rs](../tests/script/pipe.rs) | 实际 blocked_writes、二进制往返、EOF、早关、ACK、取消不可重试 |
| 并发流式与回退 | [streaming.rs](../tests/script/streaming.rs) | 2 MiB→2097152、printf→131200、消费失败不重放、typed fallback、配额释放 |
| 作用域作业 | [jobs.rs](../tests/script/jobs.rs) | 实际模块/嵌套 FD 共享、EOF stdin、退出/身份 join、暂停/恢复/取消、容量前置门、ID不复用、预算共享 |
| 实际程序 | [script_cli.rs](../tests/script_cli.rs) | 文件脚本、直接语法/exit 7、2 MiB流、后台退出3、暂停/恢复、取消130及无后续文件 |

### 8.2 最新执行与历史快照

- M4 历史稳定快照：152 模块单元 + 118 脚本功能 + 5 实际进程/ConPTY = **275 通过**，当时严格 Clippy 通过，源码/测试前后一致。此数不是 M5 当前工作树的全量结果。
- 最新稳定全量（2026-11-08）：`cargo test --offline --target-dir target/shell-build -- --test-threads=1 --nocapture`，**169 模块单元 + 145 脚本功能 + 6 实际进程/ConPTY = 320 全部通过，exit 0**。145 包含原118及 pipe8、stream7、jobs12；6 为 FD、文件桥、日志、对象归属、脚本真实进程及 ConPTY，不叠加历史批次。源码/测试前后 SHA-256 一致：`961b66b74116bbe0ca5356d39d90f6e78d266318fb0d0b792b5ef08e03dad19c`。
- 同批严格 Clippy **未通过，exit 101**：`src/filesystem/ownership.rs:293` collapsible_if；`src/filesystem/ownership_model.rs:66` manual_is_multiple_of、`:102` write_with_newline。均为并行 FS 增量，本会话未改其业务/守卫、未加 allow、未放宽门槛；不能把测试通过写成全仓质量门全绿。
- M5 修正后真实 CLI 通过：2 MiB→`2097152`、后台退出3、STOP 实际 ACK、bg/fg回收；TERM 后 wait和嵌套诊断均为130，无 status 2、无取消后的文件。正常 exit/EOF 和身份切换 join、实际共享 FD、空后台stdin、满表前置门/前台存活均纳入功能回归。
- M5 新增功能首次回归145中144通过，唯一失败是新增 FD 共享测试把 seek 参数顺序写反；按实际 `fd seek TOKEN OFFSET ORIGIN` 修正后145全过。取消错误曾被通用诊断映射为2，已按实际观察状态统一传播130，并防止嵌套sh/source/条件分支和EXIT handler在取消后执行更多效果；未改FS或时间阈值。
- M5 必要性对照（独立 rustc 编译，源码只在内存变更）：正常大文件/消费守卫各通过；强制全部缓冲后实际输出 `0\n` 而非 `2097152\n`，断言 exit 101；摘掉父 stdin 的 drain 后剩余字节断言 exit 101；取消类型改回 Interrupted 后 write_all 在 5 秒内无法结束，独立进程被限时终止并等待退出。源码/测试哈希前后一致。独立编译包装首次未正确定位 filesystem 子模块，修正后才执行对照，首次编译失败不计为守卫红。
- M4 必要性：分别移除 local/临时旧值预算，编译成功后得到 `40000:40000`，正常必须 `40000:0`，断言 exit 101；嵌套重置预算变体为 15 而非 4096，守卫红；移除 Literal 转义元数据后词法守卫红。生产文件未改。
- M3 236（152+79+5）、更早 190（133+54+3）、128/126/101 等为历史，不叠加。FD14、FS 批次已包含在对应总数中。
- Ubuntu/Linux 原生、真实断电、全部 Windows 终端、全部敌对并发未测。Windows ConPTY 不外推为所有终端通过。

## 9. 已批准开发顺序

| 顺序 | 状态 | 边界 |
|---|---|---|
| 1. mv/cp/stat 薄适配 | 已验证，附 find/du | 真实 FS 接口，不改 FS 业务 |
| 2. 共同交互/脚本入口 | 已验证 | 直接单行程序；多行续行 UI 未完成 |
| 3. 脚本互相调用 | 已验证 | 参数/输入/环境恢复、共享资源和预算 |
| 4. 常用语法 | 已验证限定子集 | 不外推完整 Bash |
| 5. 流式管道与作业控制 | 功能与真实 CLI 验收通过；全仓严格 Clippy 待 FS 三处 lint 修正 | 仅适用阶段流式；typed fallback；作用域合作式控制，不冒充内核信号/进程组 |

本表是已批准推进顺序，不因模块能编译便宣称全量完成；新增能力须同步本文件、命令账本、README 和 help。

## 10. 修订记录

| 日期 | 修订 |
|---|---|
| 2026-11-08 | M5 接通真正并发有界阶段与作用域作业，就地撤回“全部顺序缓冲”“不支持 &”；注明回退代价、队列/RSS边界、ACK/实际观察、共享预算、身份/退出join。取消统一130，新增共享FD测试参数顺序纠正；稳定全量169+145+6=320通过，三个必要性对照成立。全仓严格Clippy仍有FS三处lint，未放宽。 |
| 2026-11-08 | M4 常用语法限定子集、提示符/累计参数/保存绑定预算、显式解析深度；275 与严格 Clippy 通过，local/临时旧值必要性数据变体红。 |
| 2026-11-08 | M1–M3 文件桥、共同前端、exit 和嵌套接通；清除普通命令双分发，迁移 cat 字节精确/多路径/合法操作符，保留授权和 FD 操作审计。236 与严格 Clippy 通过，共享预算变体红。 |
| 2026-11-08 | FD 薄适配与 owner 绑定、身份关旧能力保代际，14功能及真实 CLI；190 历史快照。 |
| 2026-11-07 | 独立 lexer/parser/runtime 与 sh/source；模块库统一测试入口，真实任务/用户/日志和输入消费；128/126 历史快照。 |
