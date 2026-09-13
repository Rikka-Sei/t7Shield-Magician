# tools/ 说明

本目录是 `docs/specs/t7-magician/` 这份 core spec 自带的验证工具，从 `.agents/skills/core-spec/templates/tools/` 复制而来，**随本 spec 独立维护，不与其他 spec 共享**。

| 文件 | 作用 | 何时必须 PASS |
|---|---|---|
| `audit_manifest.json` | 唯一事实源：术语、决策、代码契约、测试锚点、屏障命令 | 每次修改 spec 后同步更新 |
| `audit_spec.py` | 结构审计器（G1/G3/G4/G5 的机器可验证部分） | 每次修改 spec 后 |
| `test_audit_spec.py` | 审计器负例自测，保证审计器自己会失败 | 每次修改审计器规则后 |
| `barriers.py` | 验收屏障运行器 | spec 为 authoritative 时必须全绿 |

## 标准验证顺序

```bash
python tools/test_audit_spec.py   # 先证明审计器可信
python tools/audit_spec.py        # 再审计 spec
python tools/barriers.py          # 最后跑验收屏障（draft 下如实报告未接线）
```

## 状态约定

- `audit_manifest.json` 的 `spec_status` 为 `draft` 时：允许代码/测试锚点尚未落地，缺失锚点只告警。
- 切到 `authoritative` 前必须：锚点真实存在、契约 pattern 与代码一致、旧契约零命中、`barriers.py` 全绿。
- 三者任一失败，本 spec 不得宣称 authoritative。

## 本 spec 的路径与适配（相对模板的差异）

1. **路径**：manifest 位于 `docs/specs/t7-magician/tools/`，因此 `spec_file = "../spec.md"`、`repo_root = "../../../.."`（屏障命令以仓库根为工作目录执行）。
2. **测试锚点同时认 Rust**：`audit_spec.py` 的锚点匹配从 `func NAME(` 放宽为 `(?:func|fn) NAME(`。模板只认 Go，而本 spec 的目标实现是 Rust crate，不放宽则锚点在代码落地后永远无法命中。
3. **新增规则均有负例**：`test_audit_spec.py` 在模板 11 个用例之外新增 6 个用例，覆盖模板原本没有 fixture 的规则（`metadata_terms` 缺失、`scoped_terms` 越界、`forbidden_occurrences` 命中、`required_occurrences` 计数不足）以及 Go/Rust 两种锚点形式的正向命中。
4. **屏障在 draft 下的语义**：三条屏障分别探测 `crates/t7-protocol`、`crates/t7-transport`、`crates/t7-app`，并按退出码区分三种如实状态——`0` = 测试真实通过；`3` = crate 尚未创建（未接线，打印 `[planned] …`）；`4` = 当前 shell 缺少 `cargo`（打印 `[blocked] …`，需在 nix devshell 或 `nix develop` 内运行）。`barriers.py` 会把非零码直接上抛，不做任何掩盖。

## 变更规则

1. 任何行为变更：先改 `spec.md` 的决策日志，再改正文，然后同步本目录 manifest。
2. 禁止先改代码再补 spec；spec 与代码必须同批提交。
3. 增加审计规则时必须同时给 `test_audit_spec.py` 增加对应负例。
