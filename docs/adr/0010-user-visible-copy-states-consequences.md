# User-visible copy states consequences, and a label is not a manual

Status: accepted (2026-10-01)

## Context

The Agent's screens had grown a manual inside themselves. Two properties of the
code kept feeding it:

- **Every screen was required to say something.** `PageHeader` demanded
  `description: string`, `EmptyState` demanded `text: string` and `SettingRow`
  demanded `description: string`. A page whose title already said everything
  still had to fill a line, and a line that exists tends to grow: 运行日志
  became "查看最近的运行事件与错误。", and the SVN account row became "用于访问
  公司 SVN 中当前账号有权限的项目仓库。".
- **Explaining a default was the dominant shape.** "HiMind Agent 会定期检查
  更新。" and "这些工具会在对话里提供给 HiMind AI 调用，停用后不再加载。" tell
  the reader nothing they can act on, and they bury the rows that do carry a
  consequence — a passphrase that cannot be recovered, a path outside the
  project, a publish that cannot be undone.

A sweep of `frontend/src` with the same literal scanner the check uses counted
about 3,400 user-visible Chinese strings. The longest settings rows and page
headers ran one or two clauses past the consequence they were carrying.

Two boundaries fixed the scope:

- **The marketplace is not this decision.** Extension descriptions are produced
  by the ECC sync and follow the metaguide
  (`himind-extensions/tooling/metaguide`): display name ≤ 18 characters,用途说明
  ≤ 120 with a 60 recommended value, written as "做什么 + 什么时候用". The 269
  skill entries sit at a median of 45 characters, and the market list clamps
  them to two lines. Shortening them here would break a documented contract to
  fix a layout the clamp already handles.
- **Safety copy needs its sentence.** "不可恢复" plus the way back does not
  compress into 20 characters without losing the part that matters.

## Decision

Three rules, one component change, one machine check, and a CI step.

1. **Rules.** (a) Do not document default behaviour — write what changes when
   the user changes it. (b) Write the consequence, not an introduction to the
   feature. (c) If there is nothing to say, render nothing; never print a
   placeholder sentence.
2. **The component signatures make silence legal.** `PageHeader.description`,
   `EmptyState.text` and `SettingRow.description` are optional; an absent value
   renders no node at all, not an empty `<p>` or `<span>`. The empty second line
   was the layout defect the review started from.
3. **Budgets**, enforced by `frontend/scripts/check-copy.mts`:

   | 位置 | 预算（汉字数） |
   | --- | --- |
   | 页头说明 `PageHeader.description` | 20 |
   | 设置项说明 `SettingRow.description` | 24 |
   | 空状态说明 `EmptyState.text` | 40 |
   | 任意单条用户可见文案 | 60（硬上限） |
   | 全仓 28–39 字的文案条数 | ≤ 28 |
   | 全仓 ≥ 40 字的文案条数 | 0 |

4. **Blacklists.** Filler that carries no information (`帮你`, `让你`,
   `可以实现`, `需要注意的是`, `温馨提示`, `一站式` …) and placeholder
   sentences (`暂无说明。`, `未提供更新说明。` …) fail the check wherever they
   appear.
5. **CI runs it.** `.github/workflows/ci.yml` invokes `npm run check:ui`, whose
   chain ends with `check:copy`. Until this record the fifteen `check:*` scripts
   existed but nothing invoked them, so "the constraint is enforced" described a
   script that no one ran.

## Consequences

- **The check is a ratchet.** It fails on new bloat; the twenty surviving
  28–39-character strings are consequence-plus-escape-hatch sentences (the
  plugin failure state, the cancelled-workbench note, the global-versus-
  directory skill install) and are allowed to stay. Widening or narrowing that
  band is a deliberate edit to the script's ceiling, not an accident.
- **A new setting row over 24 characters fails before a reviewer reads it.**
  Copy review stops competing with everything else in review.
- **Feature-complete screens may render a title and nothing else.** That is the
  intended look, not an unfinished one; the market, 我的能力 and 运行日志 headers
  are the reference.
- **Duplicate wording is reported, not failed.** The check names strings that
  appear more than once so a reviewer can decide whether the repetition is
  meaningful.
- **The wording is not localised.** Budgets are counted in CJK characters, which
  is the language the UI ships in today; an i18n pass would need these budgets
  restated per locale.

## Alternatives rejected

- **A copy style guide in Markdown.** Not actionable: 3,400 literals and no gate
  is the state that produced this record, and the previous guidance lived in
  prose that no build step could read.
- **Ban every string longer than 20 characters.** Safety and irreversible-action
  warnings need a subject, a consequence and a way back. A flat ceiling would
  force those sentences to be split across two rows or trimmed into ambiguity;
  the band ceiling keeps the pressure without that cost.
- **Keep `description` required and pass `''`.** An empty string still renders
  the element, which is the empty second line the review began with.
- **Move the strings into an i18n table and check the table.** The check reads
  each literal next to the screen that owns it. A table would centralise the
  copy but decouple it from its context, and it is a larger change than the
  drift it prevents.
- **Apply the same budget to marketplace metadata.** Those descriptions follow
  the metaguide's "做什么 + 什么时候用" contract and are clamped to two lines in
  the list; a 40-character budget there would delete information the contract
  requires without changing what the user sees.
- **Delete the long strings instead of typing them optional.** Fourteen of the
  twenty carry a real branch (independent mode, a paused plugin, a cancelled
  workbench task). Deleting them would remove the only in-place explanation of
  what the user is looking at.
