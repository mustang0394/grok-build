# Fork 维护文档（给未来的自己 / AI 助手看）

> 本 fork 目标：**在最小改动的前提下，禁用所有对外数据外发**（遥测 / 监控 / 代码上传 /
> 自动更新），并兼容第三方 OpenAI 兼容接口返回的 `usage: null`。
> 所有 fork 改动都用 `// FORK:` 注释标记，`git log -S FORK` 可列全。
> 上游：SpaceXAI grok-build（定期从 monorepo 同步）。

## 1. 改动总览

| # | 文件 | 改动（一句话） | 意图 |
|---|---|---|---|
| 1 | `crates/codegen/xai-grok-telemetry/src/client.rs` | `is_enabled()` / `is_session_metrics_enabled()` 恒返 `false`；`init()` / `init_if_needed()` 开头强制 `mode = Disabled`（原函数体保留） | 产品事件 / Mixpanel / session-metrics 永不外发 |
| 2 | `crates/codegen/xai-grok-telemetry/src/sentry.rs` | `init()` 恒走 `disabled: true` 的 no-op 路径 | 崩溃上报永不外发 |
| 3 | `crates/codegen/xai-grok-telemetry/src/otel_layer/mod.rs` | `build_otel_layer()` 开头强制 `config.exporter.enabled = false`（+3 行） | 内部 OTLP span 只建不导出 |
| 4 | `crates/codegen/xai-grok-telemetry/src/external/mod.rs` | `init()` 直接置 `None`；`is_active()` 恒返 `false` | 外部 OTEL 流永不激活（注：该流本是发往**用户自己的** collector，见 §4） |
| 5 | `crates/codegen/xai-grok-shell/src/agent/config.rs` | `resolve_trace_upload()` 首行直接 `return Resolved::new(false, …)`（原函数体保留） | 会话 trace / 代码上传总闸关闭（含 GCS / proxy / heap-profile 路径） |
| 6 | `crates/codegen/xai-grok-update/src/auto_update.rs` | `run_update_if_available` / `check_update_background` / `ensure_latest_on_disk` 三个函数开头直接返回（原函数体保留） | 自动版本检查/下载永不联网。**显式**的 `grok update`（`run_update`）没动 |
| 7 | `crates/codegen/xai-grok-sampling-types/src/serde_helpers.rs` | 新增 `null_as_zero`（`null`/缺失 → `0`） | 支撑改动 #8 |
| 8 | `…/sampling-types/src/types.rs`（`Usage` + 内层 details）、`conversation.rs`（`TokenUsage`）、`messages.rs`（`MessagesUsage` / `MessageDeltaUsage`） | 所有 `u32` 计数器加 `#[serde(default, deserialize_with = "crate::serde_helpers::null_as_zero")]` + 一个 null 回归测试 | 第三方 OpenAI 兼容接口在 `usage` 里返显式 `null` 时按 0 解析，不再整包解析失败 |
| 9 | `.github/workflows/release.yml` | 新增（上游无此文件） | 手动触发：查上游最新版本 → 编译 Linux/Windows x64 → 发 GitHub Release，见 §5 |
| 10 | `crates/codegen/xai-grok-sampler/src/client.rs` | 新增 `is_droppable_responses_event()` + 流扫描处丢弃分支（FORK 标记，+90 行） | 第三方 Responses 兼容接口的心跳/未知 `type` 事件（如 `{"type":"ping"}`）不再杀死整轮；已知 `type` 但 shape 坏的照常报错。**调用顺序约束**：`try_parse_stream_error` 必须在前，`{"type":"error"}` 帧永远是流错误、不可吞，见代码注释 |
| 11 | `crates/codegen/xai-grok-shell/src/agent/mvp_agent/agent_ops.rs` | `trace_upload_posture_allows_offer()` 恒返 `false`（+4 行） | 补旁路：用户同意反馈附 trace 时的一次性归档上传（该函数明确忽略 `trace_upload` 总闸）。纯 `/feedback` 文字不受影响 |
| 13 | `crates/codegen/xai-mixpanel/src/lib.rs` | `track`/`engage` 硬 no-op（+10 行） | 纵深防御：即使将来某条路径绕开总闸直接构造 client，HTTP 层也不发出。`prepare_properties` 及其单测保留 |
| 14 | `crates/codegen/xai-grok-telemetry/src/config.rs` | `TelemetryConfig::default` 不再烘焙端点/密钥（+8 行），测试改名 `default_is_privacy_hard_off` | 端点 URL/key 不再进二进制（`strings` 也提不出）；`trace_upload` 默认 `Some(false)` |
| 15 | `crates/codegen/xai-grok-shell/src/agent/config.rs` | `resolve_telemetry_mode()` 恒返 Disabled（+5 行） | 堵真漏：`session.telemetry_enabled`（feedback 信号同步/turn-delta）和 `product_analytics_enabled()` 的活配置路径被掐死 |
| 16 | 同上 `config.rs` + `xai-grok-update/src/auto_update.rs` | `is_feedback_enabled()` 缺省关（仅 `GROK_FEEDBACK_ENABLED` 显式开）；`run_update`/`run_install_script` 拒绝 vendor 安装并指引源码重编 | 反馈文字与手动更新不再能联网找 x.ai / 替换 fork 二进制 |
| 17 | `crates/codegen/xai-grok-shell/src/extensions/privacy.rs` | 留存开关锁死 opt-out（+8 行）：opt-in 请求在本地直接拒绝（零网络）；仅 opt-out（`true`，与默认值一致）透传给服务端确认 | 服务端训练数据留存不许开 |
| 12 | `crates/build/xai-proto-build/src/lib.rs` | Windows 下跳过 protoc 预检（`cfg!(windows)`，+9 行） | 上游预检硬编码 `--dependency_out=/dev/stdout --descriptor_set_out=/dev/null`，Windows 无此路径致构建失败；改为对 proto 文件发保守 `rerun-if-changed`。非 Windows 行为零改动 |
| 18 | 代理见 §7（sampler `shared_http.rs` + shell `config.rs`/`init.rs`，约 70 行） | `[proxy] url` / `GROK_PROXY_URL`：配了就走代理的大模型流量（http/socks5 同代码）。详见 §7 |

**刻意没动的东西**：本地日志（`debug_log` / `unified_log` / `sampling_log`）、`enforce_version_policy_or_exit`
（纯本地版本范围检查，不联网）、`announcements` 拉取。
（注：早期版本曾保留 `/feedback` 与手动 `grok update`；改动 #16 已分别加门，未再列为保留。）

## 2. 同步上游操作指南

```sh
git fetch upstream            # upstream = 上游仓库 remote
git merge upstream/main       # 或 rebase，按维护者习惯
git log -S FORK --oneline     # 确认 FORK 点都在
```

### 2.1 冲突处理原则

1. 冲突几乎只会出现在各函数的 **FOR 标记行**（函数入口 3~4 行）。解法固定：
   上游的新函数体留下，把 `// FORK:` 那几行重新贴到函数开头。
2. `is_enabled` / `is_session_metrics_enabled` 若上游重构，保持恒返 `false` 即可。
3. 若上游**删除/重命名**了某个被改函数：先找它的替代者，把 FORK 逻辑搬过去；
   找不到替代者 = 该外发路径已消失，把对应 FORK 标记删掉。
4. 改完后跑 §3 的复查清单。

### 2.2 关键：防“静默漏水”复查（每次同步必做）

最危险的不是冲突，而是上游**新增一条不走总闸的外发路径**。同步后跑这几条 grep，
出现**新面孔**就要跟进：

```sh
# Mixpanel / 产品事件是否出现新的构造点（应只有 telemetry/src/client.rs）
grep -rn "Mixpanel::" crates/ --include="*.rs" | grep -v "telemetry/src\|test\|config"
# 上传是否出现新的调用点（应全被 resolve_trace_upload 门控）
grep -rn "upload_bytes\|upload_file\|upload_stream" crates/ --include="*.rs" | grep -v test | grep -v "xai-file-utils/src"
# telemetry 总闸是否被绕过（所有外发都应先查 is_enabled / is_session_metrics_enabled / external::is_active）
grep -rn "TELEMETRY_CLIENT" crates/codegen/xai-grok-telemetry/src/*.rs | head
```

## 3. 已知测试红灯（预期内，非 bug）

| 测试 | 位置 | 红的原因 |
|---|---|---|
| `sync_profile_is_noop_in_session_metrics_mode` | telemetry `client.rs` | 断言 SessionMetrics 下 client 存活 |
| `resolve_trace_upload_explicit_config_wins_over_telemetry_off` | shell `agent/config_tests.rs` | 断言显式配置能打开上传 |
| `resolve_trace_upload_honors_config_when_telemetry_on` | 同上 | 同上 |
| `trace_upload_decision_debug_reports_winning_source` | 同上 | 可能（断言决策来源字段） |

其余已确认不受影响：sentry（纯 scrubber 测试）、external（只断言 `is_active()==false`）、
auto_update lib 单测（纯工具函数；注意 `--features updater-integration-tests` 的安装器集成套件现会按设计失败）、
otel、`sampling-types`（282 测试通过，含新增 null 回归测试）、mixpanel（`prepare_properties` 测试保留）、
telemetry config（已改名为 `default_is_privacy_hard_off` 断言新行为）、TOML 解析类测试（只测解析不测决议）。

> 可选后续：把上表测试改成断言“禁用行为”（加 FORK 标记），让 `cargo test` 全绿，
> 以后新增的破坏一眼可见。（维护者当时没做，觉得值得就做。）

## 4. 注意事项

- `external` OTEL 流发往的是**用户自己的 collector**（用户显式配 `OTEL_*` 才启用），
  不是 xAI。如果以后想恢复自建监控可观测性，只需删掉改动 #4 的两处 FORK。
- 版本号注入：release 构建靠 `GROK_VERSION` 环境变量（`pager-bin/build.rs` 打进二进制），
  没注入就是 dev 构建。

## 5. Release 流程

`.github/workflows/release.yml`：Actions → Build & Release → Run workflow。
`version` 留空 = 编译时查询上游最新版本（`https://x.ai/cli/stable` 文本指针，
GCS 备用；与内置 updater 同源）；填值则覆盖。产物 `grok-linux-x64` /
`grok-win-x64.exe`，tag `v<版本>`，已存在则覆盖上传。Windows 上游本就 best-effort，
流水线 `fail-fast: false`，Linux 成功即发布。

## 6. 本机环境约束（省得再踩坑）

- 机器内存上限 **2GB**：**不要**跑 `cargo check -p xai-grok-shell` 整包检查（会 OOM/卡死）。
  验证策略：小包（`sampling-types` / `telemetry`）可正常 `cargo check` / `cargo test`；
  大包改动靠 `rustfmt --check` 过解析 + 逐段复核，发版前找大内存机器跑全量检查。
- 构建需要 `protoc`：先 `cargo install dotslash`（`bin/protoc` 经它解析），否则
  `xai-grok-tools-api` 的 build script 会失败。
- Git 全局身份：`mustang0394` / `128272411+mustang0394@users.noreply.github.com`。
- 注意：本仓库 `remote.origin.url` 缺 `github.com` 主机部分，维护者明确说**不用改**，
  push/pull 出问题再看。

## 7. 代理配置（`[proxy] url` / `GROK_PROXY_URL`）

fork 新增功能（上游没有）：配了代理后，**所有大模型请求流量**走代理。

```toml
# ~/.grok/config.toml
[proxy]
url = "http://127.0.0.1:8080"        # 或 https://，或 socks5(h)://
# url = "socks5h://127.0.0.1:1080"
# url = "http://user:pass@proxy.corp:8080"   # 账号密码可内嵌
```

也可用环境变量（`[proxy] url` 优先，缺席时回退）：`GROK_PROXY_URL=http://127.0.0.1:8080`。

### 行为

- 生效范围：sampler 全部模型面（chat-completions / responses / messages，H2 首选 +
  H1 降级 + 预热共用同一构造器），其它子系统（MCP、模型列表拉取等）不受影响。
- http 与 socks5 走同一 `reqwest::Proxy::all`，`socks` feature 工作区已开，无差别支持。
- scheme 只认 `http(s)://`、`socks5(h)://`（大小写不敏感），其它值 warn 后当没配（直连），
  绝不因配错把工具搞砖；URL 本体（含账号密码）永不记日志。
- 未配置 = 与原来逐行一致的直连行为。

### 实现位置（同步时对照）

- `xai-grok-sampler/src/shared_http.rs`：`EGRESS_PROXY` 闩 + `normalize_egress_proxy`
  （纯函数，有单测）+ 两构造器的 `apply_egress_proxy`；`lib.rs` 只重导出
  `set_egress_proxy` 一个符号。
- `xai-grok-shell/src/agent/config.rs`：`ProxyConfig` + `AgentConfig.proxy` 字段。
- `xai-grok-shell/src/agent/init.rs`：`init_process()`（全模式必经的 `Once`）内闩定，
  文件值优先、环境变量回退。
- 文档：`26-config-reference.md` 的 `### proxy` 节。

### 已知限制（v1，故意的）

- `NO_PROXY` 例外名单不支持：配了就是全走代理。以后要加就是 `ProxyConfig` 多一个
  `no_proxy` 字段 + 构造器里 `Proxy::all` 换自定义判断，扩展位已留好。
- 闩定后进程内不可改（与该文件其它 read-once 开关一致）；改配置要重启进程
  （leader 模式重启 leader 生效）。
