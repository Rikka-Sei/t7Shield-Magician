# 界面原生重构 + 设置功能 + 文案产品化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用纯 libadwaita 原生组件彻底重做 MagiShield 界面（深色主题、结构化设备卡、pill 语义徽章、空态、错误分级），新增设置功能（主题/语言切换，运行时即时生效并持久化），并把全部 86+ 条用户可见文案重写为产品级语言。

**Architecture:** 三件套串行：先 spec 归因 D28（spec-first)，再 UI 重构（零自定义 CSS，只用内置类与原生组件），再设置功能（`AdwPreferencesDialog` + `glib::KeyFile` 持久化 + `relocalize()` 重渲染），最后文案重写（只改 locales 值，键名不动）。所有显示文案仍只经 i18n 键渲染，`.ui` 保持零字面量。

**Tech Stack:** Rust + GTK4 0.11.4(v4_10) + libadwaita 0.9.2(v1_6) + rust-i18n 4.2 + glib KeyFile（随 gtk4 重导出，零新增依赖）。

**Spec:** `docs/specs/t7-magician/spec.md`（唯一权威；变更流程见 §10 第 971 行附近）

## Global Constraints

每个任务的实现者都必须遵守（值逐字拷贝，不得改写）：

- **GTK/libadwaita 版本**：gtk4 0.11.4(features `v4_10`)、libadwaita 0.9.2(features `v1_6`)；nix 运行时 GTK 4.22.4 / libadwaita 1.9.3。**不得新增任何 Cargo 依赖**（glib/gio 经 `gtk::glib`/`gtk::gio` 重导出使用）。
- **零自定义 CSS**：只用 libadwaita/GTK 内置样式类（`card`/`title-1`/`title-3`/`title-4`/`heading`/`dim-label`/`caption`/`suggested-action`/`flat`/`warning`/`success`/`error`/`navigation-sidebar`/`pill`）与原生组件。不得创建 `.css` 文件，不得用 `CssProvider`。
- **`.ui` 零文案字面量**(K5 / spec §4.11 / AC-015)：模板只放结构、id、class 与 `icon-name`；禁中文，禁 `label=`/`title=`/`subtitle=`/`text=`/`tooltip-text=`/`placeholder-text=` 非空取值。测试 `assert_no_display_literals`（main_window.rs）会扫描。
- **i18n 键纪律**：`zh-CN.yml` 与 `en.yml` 键集合完全一致（presentation.rs 有测试）；键名不得改动；`%{var}` 占位符必须保留；`progress.{Step}` 七键、每个错误码的 `reason`/`advice` 键不可删。
- **错误拼接格式不动**：`show_error` 的 `{code}：{reason}；{advice}`（全角冒号与分号）与 `password_dialog.rs` 的 `{code}：{reason}` 格式保持不变（password_dialog.rs:242-248 测试耦合）。因此 `reason.*`/`advice.*` 文案值内**不得出现** `：` `；` 或英文 `;`。
- **测试纪律**：测试里调用 `rust_i18n::set_locale` 后必须恢复原值（进程级全局，并行测试会互相污染）；文案断言一律 `t!(key, locale = locale)` 参数化。既有 8 条锚点测试（见 manifest test_anchors）不得改名/删除。
- **spec 用词红线**(manifest vague_terms 全局扫描）：spec 正文禁「支持/可能/快速/合理/适当/若干」等模糊词；§4 禁像素值与十六进制色值。
- **口令纪律**：口令不落盘不入日志；设置文件只持久化 `language` 与 `theme` 两个枚举值（AC-011 不冲突）。
- **提交**：遵循 `.claude/commands/commit-message.md` 生成中文提交信息，GPG 签名（`git commit -S`）。
- **验证命令**（在 `nix develop` 环境内）：`cargo test --workspace`、`cargo clippy --workspace -- -D warnings`、`python3 docs/specs/t7-magician/tools/test_audit_spec.py`、`python3 docs/specs/t7-magician/tools/audit_spec.py`、`python3 docs/specs/t7-magician/tools/barriers.py`。
- **运行目标平台仅 Linux**(D27);macOS 上的运行仅作视觉验证，不构成平台承诺。

## 设计决策汇总（调查归因，实现者照此执行，不得另选）

| 决策 | 取值 | 依据 |
|---|---|---|
| 设置入口 | HeaderBar 末端图标按钮 `action_preferences`(icon `emblem-system-symbolic`)，不动侧边栏 | spec §4.11 把导航写死为「三类页面（仪表盘、诊断、关于）」 |
| 设置对话框 | `AdwPreferencesDialog` 模板 `MagiSettingsDialog`，两个 `AdwComboRow`（主题/语言），候选项在 Rust 侧用 `gtk::StringList` + `t!()` 构造（.ui 禁字面量） | libadwaita 0.9.2 全部可用 |
| 主题三态 | `跟随系统 → adw::ColorScheme::Default`；`浅色 → ForceLight`；`深色 → ForceDark`。**默认深色** | spec §4.11「侧边栏为深色外观」判据 + Samsung Magician 观感 |
| 语言三态 | `跟随系统`（走环境变量）/ `中文` / `English`；语言项用 autonym（"中文"/"English" 两种语言下各自不变） | 产品惯例 |
| 语言优先级 | 应用内显式选择 > 环境变量（`LC_ALL`/`LC_MESSAGES`/`LANG`) > 默认 `zh-CN` | spec §6 从未规定语言由环境决定，环境检测只是现状实现 |
| 持久化 | `glib::KeyFile` 写 `glib::user_config_dir()/magi/settings.ini`，组 `[ui]`，键 `language`/`theme`；读写失败一律回落内存默认值 | GSettings 需 gschema 编译 + GSETTINGS_SCHEMA_DIR，nix/headless 双重摩擦（调查结论） |
| 重渲染 | `MainWindow::relocalize()` 重放全部静态文案；`WindowState` 新增 `last_hit: Option<ScanHit>`、`last_outcome: Option<StoredOutcome>`、`open_dialog: Option<glib::WeakRef<PasswordDialog>>` 三处缓存；`PasswordDialog` 加 `relocalize()` | `RescanState::observe` 身份不变返回 Keep，refresh_devices 不会重刷文案，必须显式重放 |
| 术语 | UI 文案中文一律用「密码」（弃「口令」）；诊断页技术文案可保留术语 | CopyAudit 调查结论（research-copy.md §4） |
| 呈现码前缀 | 保留 `{code}：…` 前缀（spec §4.13 呈现码是规范要求，不改） | spec |

---

### Task 1: spec 归因 D28（设置：主题与语言切换）

**Files:**
- Modify: `docs/specs/t7-magician/spec.md`（§4.11、§6、§9）
- Modify: `docs/specs/t7-magician/tools/audit_manifest.json`(`latest_decision`、`decisions`)

**Interfaces:**
- Consumes: 现有 spec 结构（§9 最大决策 ID = D27,manifest `latest_decision: 27`)。
- Produces: D28 决策与 §4.11/§6 新条款，供 Task 3 实现对照；§10 锚点表与 manifest `test_anchors` 的新增**留给 Task 3** 与代码同批（锚点指向尚不存在的测试名时审计会报错，故不同步到本任务）。

- [ ] **Step 1: 读 spec 相关段落**

读 `docs/specs/t7-magician/spec.md` 的 §4.11（约 657-737 行）、§6（约 855-870 行）、§9（约 902-935 行）、§10 锚点表，以及 `tools/audit_manifest.json` 的 `latest_decision`、`decisions` 数组末项（D27）写法、D16 规则（§6 内 `zh-CN` 计数）、D25 规则（§4 术语计数与禁用模式）、`vague_terms` 列表。

- [ ] **Step 2: 改 manifest（先改）**

`tools/audit_manifest.json`:
1. `"latest_decision": 27` → `28`。
2. `decisions` 数组按 D27 同款结构新增 D28 条目，`sections` 覆盖 `§4.11`、`§6`、`§9`,required occurrence 规则：
   - §9 出现 `D28` ≥ 1;
   - §4.11 出现「首选项」≥ 1 与「设置」≥ 1;
   - §6 出现「语言优先级」≥ 1。
   （若 manifest 的 decisions 条目结构与上述字段名不同，按现有 D27 条目的真实结构对齐，保持审计器可解析。)

- [ ] **Step 3: 改 spec.md**

1. §4.11 验收标准（Given/When/Then 列表）末尾新增一条：

```
> - Given 任一页面；When 点击 HeaderBar 的首选项入口；Then 打开设置对话框，呈现主题（跟随系统/浅色/深色）与语言（跟随系统/中文/English）两组选择；更改立即生效并持久化，下次启动保持。
```

2. §4.11 布局契约表追加两行（沿用既有三列格式）：

```
| HeaderBar | 首选项入口 | 任一页面可触达；点击打开设置对话框 |
| 设置对话框 | 主题三态、语言三态 | 默认深色主题；默认语言跟随系统；更改立即生效并持久化 |
```

3. §4.11 契约块（`MainWindow` 模板子件清单附近）补一行 `#[template_child] pub action_preferences: TemplateChild<gtk::Button>, // 首选项入口`，并在契约块后补一段设置对话框契约（与 PasswordDialog 块同款风格）：

```rust
// 设置对话框（D28）：主题与语言两组三态选择，更改立即生效并持久化。
pub struct SettingsDialog … {
    #[template_child] pub theme_row: TemplateChild<adw::ComboRow>,
    #[template_child] pub language_row: TemplateChild<adw::ComboRow>,
}
```

4. §6「国际化」条目（保持 `zh-CN` 出现 ≥1 次，不用 vague_terms)追加：

```
  - 语言优先级：应用内显式选择 > 环境变量（LC_ALL/LC_MESSAGES/LANG）> 默认 `zh-CN`；主题提供跟随系统/浅色/深色三态，默认深色；两项选择持久化于用户配置目录，更改即时生效（D28）。
```

5. §9 决策日志追加（表格格式与 D27 行对齐）：

```
| D28 | 应用内设置：HeaderBar 提供首选项入口，打开设置对话框；主题为跟随系统/浅色/深色三态，默认深色；界面语言为跟随系统/中文/English 三态，默认跟随系统，语言优先级 = 应用内显式选择 > 环境变量 > 默认 zh-CN；两项选择持久化于用户配置目录（glib KeyFile），更改即时生效（主题经 AdwStyleManager，语言经 rust_i18n 重渲染全部静态文案）；侧边栏导航仍为三类页面不变 | §4.11、§6、§9 | §4.11 出现首选项入口与设置对话框契约；§6 出现语言优先级链 |
```

6. §4.11 节首的归因行（「归因 D09、D10、D24、D25」处）补 `、D28`。

- [ ] **Step 4: 跑 spec 侧两道审计**

Run: `nix develop -c python3 docs/specs/t7-magician/tools/test_audit_spec.py && nix develop -c python3 docs/specs/t7-magician/tools/audit_spec.py`
Expected: 全 PASS、无 error/warning。若 D25/D16 计数或 vague_terms 报错，按审计输出修正措辞（禁词替换、计数不足则在相应章节自然补术语），不得删规则。

- [ ] **Step 5: 跑 barriers 确认无回归**

Run: `nix develop -c python3 docs/specs/t7-magician/tools/barriers.py`
Expected: 全绿（本任务不改代码，三条 cargo test 屏障应原样通过）。

- [ ] **Step 6: Commit**

```bash
git add docs/specs/t7-magician/spec.md docs/specs/t7-magician/tools/audit_manifest.json
# 提交信息按 .claude/commands/commit-message.md 生成（type: docs(spec)），GPG 签名
git commit -S -m "<生成的提交信息>"
```

---

### Task 2: 主窗口与口令对话框的原生重构

**Files:**
- Modify: `crates/magi-app/src/ui/main_window.ui`（整体重写为下方内容）
- Modify: `crates/magi-app/src/main_window.rs`(imp 子件、setup、设备卡呈现、徽章、结果区、进度、空态、侧边栏折叠）
- Modify: `crates/magi-app/src/ui/password_dialog.ui`（下方内容）
- Modify: `crates/magi-app/src/password_dialog.rs`（错误行加 `error` 类）
- Modify: `crates/magi-app/locales/zh-CN.yml`、`crates/magi-app/locales/en.yml`（新增 6 键）

**Interfaces:**
- Consumes: 现有 `DeviceIdentity`/`ActionId`/`UnlockGate`/`ScanHit`/`AppError` 接口不变。
- Produces（Task 3 依赖的接口，名字与签名必须逐字如此）:
  - 模板子件 id:`split_view`、`sidebar_toggle`、`device_area_stack`、`empty_state`、`device_icon`、`status_icon`、`result_icon`、`progress_box`、`password_admin_note`、`actions_caption`、`feedback_caption`、`device_node_caption`、`device_channel_caption`、`device_descriptor_caption`;
  - `MainWindow::show_outcome(&self, text: &str, kind: Outcome)`，其中 `enum Outcome { Neutral, Success, Error }`(pub(crate));
  - `MainWindow::relocalize()` 在本任务**不实现**(Task 3 实现），但 setup() 的文案赋值段要保持集中，便于 Task 3 抽取。

**约束复述**：本任务只做结构与呈现；`action_preferences` 按钮由 Task 3 加入，本任务不加。默认深色主题由 Task 3 落地（StyleManager 需要设置默认值驱动），本任务不动 lib.rs。

- [ ] **Step 1: 重写 `crates/magi-app/src/ui/main_window.ui` 为以下内容（逐字）**

要点说明（供理解，不写进文件）：保留全部既有 id(device_card/status_label/action_*/progress/result_label 等，类型不变：device_card=AdwBin、status_label=GtkLabel、action_unlock=GtkButton、progress=GtkProgressBar、result_label=GtkLabel);navigation-sidebar 类保留；无文案字面量；icon-name 允许。

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!--
  §4.12 主窗口模板（K5）：只承载结构与 id/class，不含任何用户可见字面量；
  全部文案在 MainWindow::setup() 里经 i18n 键赋值（locales/*.yml）。
  布局全部使用 libadwaita/GTK 原生组件与内置样式类（无自定义样式表）：
  AdwOverlaySplitView 侧边栏（自带 AdwHeaderBar）+ GtkListBox 挂内置
  `.navigation-sidebar` 类做导航（选中态走 ListBox 原生选中机制）；
  仪表盘：设备卡（.card + 图标 + pill 语义徽章）/ 操作区 / 进度与结果反馈；
  空态用 AdwStatusPage；深浅色由 AdwStyleManager 统一裁决（D28 默认深色）。
-->
<interface>
  <requires lib="gtk" version="4.10"/>
  <requires lib="Adw" version="1.6"/>
  <template class="MagiMainWindow" parent="AdwApplicationWindow">
    <property name="default-width">1024</property>
    <property name="default-height">680</property>
    <property name="content">
      <object class="AdwToolbarView">
        <child type="top">
          <object class="AdwHeaderBar">
            <child type="start">
              <object class="GtkToggleButton" id="sidebar_toggle">
                <property name="icon-name">sidebar-show-symbolic</property>
              </object>
            </child>
            <property name="title-widget">
              <object class="AdwWindowTitle" id="window_title"/>
            </property>
          </object>
        </child>
        <property name="content">
          <object class="AdwOverlaySplitView" id="split_view">
            <property name="min-sidebar-width">240</property>
            <property name="max-sidebar-width">280</property>
            <property name="sidebar">
              <object class="AdwToolbarView">
                <child type="top">
                  <object class="AdwHeaderBar">
                    <property name="title-widget">
                      <object class="GtkLabel" id="brand_label">
                        <style>
                          <class name="title-4"/>
                        </style>
                      </object>
                    </property>
                    <style>
                      <class name="flat"/>
                    </style>
                  </object>
                </child>
                <property name="content">
                  <object class="GtkBox" id="sidebar">
                    <property name="orientation">vertical</property>
                    <property name="spacing">12</property>
                    <property name="margin-top">12</property>
                    <property name="margin-bottom">12</property>
                    <property name="margin-start">12</property>
                    <property name="margin-end">12</property>
                    <child>
                      <object class="GtkListBox" id="nav_list">
                        <property name="selection-mode">single</property>
                        <style>
                          <class name="navigation-sidebar"/>
                        </style>
                        <child>
                          <object class="GtkListBoxRow" id="nav_dashboard">
                            <child>
                              <object class="GtkBox">
                                <property name="orientation">horizontal</property>
                                <property name="spacing">10</property>
                                <child>
                                  <object class="GtkImage" id="nav_dashboard_icon">
                                    <property name="icon-name">view-grid-symbolic</property>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="nav_dashboard_label">
                                    <property name="xalign">0</property>
                                  </object>
                                </child>
                              </object>
                            </child>
                          </object>
                        </child>
                        <child>
                          <object class="GtkListBoxRow" id="nav_diagnostics">
                            <child>
                              <object class="GtkBox">
                                <property name="orientation">horizontal</property>
                                <property name="spacing">10</property>
                                <child>
                                  <object class="GtkImage" id="nav_diagnostics_icon">
                                    <property name="icon-name">view-list-symbolic</property>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="nav_diagnostics_label">
                                    <property name="xalign">0</property>
                                  </object>
                                </child>
                              </object>
                            </child>
                          </object>
                        </child>
                        <child>
                          <object class="GtkListBoxRow" id="nav_about">
                            <child>
                              <object class="GtkBox">
                                <property name="orientation">horizontal</property>
                                <property name="spacing">10</property>
                                <child>
                                  <object class="GtkImage" id="nav_about_icon">
                                    <property name="icon-name">help-about-symbolic</property>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="nav_about_label">
                                    <property name="xalign">0</property>
                                  </object>
                                </child>
                              </object>
                            </child>
                          </object>
                        </child>
                      </object>
                    </child>
                    <child>
                      <object class="GtkLabel" id="sidebar_notice_label">
                        <property name="xalign">0</property>
                        <property name="halign">start</property>
                        <property name="wrap">true</property>
                        <property name="vexpand">true</property>
                        <property name="valign">end</property>
                        <style>
                          <class name="dim-label"/>
                          <class name="caption"/>
                        </style>
                      </object>
                    </child>
                  </object>
                </property>
              </object>
            </property>
            <property name="content">
              <object class="GtkStack" id="content_stack">
                <property name="hexpand">true</property>
                <property name="vexpand">true</property>
                <property name="transition-type">crossfade</property>
                <property name="transition-duration">200</property>
                <child>
                  <object class="GtkStackPage">
                    <property name="name">dashboard</property>
                    <property name="child">
                      <object class="GtkScrolledWindow">
                        <property name="hscrollbar-policy">never</property>
                        <property name="child">
                          <object class="AdwClamp">
                            <property name="maximum-size">640</property>
                            <property name="tightening-threshold">480</property>
                            <property name="child">
                              <object class="GtkBox">
                                <property name="orientation">vertical</property>
                                <property name="spacing">18</property>
                                <property name="margin-top">24</property>
                                <property name="margin-bottom">24</property>
                                <property name="margin-start">24</property>
                                <property name="margin-end">24</property>
                                <child>
                                  <object class="GtkLabel" id="dashboard_title_label">
                                    <property name="xalign">0</property>
                                    <property name="halign">start</property>
                                    <style>
                                      <class name="title-1"/>
                                    </style>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkStack" id="device_area_stack">
                                    <property name="hhomogeneous">false</property>
                                    <property name="vhomogeneous">false</property>
                                    <property name="transition-type">crossfade</property>
                                    <property name="transition-duration">200</property>
                                    <child>
                                      <object class="GtkStackPage">
                                        <property name="name">card</property>
                                        <property name="child">
                                          <object class="AdwBin" id="device_card">
                                            <style>
                                              <class name="card"/>
                                            </style>
                                            <property name="child">
                                              <object class="GtkBox">
                                                <property name="orientation">vertical</property>
                                                <property name="margin-top">18</property>
                                                <property name="margin-bottom">18</property>
                                                <property name="margin-start">18</property>
                                                <property name="margin-end">18</property>
                                                <child>
                                                  <object class="GtkLabel" id="device_group_title">
                                                    <property name="xalign">0</property>
                                                    <property name="halign">start</property>
                                                    <property name="margin-bottom">10</property>
                                                    <style>
                                                      <class name="dim-label"/>
                                                      <class name="caption"/>
                                                    </style>
                                                  </object>
                                                </child>
                                                <child>
                                                  <object class="GtkBox">
                                                    <property name="orientation">horizontal</property>
                                                    <property name="spacing">14</property>
                                                    <child>
                                                      <object class="GtkImage" id="device_icon">
                                                        <property name="icon-name">drive-harddisk-symbolic</property>
                                                        <property name="pixel-size">42</property>
                                                        <property name="valign">center</property>
                                                        <style>
                                                          <class name="dim-label"/>
                                                        </style>
                                                      </object>
                                                    </child>
                                                    <child>
                                                      <object class="GtkBox">
                                                        <property name="orientation">vertical</property>
                                                        <property name="spacing">2</property>
                                                        <property name="hexpand">true</property>
                                                        <property name="valign">center</property>
                                                        <child>
                                                          <object class="GtkLabel" id="device_model_label">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <style>
                                                              <class name="heading"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                        <child>
                                                          <object class="GtkLabel" id="device_ids_label">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <property name="selectable">true</property>
                                                            <style>
                                                              <class name="dim-label"/>
                                                              <class name="caption"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                      </object>
                                                    </child>
                                                    <child>
                                                      <object class="GtkBox">
                                                        <property name="orientation">horizontal</property>
                                                        <property name="spacing">6</property>
                                                        <property name="valign">center</property>
                                                        <property name="halign">end</property>
                                                        <child>
                                                          <object class="GtkImage" id="status_icon">
                                                            <property name="pixel-size">16</property>
                                                          </object>
                                                        </child>
                                                        <child>
                                                          <object class="GtkLabel" id="status_label">
                                                            <style>
                                                              <class name="heading"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                      </object>
                                                    </child>
                                                  </object>
                                                </child>
                                                <child>
                                                  <object class="GtkSeparator">
                                                    <property name="margin-top">14</property>
                                                    <property name="margin-bottom">14</property>
                                                  </object>
                                                </child>
                                                <child>
                                                  <object class="GtkBox">
                                                    <property name="orientation">vertical</property>
                                                    <property name="spacing">10</property>
                                                    <child>
                                                      <object class="GtkBox">
                                                        <property name="orientation">vertical</property>
                                                        <property name="spacing">2</property>
                                                        <child>
                                                          <object class="GtkLabel" id="device_node_caption">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <style>
                                                              <class name="dim-label"/>
                                                              <class name="caption"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                        <child>
                                                          <object class="GtkLabel" id="device_node_label">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <property name="selectable">true</property>
                                                          </object>
                                                        </child>
                                                      </object>
                                                    </child>
                                                    <child>
                                                      <object class="GtkBox">
                                                        <property name="orientation">vertical</property>
                                                        <property name="spacing">2</property>
                                                        <child>
                                                          <object class="GtkLabel" id="device_channel_caption">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <style>
                                                              <class name="dim-label"/>
                                                              <class name="caption"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                        <child>
                                                          <object class="GtkLabel" id="device_channel_label">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                          </object>
                                                        </child>
                                                      </object>
                                                    </child>
                                                    <child>
                                                      <object class="GtkBox">
                                                        <property name="orientation">vertical</property>
                                                        <property name="spacing">2</property>
                                                        <child>
                                                          <object class="GtkLabel" id="device_descriptor_caption">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <style>
                                                              <class name="dim-label"/>
                                                              <class name="caption"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                        <child>
                                                          <object class="GtkLabel" id="device_descriptor_label">
                                                            <property name="xalign">0</property>
                                                            <property name="halign">start</property>
                                                            <property name="wrap">true</property>
                                                            <style>
                                                              <class name="dim-label"/>
                                                            </style>
                                                          </object>
                                                        </child>
                                                      </object>
                                                    </child>
                                                  </object>
                                                </child>
                                              </object>
                                            </property>
                                          </object>
                                        </property>
                                      </object>
                                    </child>
                                    <child>
                                      <object class="GtkStackPage">
                                        <property name="name">empty</property>
                                        <property name="child">
                                          <object class="AdwStatusPage" id="empty_state">
                                            <property name="icon-name">drive-harddisk-symbolic</property>
                                          </object>
                                        </property>
                                      </object>
                                    </child>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="actions_caption">
                                    <property name="xalign">0</property>
                                    <property name="halign">start</property>
                                    <style>
                                      <class name="title-4"/>
                                    </style>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkBox">
                                    <property name="orientation">horizontal</property>
                                    <property name="spacing">8</property>
                                    <property name="homogeneous">true</property>
                                    <child>
                                      <object class="GtkButton" id="action_unlock">
                                        <style>
                                          <class name="suggested-action"/>
                                        </style>
                                      </object>
                                    </child>
                                    <child>
                                      <object class="GtkButton" id="action_validate_password"/>
                                    </child>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkBox">
                                    <property name="orientation">horizontal</property>
                                    <property name="spacing">8</property>
                                    <property name="homogeneous">true</property>
                                    <child>
                                      <object class="GtkButton" id="action_set_password"/>
                                    </child>
                                    <child>
                                      <object class="GtkButton" id="action_change_password"/>
                                    </child>
                                    <child>
                                      <object class="GtkButton" id="action_delete_password"/>
                                    </child>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="password_admin_note">
                                    <property name="xalign">0</property>
                                    <property name="halign">start</property>
                                    <property name="wrap">true</property>
                                    <style>
                                      <class name="dim-label"/>
                                      <class name="caption"/>
                                    </style>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="feedback_caption">
                                    <property name="xalign">0</property>
                                    <property name="halign">start</property>
                                    <style>
                                      <class name="title-4"/>
                                    </style>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkBox" id="progress_box">
                                    <property name="orientation">horizontal</property>
                                    <property name="spacing">8</property>
                                    <property name="visible">false</property>
                                    <child>
                                      <object class="GtkProgressBar" id="progress">
                                        <property name="hexpand">true</property>
                                        <property name="valign">center</property>
                                      </object>
                                    </child>
                                    <child>
                                      <object class="GtkButton" id="action_cancel"/>
                                    </child>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkBox">
                                    <property name="orientation">horizontal</property>
                                    <property name="spacing">8</property>
                                    <child>
                                      <object class="GtkImage" id="result_icon">
                                        <property name="pixel-size">16</property>
                                        <property name="valign">start</property>
                                        <property name="margin-top">2</property>
                                        <property name="visible">false</property>
                                      </object>
                                    </child>
                                    <child>
                                      <object class="GtkLabel" id="result_label">
                                        <property name="xalign">0</property>
                                        <property name="halign">start</property>
                                        <property name="hexpand">true</property>
                                        <property name="wrap">true</property>
                                      </object>
                                    </child>
                                  </object>
                                </child>
                                <child>
                                  <object class="GtkLabel" id="platform_notice_label">
                                    <property name="xalign">0</property>
                                    <property name="halign">start</property>
                                    <property name="wrap">true</property>
                                    <style>
                                      <class name="dim-label"/>
                                      <class name="caption"/>
                                    </style>
                                  </object>
                                </child>
                              </object>
                            </property>
                          </object>
                        </property>
                      </object>
                    </property>
                  </object>
                </child>
                <child>
                  <object class="GtkStackPage">
                    <property name="name">diagnostics</property>
                    <property name="child">
                      <object class="GtkBox">
                        <property name="orientation">vertical</property>
                        <property name="spacing">12</property>
                        <property name="margin-top">24</property>
                        <property name="margin-bottom">24</property>
                        <property name="margin-start">24</property>
                        <property name="margin-end">24</property>
                        <child>
                          <object class="GtkLabel" id="diagnostics_title_label">
                            <property name="xalign">0</property>
                            <property name="halign">start</property>
                            <style>
                              <class name="title-1"/>
                            </style>
                          </object>
                        </child>
                        <child>
                          <object class="GtkLabel" id="diagnostics_hint_label">
                            <property name="xalign">0</property>
                            <property name="halign">start</property>
                            <property name="wrap">true</property>
                            <style>
                              <class name="dim-label"/>
                            </style>
                          </object>
                        </child>
                        <child>
                          <object class="GtkScrolledWindow">
                            <property name="vexpand">true</property>
                            <property name="min-content-height">240</property>
                            <style>
                              <class name="card"/>
                            </style>
                            <property name="child">
                              <object class="GtkTextView" id="diagnostics_view">
                                <property name="editable">false</property>
                                <property name="cursor-visible">false</property>
                                <property name="monospace">true</property>
                                <property name="wrap-mode">word-char</property>
                                <property name="top-margin">12</property>
                                <property name="bottom-margin">12</property>
                                <property name="left-margin">12</property>
                                <property name="right-margin">12</property>
                              </object>
                            </property>
                          </object>
                        </child>
                        <child>
                          <object class="GtkButton" id="action_export_diagnostics">
                            <property name="halign">end</property>
                            <style>
                              <class name="suggested-action"/>
                            </style>
                          </object>
                        </child>
                      </object>
                    </property>
                  </object>
                </child>
                <child>
                  <object class="GtkStackPage">
                    <property name="name">about</property>
                    <property name="child">
                      <object class="AdwClamp">
                        <property name="maximum-size">480</property>
                        <property name="child">
                          <object class="GtkBox">
                            <property name="orientation">vertical</property>
                            <property name="spacing">10</property>
                            <property name="margin-top">32</property>
                            <property name="margin-bottom">24</property>
                            <property name="margin-start">24</property>
                            <property name="margin-end">24</property>
                            <child>
                              <object class="GtkImage" id="about_icon">
                                <property name="icon-name">drive-harddisk-symbolic</property>
                                <property name="pixel-size">64</property>
                                <property name="halign">center</property>
                                <property name="margin-bottom">6</property>
                                <style>
                                  <class name="dim-label"/>
                                </style>
                              </object>
                            </child>
                            <child>
                              <object class="GtkLabel" id="about_title_label">
                                <property name="halign">center</property>
                                <style>
                                  <class name="title-1"/>
                                </style>
                              </object>
                            </child>
                            <child>
                              <object class="GtkLabel" id="about_version_label">
                                <property name="halign">center</property>
                                <property name="selectable">true</property>
                                <style>
                                  <class name="dim-label"/>
                                </style>
                              </object>
                            </child>
                            <child>
                              <object class="GtkLabel" id="about_repository_label">
                                <property name="halign">center</property>
                                <property name="selectable">true</property>
                                <property name="wrap">true</property>
                                <property name="justify">center</property>
                                <style>
                                  <class name="dim-label"/>
                                  <class name="caption"/>
                                </style>
                              </object>
                            </child>
                            <child>
                              <object class="GtkLabel" id="about_notice_label">
                                <property name="halign">center</property>
                                <property name="wrap">true</property>
                                <property name="justify">center</property>
                                <property name="margin-top">8</property>
                                <style>
                                  <class name="dim-label"/>
                                </style>
                              </object>
                            </child>
                          </object>
                        </property>
                      </object>
                    </property>
                  </object>
                </child>
              </object>
            </property>
          </object>
        </property>
      </object>
    </property>
  </template>
</interface>
```

- [ ] **Step 2: 改 `main_window.rs` 的 imp 子件表**

在 `mod imp` 的 struct 中新增（保持既有子件不动）:

```rust
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub sidebar_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub device_area_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_state: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub result_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub progress_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub password_admin_note: TemplateChild<gtk::Label>,
        #[template_child]
        pub actions_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub feedback_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_node_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_channel_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub device_descriptor_caption: TemplateChild<gtk::Label>,
```

- [ ] **Step 3: 新增 `Outcome` 枚举与结果呈现中枢**

在 `badge_class` 附近新增：

```rust
/// 结果区语义分级（内置类）：中性（默认文本，无图标）、成功（success + emblem-ok）、
/// 错误（error + dialog-warning）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Neutral,
    Success,
    Error,
}

/// 结果键 → 语义分级：判据确认/口令被接受为成功；其余（含取消、未观察到重枚举）为中性。
pub(crate) fn result_kind(key: &str) -> Outcome {
    match key {
        "result.RealPartitionTable" | "result.LockingFlags" | "result.validate_accepted" => {
            Outcome::Success
        }
        "diagnostics.export_failed" => Outcome::Error,
        _ => Outcome::Neutral,
    }
}

/// 徽章图标（语义与徽章类一一对应，全部为 Adwaita 图标主题自带 symbolic 图标）。
pub(crate) fn status_icon_name(identity: DeviceIdentity) -> &'static str {
    match identity {
        DeviceIdentity::Locked => "changes-prevent-symbolic",
        DeviceIdentity::Unlocked => "emblem-ok-symbolic",
        DeviceIdentity::ReEnumerating => "view-refresh-symbolic",
        DeviceIdentity::Unrecognized => "dialog-question-symbolic",
    }
}
```

`MainWindow` 新增：

```rust
    /// 结果区统一呈现：文本 + 语义类 + 图标；进度条随结果/错误隐藏。
    pub(crate) fn show_outcome(&self, text: &str, kind: Outcome) {
        let imp = self.imp();
        for class in ["success", "error", "dim-label"] {
            imp.result_label.remove_css_class(class);
            imp.result_icon.remove_css_class(class);
        }
        imp.result_label.set_label(text);
        match kind {
            Outcome::Neutral => imp.result_icon.set_visible(false),
            Outcome::Success => {
                imp.result_label.add_css_class("success");
                imp.result_icon.add_css_class("success");
                imp.result_icon.set_icon_name(Some("emblem-ok-symbolic"));
                imp.result_icon.set_visible(true);
            }
            Outcome::Error => {
                imp.result_label.add_css_class("error");
                imp.result_icon.add_css_class("error");
                imp.result_icon.set_icon_name(Some("dialog-warning-symbolic"));
                imp.result_icon.set_visible(true);
            }
        }
        imp.progress.set_fraction(0.0);
        imp.progress_box.set_visible(false);
    }
```

- [ ] **Step 4: 改造既有呈现函数**

- `set_badge_class`：对 `status_label` 与 `status_icon` 同步摘挂 `warning`/`success`/`dim-label` 类，并 `imp.status_icon.set_icon_name(Some(status_icon_name(identity)))`。
- `show_hit`：首行加 `imp.device_area_stack.set_visible_child_name("card");`;`device_node_label` 只填节点值（`Some(node) => node.to_string()`,`None => ""`)，不再拼 `device.node` 前缀（前缀由 `device_node_caption` 承担）。
- `show_unknown_device`：首行加 `imp.device_area_stack.set_visible_child_name("empty");`，其余置空逻辑不变。
- `show_step`:`self.imp().progress.set_visible(true)` 改为 `self.imp().progress_box.set_visible(true)`。
- `show_result`：改为 `self.show_outcome(&t!(message_key), result_kind(message_key));`（删除对 progress 的直接操作）。
- `show_error`：文本构造与诊断记录不变，呈现改为 `self.show_outcome(&text, Outcome::Error);`，返回 `text` 不变。
- `finish` 的 mounted 分支：`self.imp().result_label.set_label(&text); self.imp().progress.set_fraction(0.0);` 两行改为 `self.show_outcome(&text, Outcome::Success);`。

- [ ] **Step 5: setup() 增补文案与侧边栏折叠绑定**

在 setup() 中（文案段保持集中）追加：

```rust
        imp.device_node_caption.set_label(&t!("device.node"));
        imp.device_channel_caption.set_label(&t!("device.channel_title"));
        imp.device_descriptor_caption
            .set_label(&t!("device.descriptor_title"));
        imp.actions_caption.set_label(&t!("dashboard.actions_title"));
        imp.feedback_caption.set_label(&t!("dashboard.feedback_title"));
        imp.password_admin_note.set_label(&t!("reason.evidence_gap"));
        imp.empty_state
            .set_title(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.empty_state
            .set_description(Some(&t!("device.empty_hint")));
        imp.sidebar_toggle
            .set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        // 窄窗口折叠时才显示侧边栏开关；开关与 show-sidebar 双向同步（原生绑定）。
        imp.split_view
            .bind_property("collapsed", &imp.sidebar_toggle.get(), "visible")
            .sync_create()
            .build();
        imp.sidebar_toggle
            .bind_property("active", &imp.split_view.get(), "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
        self.show_outcome(&t!("progress.idle"), Outcome::Neutral);
```

同时删除 setup() 里的 `imp.progress.set_visible(false);` 与 `imp.result_label.set_label(&t!("progress.idle"));`（由上一行 `show_outcome` 接管）。

- [ ] **Step 6: 口令对话框模板重写为以下内容（逐字）**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!--
  §4.12 口令对话框模板（K5）：只承载结构与 id/class，不含任何用户可见字面量；
  `GtkPasswordEntry` 的 `visibility` 固定为 false（输入不回显），且不提供回显切换图标（§6 零剪贴板/零回显）。
  全部文案在 PasswordDialog::setup() 里经 i18n 键赋值（locales/*.yml）。
-->
<interface>
  <requires lib="gtk" version="4.10"/>
  <requires lib="Adw" version="1.6"/>
  <template class="T7PasswordDialog" parent="AdwDialog">
    <property name="content-width">400</property>
    <property name="follows-content-size">true</property>
    <property name="child">
      <object class="AdwToolbarView">
        <child type="top">
          <object class="AdwHeaderBar">
            <property name="title-widget">
              <object class="AdwWindowTitle" id="dialog_title"/>
            </property>
          </object>
        </child>
        <property name="content">
          <object class="GtkBox">
            <property name="orientation">vertical</property>
            <property name="spacing">14</property>
            <property name="margin-top">18</property>
            <property name="margin-bottom">18</property>
            <property name="margin-start">18</property>
            <property name="margin-end">18</property>
            <child>
              <object class="GtkBox">
                <property name="orientation">horizontal</property>
                <property name="spacing">12</property>
                <child>
                  <object class="GtkImage" id="dialog_icon">
                    <property name="icon-name">dialog-password-symbolic</property>
                    <property name="pixel-size">32</property>
                    <property name="valign">start</property>
                    <style>
                      <class name="dim-label"/>
                    </style>
                  </object>
                </child>
                <child>
                  <object class="GtkLabel" id="body_label">
                    <property name="xalign">0</property>
                    <property name="halign">start</property>
                    <property name="hexpand">true</property>
                    <property name="wrap">true</property>
                    <style>
                      <class name="dim-label"/>
                    </style>
                  </object>
                </child>
              </object>
            </child>
            <child>
              <!-- GtkPasswordEntry 本身不提供回显开关（无 `visibility` 属性）：输入恒不回显；
                   这里再关闭「显示明文」图标，使对话框中不存在任何回显通路（§4.12/§6）。 -->
              <object class="GtkPasswordEntry" id="entry">
                <property name="show-peek-icon">false</property>
                <property name="activates-default">true</property>
                <property name="hexpand">true</property>
              </object>
            </child>
            <child>
              <object class="GtkLabel" id="error_label">
                <property name="xalign">0</property>
                <property name="halign">start</property>
                <property name="wrap">true</property>
              </object>
            </child>
            <child>
              <object class="GtkBox">
                <property name="orientation">horizontal</property>
                <property name="spacing">8</property>
                <property name="halign">end</property>
                <child>
                  <object class="GtkButton" id="cancel"/>
                </child>
                <child>
                  <object class="GtkButton" id="submit">
                    <style>
                      <class name="suggested-action"/>
                    </style>
                  </object>
                </child>
              </object>
            </child>
          </object>
        </property>
      </object>
    </property>
  </template>
</interface>
```

`password_dialog.rs` 的 `show_error`：末尾追加 `self.imp().error_label.add_css_class("error");`（幂等，重复添加同类无副作用）。

- [ ] **Step 7: locales 新增 6 键（两份文件同键）**

zh-CN.yml:

```yaml
nav:
  toggle_sidebar: "切换侧边栏"
device:
  channel_title: "传输通道"
  descriptor_title: "USB 备用设置"
  empty_hint: "连接 Samsung Portable SSD T7 Shield 后将自动识别，无需手动刷新。"
dashboard:
  actions_title: "操作"
  feedback_title: "进度与结果"
```

en.yml:

```yaml
nav:
  toggle_sidebar: "Toggle Sidebar"
device:
  channel_title: "Transport channel"
  descriptor_title: "USB alternate setting"
  empty_hint: "Connect a Samsung Portable SSD T7 Shield and it will be detected automatically."
dashboard:
  actions_title: "Actions"
  feedback_title: "Progress & Result"
```

（并入既有同名 section，不要重复 section 头。)

- [ ] **Step 8: 结构测试扩充**

`test_ui_templates_declare_required_children` 的 required 列表追加：`"device_area_stack"`、`"empty_state"`、`"progress_box"`、`"status_icon"`、`"result_icon"`、`"sidebar_toggle"`。

- [ ] **Step 9: 验证**

Run: `nix develop -c cargo test --workspace && nix develop -c cargo clippy --workspace -- -D warnings`
Expected: 全绿、零警告。

- [ ] **Step 10: Commit**

```bash
git add crates/magi-app/
git commit -S -m "<按 .claude/commands/commit-message.md 生成；type: refactor 或 feat>"
```

---

### Task 3: 设置功能（主题/语言切换 + 持久化 + 运行时重渲染）

**Files:**
- Create: `crates/magi-app/src/settings.rs`
- Create: `crates/magi-app/src/settings_dialog.rs`
- Create: `crates/magi-app/src/ui/settings_dialog.ui`
- Modify: `crates/magi-app/src/lib.rs`(mod 声明、run() 装配、init_locale 让位）
- Modify: `crates/magi-app/src/main_window.rs`(`action_preferences` 子件与接线、`relocalize()`、WindowState 三处缓存）
- Modify: `crates/magi-app/src/password_dialog.rs`（存 action、`relocalize()`)
- Modify: `crates/magi-app/src/ui/main_window.ui`(HeaderBar 末端加 `action_preferences` 按钮）
- Modify: `crates/magi-app/locales/zh-CN.yml`、`en.yml`（新增 settings.* 与 action.preferences 共 9 键）
- Modify: `docs/specs/t7-magician/spec.md` §10 锚点表、`docs/specs/t7-magician/tools/audit_manifest.json` test_anchors（与代码同批）

**Interfaces:**
- Consumes: Task 2 的全部子件 id 与 `Outcome`/`result_kind`/`show_outcome`/`status_icon_name`;Task 1 的 D28 条款。
- Produces:
  - `settings::Settings { theme: ThemePreference, language: LanguagePreference }`,`Settings::load() -> Self`、`Settings::save(&self)`、`Settings::color_scheme(&self) -> adw::ColorScheme`、`Settings::apply_theme(&self)`、`Settings::resolve_locale(&self, env_tag: Option<&str>) -> &'static str`;
  - `MainWindow::relocalize(&self)`;
  - `PasswordDialog::relocalize(&self)`;
  - 锚点测试：`test_settings_keyfile_roundtrip`、`test_language_precedence`、`test_theme_default_is_dark`、`test_settings_dialog_instantiates_with_template`。

- [ ] **Step 1: `settings.rs`（纯逻辑，可在无 GTK 环境测试的部分与 GTK 应用分离）**

```rust
//! D28 应用内设置：主题与语言的读取、持久化（glib KeyFile）与运行时应用。
//!
//! 持久化路径：`glib::user_config_dir()/magi/settings.ini`，组 `[ui]`，键 `language`/`theme`。
//! 读写失败一律回落默认值（默认深色主题、语言跟随系统）；设置文件只含两个枚举值（§6/AC-011）。

use std::path::{Path, PathBuf};

use gtk4 as gtk;
use libadwaita as adw;

/// 主题三态（D28）：默认深色（Samsung Magician 观感，§4.11 深色侧边栏判据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// 跟随系统。
    System,
    /// 浅色。
    Light,
    /// 深色（默认）。
    #[default]
    Dark,
}

/// 语言三态（D28）：默认跟随系统（环境变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguagePreference {
    /// 跟随系统（环境变量）。
    #[default]
    System,
    /// 中文。
    ZhCn,
    /// English。
    En,
}

impl ThemePreference {
    fn from_value(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl LanguagePreference {
    fn from_value(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "zh-CN" => Some(Self::ZhCn),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }
}

/// 应用内设置（D28）：两项选择；读取失败/字段非法时单项回落默认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub theme: ThemePreference,
    pub language: LanguagePreference,
}

impl Settings {
    /// 从默认路径读取（不存在或损坏 → 全默认）。
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// 默认配置文件路径：`$XDG_CONFIG_HOME/magi/settings.ini`。
    pub fn path() -> PathBuf {
        gtk::glib::user_config_dir().join("magi").join("settings.ini")
    }

    /// 从指定路径读取（测试注入点）。
    fn load_from(path: &Path) -> Self {
        let key_file = gtk::glib::KeyFile::new();
        if key_file.load_from_file(path, gtk::glib::KeyFileFlags::NONE).is_err() {
            return Self::default();
        }
        let theme = key_file
            .string("ui", "theme")
            .ok()
            .and_then(|value| ThemePreference::from_value(&value))
            .unwrap_or_default();
        let language = key_file
            .string("ui", "language")
            .ok()
            .and_then(|value| LanguagePreference::from_value(&value))
            .unwrap_or_default();
        Self { theme, language }
    }

    /// 写入默认路径；父目录缺失时创建；写失败只记诊断、不打断交互（内存态已生效）。
    pub fn save(&self) {
        if let Some(parent) = Self::path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let key_file = gtk::glib::KeyFile::new();
        key_file.set_string("ui", "theme", self.theme.value());
        key_file.set_string("ui", "language", self.language.value());
        if let Err(error) = key_file.save_to_file(Self::path()) {
            crate::diagnostics::ring().record(
                crate::diagnostics::Level::Warn,
                &format!("settings save failed: {error}"),
            );
        }
    }

    /// 主题 → 颜色方案映射（纯函数，可在无 GTK 环境断言）。
    pub fn color_scheme(&self) -> adw::ColorScheme {
        match self.theme {
            ThemePreference::System => adw::ColorScheme::Default,
            ThemePreference::Light => adw::ColorScheme::ForceLight,
            ThemePreference::Dark => adw::ColorScheme::ForceDark,
        }
    }

    /// 应用主题到全局样式管理器（须 GTK 已初始化；测试不调）。
    pub fn apply_theme(&self) {
        adw::StyleManager::default().set_color_scheme(self.color_scheme());
    }

    /// 语言优先级（D28）：应用内显式选择 > 环境变量 > 默认 zh-CN。
    pub fn resolve_locale(&self, env_tag: Option<&str>) -> &'static str {
        match self.language {
            LanguagePreference::ZhCn => "zh-CN",
            LanguagePreference::En => "en",
            LanguagePreference::System => crate::presentation::locale_for_tag(env_tag),
        }
    }
}
```

模块测试（`settings.rs` 内 `#[cfg(test)]`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 持久化往返：写入临时路径后读回，两个枚举值保持一致。
    #[test]
    fn test_settings_keyfile_roundtrip() {
        let dir = std::env::temp_dir().join(format!("magi-settings-test-{}", std::process::id()));
        let path = dir.join("settings.ini");
        let settings = Settings { theme: ThemePreference::Light, language: LanguagePreference::En };
        let key_file = gtk::glib::KeyFile::new();
        key_file.set_string("ui", "theme", settings.theme.value());
        key_file.set_string("ui", "language", settings.language.value());
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        key_file.save_to_file(&path).expect("写入设置文件");
        assert_eq!(Settings::load_from(&path), settings);
        // 损坏文件 → 全默认，不 panic。
        std::fs::write(&path, b"not a key file {{{").expect("写入损坏文件");
        assert_eq!(Settings::load_from(&path), Settings::default());
        // 不存在的文件 → 全默认。
        assert_eq!(Settings::load_from(&dir.join("missing.ini")), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 语言优先级（D28）：显式选择 > 环境变量 > 默认 zh-CN。
    #[test]
    fn test_language_precedence() {
        let zh = Settings { language: LanguagePreference::ZhCn, ..Default::default() };
        assert_eq!(zh.resolve_locale(Some("en_US.UTF-8")), "zh-CN");
        let en = Settings { language: LanguagePreference::En, ..Default::default() };
        assert_eq!(en.resolve_locale(None), "en");
        let sys = Settings::default();
        assert_eq!(sys.resolve_locale(Some("en_US.UTF-8")), "en");
        assert_eq!(sys.resolve_locale(None), "zh-CN");
        assert_eq!(sys.resolve_locale(Some("ja_JP.UTF-8")), "zh-CN");
    }

    /// 默认主题为深色（D28 / §4.11 深色侧边栏判据）。
    #[test]
    fn test_theme_default_is_dark() {
        assert_eq!(Settings::default().theme, ThemePreference::Dark);
        assert_eq!(Settings::default().color_scheme(), adw::ColorScheme::ForceDark);
        assert_eq!(Settings { theme: ThemePreference::System, ..Default::default() }.color_scheme(), adw::ColorScheme::Default);
        assert_eq!(Settings { theme: ThemePreference::Light, ..Default::default() }.color_scheme(), adw::ColorScheme::ForceLight);
    }
}
```

- [ ] **Step 2: `ui/settings_dialog.ui`（逐字；无字面量、候选项不在 .ui）**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!--
  D28 设置对话框模板（K5）：只承载结构与 id；组标题、行标题与全部候选项文案
  都在 SettingsDialog 侧经 i18n 键赋值（locales/*.yml），模板不含任何字面量。
-->
<interface>
  <requires lib="gtk" version="4.10"/>
  <requires lib="Adw" version="1.6"/>
  <template class="MagiSettingsDialog" parent="AdwPreferencesDialog">
    <property name="content-width">420</property>
    <property name="content-height">360</property>
    <property name="child">
      <object class="AdwPreferencesPage">
        <child>
          <object class="AdwPreferencesGroup" id="appearance_group">
            <child>
              <object class="AdwComboRow" id="theme_row"/>
            </child>
            <child>
              <object class="AdwComboRow" id="language_row"/>
            </child>
          </object>
        </child>
      </object>
    </property>
  </template>
</interface>
```

- [ ] **Step 3: `settings_dialog.rs`**

```rust
//! D28 设置对话框（`CompositeTemplate`）：主题/语言三态选择，更改即时生效并持久化。
//!
//! 候选项文案经 `gtk::StringList` + `t!()` 在 Rust 侧构造（`.ui` 零字面量，K5）；
//! 语言切换后对话框自身与主窗口同步重渲染（relocalize）。

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::CompositeTemplate;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::main_window::MainWindow;
use crate::settings::{LanguagePreference, Settings, ThemePreference};

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "ui/settings_dialog.ui")]
    pub struct SettingsDialog {
        #[template_child]
        pub appearance_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub theme_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub language_row: TemplateChild<adw::ComboRow>,
        pub(crate) settings: std::cell::RefCell<Settings>,
        pub(crate) window: std::cell::RefCell<Option<glib::WeakRef<MainWindow>>>,
        /// 装配期间屏蔽 notify::selected 回调（初始 selected 写入不应触发持久化）。
        pub(crate) loading: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SettingsDialog {
        const NAME: &'static str = "MagiSettingsDialog";
        type Type = super::SettingsDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SettingsDialog {}
    impl WidgetImpl for SettingsDialog {}
    impl adw::subclass::prelude::DialogImpl for SettingsDialog {}
    impl adw::subclass::prelude::PreferencesDialogImpl for SettingsDialog {}
}

glib::wrapper! {
    /// 设置对话框（D28）。
    pub struct SettingsDialog(ObjectSubclass<imp::SettingsDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

/// 主题三态在 ComboRow 中的行序（0/1/2）。
fn theme_index(theme: ThemePreference) -> u32 {
    match theme {
        ThemePreference::System => 0,
        ThemePreference::Light => 1,
        ThemePreference::Dark => 2,
    }
}

fn theme_from_index(index: u32) -> ThemePreference {
    match index {
        1 => ThemePreference::Light,
        2 => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

/// 语言三态在 ComboRow 中的行序（0/1/2）。
fn language_index(language: LanguagePreference) -> u32 {
    match language {
        LanguagePreference::System => 0,
        LanguagePreference::ZhCn => 1,
        LanguagePreference::En => 2,
    }
}

fn language_from_index(index: u32) -> LanguagePreference {
    match index {
        1 => LanguagePreference::ZhCn,
        2 => LanguagePreference::En,
        _ => LanguagePreference::System,
    }
}

impl SettingsDialog {
    /// 构造对话框：装配文案、候选项与当前选中，并接线即时生效回调。
    pub fn new(window: &MainWindow, settings: Settings) -> Self {
        let dialog: Self = glib::Object::new();
        *dialog.imp().window.borrow_mut() = Some(window.downgrade());
        *dialog.imp().settings.borrow_mut() = settings;
        dialog.imp().loading.set(true);
        dialog.relocalize();
        dialog.imp().theme_row.set_selected(theme_index(settings.theme));
        dialog.imp().language_row.set_selected(language_index(settings.language));
        dialog.imp().loading.set(false);
        dialog.connect_rows();
        dialog
    }

    /// 语言切换后重设对话框自身文案（组/行标题与候选项，选中态保持）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        self.set_title(&t!("settings.title"));
        imp.appearance_group.set_title(&t!("settings.group"));
        imp.theme_row.set_title(&t!("settings.theme"));
        imp.language_row.set_title(&t!("settings.language"));
        let theme = theme_from_index(imp.theme_row.selected());
        let language = language_from_index(imp.language_row.selected());
        imp.loading.set(true);
        imp.theme_row.set_model(Some(&gtk::StringList::new(&[
            &t!("settings.theme.system"),
            &t!("settings.theme.light"),
            &t!("settings.theme.dark"),
        ])));
        imp.theme_row.set_selected(theme_index(theme));
        imp.language_row.set_model(Some(&gtk::StringList::new(&[
            &t!("settings.language.system"),
            &t!("settings.language.zh"),
            &t!("settings.language.en"),
        ])));
        imp.language_row.set_selected(language_index(language));
        imp.loading.set(false);
    }

    /// 选择回调：即时应用 + 持久化 + 主窗口重渲染（装配期屏蔽）。
    fn connect_rows(&self) {
        let dialog = self.clone();
        self.imp().theme_row.connect_selected_notify(move |row| {
            let imp = dialog.imp();
            if imp.loading.get() {
                return;
            }
            let mut settings = imp.settings.borrow_mut();
            settings.theme = theme_from_index(row.selected());
            settings.apply_theme();
            settings.save();
        });
        let dialog = self.clone();
        self.imp().language_row.connect_selected_notify(move |row| {
            let imp = dialog.imp();
            if imp.loading.get() {
                return;
            }
            let env_tag = std::env::var("LC_ALL")
                .or_else(|_| std::env::var("LC_MESSAGES"))
                .or_else(|_| std::env::var("LANG"))
                .ok();
            let window = imp.window.borrow().as_ref().and_then(|weak| weak.upgrade());
            {
                let mut settings = imp.settings.borrow_mut();
                settings.language = language_from_index(row.selected());
                rust_i18n::set_locale(settings.resolve_locale(env_tag.as_deref()));
                settings.save();
            }
            dialog.relocalize();
            if let Some(window) = window {
                window.relocalize();
            }
        });
    }

    /// 当前设置快照（主窗口启动时装配初始态用）。
    pub fn settings(&self) -> Settings {
        *self.imp().settings.borrow()
    }
}
```

- [ ] **Step 4: main_window.ui 加 HeaderBar 末端按钮**

在 `</object>` 结束 AdwHeaderBar 前（`title-widget` 之后）插入：

```xml
            <child type="end">
              <object class="GtkButton" id="action_preferences">
                <property name="icon-name">emblem-system-symbolic</property>
              </object>
            </child>
```

- [ ] **Step 5: main_window.rs 接线**

1. imp 新增 `pub action_preferences: TemplateChild<gtk::Button>`(gtk::Button)。
2. `WindowState` 新增三处缓存（定义见 Step 6),`Settings` 也由窗口持有：

```rust
    /// 结果区重放缓存（语言切换后按新语言重放）。
    last_outcome: Option<StoredOutcome>,
    /// 最后一次设备扫描命中（relocalize 重放设备卡文案用）。
    last_hit: Option<crate::jobs::ScanHit>,
    /// 打开中的口令对话框（语言切换时同步重渲染）。
    open_dialog: Option<glib::WeakRef<PasswordDialog>>,
```

`StoredOutcome`（文件内私有）:

```rust
/// 结果区呈现的重放数据（relocalize 用）。
#[derive(Debug, Clone)]
enum StoredOutcome {
    /// 固定结果键（result.* / progress.idle 之外的结果）。
    Result(&'static str),
    /// 已解锁并挂载（含挂载点参数）。
    Mounted(Vec<String>),
    /// 错误（呈现码 + 原因/建议键由 AppError 推导）。
    Error(AppError),
}
```

若 `AppError` 未实现 `Clone`，在 presentation.rs 为其 derive（它是纯数据枚举，derive 即可）。

3. `show_hit`：写入 `state.last_hit = Some(hit.clone())`(ScanHit 已 Clone)。
4. `show_result`：`state.last_outcome = Some(StoredOutcome::Result(message_key))`(success/neutral 均记）。
5. `show_error`：`state.last_outcome = Some(StoredOutcome::Error(error.clone()))`。
6. `finish` mounted 分支：`state.last_outcome = Some(StoredOutcome::Mounted(mounted.clone()))`。
7. `prompt_password`:`state.open_dialog = Some(dialog.downgrade())`。
8. setup() 追加：`imp.action_preferences.set_tooltip_text(Some(&t!("action.preferences")));`
9. connect_actions() 追加首选项接线：

```rust
        let this = self.clone();
        imp.action_preferences.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.present_preferences()
        ));
```

10. 新增两个方法：

```rust
    /// 打开设置对话框（D28）：装入当前设置快照；更改即时生效并回写本窗口。
    fn present_preferences(&self) {
        let settings = crate::settings::Settings::load();
        let dialog = crate::settings_dialog::SettingsDialog::new(self, settings);
        dialog.present(Some(self));
    }

    /// 语言切换后重设全部静态文案并重放动态呈现（D28：切换即时生效）。
    pub fn relocalize(&self) {
        let imp = self.imp();
        imp.window_title.set_title(&t!("app.title"));
        imp.window_title.set_subtitle(&t!("app.subtitle"));
        imp.brand_label.set_label(&t!("app.title"));
        imp.nav_dashboard_label.set_label(&t!(NavItem::Dashboard.label_key()));
        imp.nav_diagnostics_label.set_label(&t!(NavItem::Diagnostics.label_key()));
        imp.nav_about_label.set_label(&t!(NavItem::About.label_key()));
        imp.dashboard_title_label.set_label(&t!("dashboard.title"));
        imp.diagnostics_title_label.set_label(&t!("diagnostics.view_title"));
        imp.diagnostics_hint_label.set_label(&t!("diagnostics.hint"));
        imp.about_title_label.set_label(&t!("nav.about"));
        imp.about_version_label.set_label(&t!("about.version", version = env!("CARGO_PKG_VERSION")));
        imp.about_repository_label.set_label(&t!("about.repository"));
        imp.about_notice_label.set_label(&t!("about.notice"));
        imp.sidebar_notice_label.set_label(&t!("app.subtitle"));
        imp.device_group_title.set_label(&t!("device.group_title"));
        imp.device_model_label.set_label(&t!("device.model"));
        for action in ActionId::ALL {
            if let Some(button) = self.action_button(action) {
                button.set_label(&t!(action.label_key()));
            }
        }
        imp.action_cancel.set_label(&t!("action.cancel"));
        imp.action_export_diagnostics.set_label(&t!("action.export_diagnostics"));
        imp.action_preferences.set_tooltip_text(Some(&t!("action.preferences")));
        imp.sidebar_toggle.set_tooltip_text(Some(&t!("nav.toggle_sidebar")));
        imp.device_node_caption.set_label(&t!("device.node"));
        imp.device_channel_caption.set_label(&t!("device.channel_title"));
        imp.device_descriptor_caption.set_label(&t!("device.descriptor_title"));
        imp.actions_caption.set_label(&t!("dashboard.actions_title"));
        imp.feedback_caption.set_label(&t!("dashboard.feedback_title"));
        imp.password_admin_note.set_label(&t!("reason.evidence_gap"));
        imp.empty_state.set_title(&t!(DeviceIdentity::Unrecognized.status_key()));
        imp.empty_state.set_description(Some(&t!("device.empty_hint")));
        // 设备卡重放：有缓存命中按新语言重渲染（不走 refresh_devices——
        // RescanState 身份不变时返回 Keep，不会重刷文案）。
        let last_hit = self.state().borrow().last_hit.clone();
        match last_hit {
            Some(hit) => self.show_hit(&hit),
            None => self.show_unknown_device(),
        }
        self.show_platform_notice();
        self.refresh_actions();
        // 结果区重放。
        let last_outcome = self.state().borrow().last_outcome.clone();
        match last_outcome {
            Some(StoredOutcome::Result(key)) => self.show_result(key),
            Some(StoredOutcome::Mounted(volumes)) => {
                let text = format!(
                    "{} {}",
                    t!("result.RealPartitionTable"),
                    t!("result.mounted", volumes = volumes.join(", "))
                );
                self.show_outcome(&text, Outcome::Success);
            }
            Some(StoredOutcome::Error(error)) => {
                self.show_error(&error);
            }
            None => self.show_outcome(&t!("progress.idle"), Outcome::Neutral),
        }
        // 诊断页可见时重渲染；打开中的口令对话框同步重渲染。
        let current = imp.content_stack.visible_child_name().unwrap_or_default();
        if current.as_str() == NavItem::Diagnostics.page_name() {
            self.refresh_diagnostics_view();
        }
        let dialog = self.state().borrow().open_dialog.as_ref().and_then(|weak| weak.upgrade());
        if let Some(dialog) = dialog {
            dialog.relocalize();
        }
    }
```

注意：`show_result`/`show_error`/`show_hit` 本身会写缓存——重放时重复写同一值，幂等无害，不得再加守卫。

11. 模块文档头更新：提及 D28 设置入口与 relocalize 职责。

- [ ] **Step 6: password_dialog.rs 支持重渲染**

- imp 新增 `pub(crate) action: std::cell::Cell<ActionId>`(ActionId 已 Copy);`new()` 里 `imp.action.set(action)`。
- 新增：

```rust
    /// 语言切换后重设对话框文案（D28）：标题/说明/按钮/占位符全部重取。
    pub fn relocalize(&self) {
        let imp = self.imp();
        imp.dialog_title.set_title(&t!(imp.action.get().label_key()));
        imp.body_label.set_label(&t!("password.body"));
        imp.submit.set_label(&t!("password.submit"));
        imp.cancel.set_label(&t!("password.cancel"));
        imp.entry.set_placeholder_text(Some(&t!("password.placeholder")));
    }
```

（`new()` 的原有赋值保持不变；`relocalize` 与之同构。)

- [ ] **Step 7: lib.rs 装配**

- 新增 `pub mod settings;` 与 `pub mod settings_dialog;`（模块文档一行：D28)。
- `run()` 改为：

```rust
pub fn run() -> gtk::glib::ExitCode {
    let settings = settings::Settings::load();
    rust_i18n::set_locale(settings.resolve_locale(current_env_tag().as_deref()));
    let app = adw::Application::builder().application_id(APP_ID).build();
    settings.apply_theme();
    main_window::MainWindow::install(&app);
    app.run()
}

/// 环境变量语言标签（D28 语言优先级链的中间级）：LC_ALL → LC_MESSAGES → LANG。
fn current_env_tag() -> Option<String> {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .ok()
}
```

- 删除/改造 `init_locale()`：其环境读取逻辑并入 `current_env_tag()`；若测试引用 `init_locale`，相应更新（grep 确认）。`init_locale` 的公开性无 spec 契约，可直接替换。

- [ ] **Step 8: locales 新增 9 键（两份文件同键）**

zh-CN.yml:

```yaml
action:
  preferences: "设置"
settings:
  title: "设置"
  group: "界面"
  theme: "主题"
  theme:
    system: "跟随系统"
    light: "浅色"
    dark: "深色"
  language: "语言"
  language:
    system: "跟随系统"
    zh: "中文"
    en: "English"
```

（YAML 注意：`settings.theme` 既是字符串键又是父级会冲突——实际写法为扁平化还是嵌套以 rust-i18n 的解析为准；既有文件用嵌套风格。冲突规避：主题选项键用 `settings.theme_system/theme_light/theme_dark`，语言选项用 `settings.lang_system/lang_zh/lang_en`，避免同名父子冲突。代码里的键名同步用这套。)

en.yml 对应：

```yaml
action:
  preferences: "Settings"
settings:
  title: "Settings"
  group: "Interface"
  theme: "Theme"
  theme_system: "Follow system"
  theme_light: "Light"
  theme_dark: "Dark"
  language: "Language"
  lang_system: "Follow system"
  lang_zh: "中文"
  lang_en: "English"
```

实现者以「两份文件键集合完全一致 + 编译通过 + 键解析测试全绿」为准校正嵌套写法。

- [ ] **Step 9: 新测试锚点落地 + spec §10 与 manifest 同批**

1. `main_window.rs` 测试模块新增（gtk_ready 门控，与既有模板测试同款）:

```rust
    /// 锚点（D28）：设置对话框模板可实例化，两个三态行齐备。
    #[test]
    fn test_settings_dialog_instantiates_with_template() {
        if !crate::test_support::gtk_ready("test_settings_dialog_instantiates_with_template") {
            return;
        }
        let window = MainWindow::new();
        let dialog = crate::settings_dialog::SettingsDialog::new(&window, Default::default());
        assert_eq!(dialog.imp().theme_row.get().type_().name(), "AdwComboRow");
        assert_eq!(dialog.imp().language_row.get().type_().name(), "AdwComboRow");
        // 默认深色主题选中第 3 行；默认语言跟随系统选中第 1 行。
        assert_eq!(dialog.imp().theme_row.selected(), 2);
        assert_eq!(dialog.imp().language_row.selected(), 0);
    }
```

2. `test_ui_templates_declare_required_children` 追加对 `ui/settings_dialog.ui` 的同款扫描（required ids:`appearance_group`、`theme_row`、`language_row`;`assert_no_display_literals` 同扫）。
3. spec.md §10 锚点表追加四行：`test_settings_keyfile_roundtrip`、`test_language_precedence`、`test_theme_default_is_dark`（ crate `crates/magi-app`，判据分别为持久化往返/语言优先级/默认深色）、`test_settings_dialog_instantiates_with_template`（设置对话框模板与默认选中态）。
4. manifest `test_anchors` 按既有条目同款结构新增这四条（`source_globs: ["crates/magi-app/**/*.rs"]`,`required: true`)。

- [ ] **Step 10: 验证（全套）**

Run: `nix develop -c cargo test --workspace && nix develop -c cargo clippy --workspace -- -D warnings && python3 docs/specs/t7-magician/tools/test_audit_spec.py && python3 docs/specs/t7-magician/tools/audit_spec.py && python3 docs/specs/t7-magician/tools/barriers.py`
Expected: 全绿。

- [ ] **Step 11: Commit**

```bash
git add crates/magi-app/ docs/specs/t7-magician/
git commit -S -m "<按 .claude/commands/commit-message.md 生成；type: feat>"
```

---

### Task 4: 文案产品化重写（两份 locales 全量值替换）

**Files:**
- Modify: `crates/magi-app/locales/zh-CN.yml`
- Modify: `crates/magi-app/locales/en.yml`

**Interfaces:**
- Consumes: `.superpowers/sdd/2026-09-14-ui-overhaul-settings-copy/research-copy.md`（调查产物：§2 全键清单、§3 逐键诊断、§4 文案原则、§5 新文案草案、§6 新旧对照速查表）;Task 2/3 新增的键（research-copy.md 不含它们，按 §4 原则补齐）。
- Produces: 两份 locales 的最终产品文案（键名不变、键集合一致、占位符保留）。

- [ ] **Step 1: 读调查产物与现行 locales**

读 `.superpowers/sdd/2026-09-14-ui-overhaul-settings-copy/research-copy.md` 全文与两份 locales 现状（含 Task 2/3 新增键）。

- [ ] **Step 2: 应用草案**

按 research-copy.md §5 草案逐键替换两份文件的**值**；遵守硬约束：

- 键名不变；`%{version}`/`%{proto}`/`%{volumes}` 占位符保留；两文件键集合完全一致。
- `reason.*` 与 `advice.*` 的值内不得出现 `：` `；` 或 `;`（拼接格式 `{code}：{reason}；{advice}` 不动）。
- 术语统一：中文用「密码」（弃「口令」）;「重枚举」在用户层文案改为通俗表述，仅诊断页/技术语境可保留。
- 按钮文案（`action.*`）简短；`device.model`、`app.title` 不动；`app.subtitle` 的 VID:PID 身份信息按草案处理。
- 诊断页/导出相关键（`diagnostics.*`、呈现码 reason/advice 的技术细节）可保持技术向，但仍须通顺。
- Task 2/3 新增键（`nav.toggle_sidebar`、`device.channel_title`、`device.descriptor_title`、`device.empty_hint`、`dashboard.actions_title`、`dashboard.feedback_title`、`action.preferences`、`settings.*`）按 §4 原则给出最终文案；草案若与新增键语义重叠（如空态提示），以一致性为准收敛。

- [ ] **Step 3: 验证**

Run: `nix develop -c cargo test --workspace`
Expected: 全绿（键集合一致性、键解析、导航/进度键断言均通过）。若 password_dialog 的「：」断言失败，说明误改了拼接格式——恢复格式，不得改测试。

- [ ] **Step 4: Commit**

```bash
git add crates/magi-app/locales/
git commit -S -m "<按 .claude/commands/commit-message.md 生成；type: docs 或 refactor>"
```

---

## Self-Review 记录（计划作者自检）

- Spec 覆盖：D28 条款 → Task 1 落 spec、Task 3 落代码；§4.11 深色侧边栏/设备卡/徽章既有判据 → Task 2；语言优先级 → Task 3 Step 7；文案产品化（用户需求） → Task 4。无 spec 条款无任务对应。
- 占位符扫描：无 TBD/TODO；Task 4 的「按草案」依赖 research-copy.md 实物文件（已存在，585 行，86 键全覆盖）。
- 类型一致性：`Outcome`/`result_kind`/`status_icon_name`/`show_outcome`(Task 2)→ Task 3 relocalize 复用同名；`StoredOutcome` 三变体与 finish/show_result/show_error 写入点一一对应；settings 键名父子冲突已在 Task 3 Step 8 规避（theme_system 等扁平键）。
- 冲突扫描（任务间共享文件）:
  - main_window.rs:T2（重构）→ T3（加子件+relocalize)，串行，接口 = T2 的子件 id 表与 show_outcome;
  - main_window.ui:T2 重写，T3 只加 HeaderBar 末端按钮块；
  - locales 两文件：T2/T3 加键，T4 改全部值，串行无冲突；
  - spec.md/manifest:T1（§4.11/§6/§9/decisions)与 T3(§10/test_anchors）改不同段落。
- 遗留调查事项（不阻塞，记录备查）:CopyAudit 发现 `limit.devices_exceeded` 译值未被渲染（jobs.rs 记原始键名）、en 错误拼接用全角标点——属既有实现问题，不在本计划范围；终评审时复核是否登记 issue。
