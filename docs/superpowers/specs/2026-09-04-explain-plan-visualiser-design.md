# Explain Plan Visualiser — Design

**Scope:** Phase 3 Milestone 8 — Enhanced `EXPLAIN` (`docs/prd/phase-3-developer-productivity.md` §15,
FR3-019 Explain Analyze, FR3-020 Plan View). Read-only and production profile flags (FR3-021,
FR3-022) are **out of scope**; the safeguard shipped here is the statement-kind refusal below.

**Visual reference:** the design canvas *Explain Plan Visualiser*
(https://claude.ai/code/artifact/c8bb7db2-a4d4-4d9f-9c98-cc94b0f6fe5a) — three artboards drawn in the
app's own palette and dimensions. All three become switchable views over one plan model.

## Decisions

1. **One execution, structured output.** Explain and Explain Analyze both run
   `EXPLAIN (ANALYZE <bool>, FORMAT JSON) <statement>` once. The JSON is parsed into an
   engine-neutral `QueryPlan` in the PostgreSQL provider. There is no second round trip: an
   `ANALYZE` executes the statement, so it must run exactly once.
2. **The TEXT view is rendered from the model** in PostgreSQL's own text layout (`->` arrows,
   `(cost=…)`, `(actual time=…)`, property lines). It is what the PRD's "raw text plan" clause is
   satisfied by; it is not the server's byte-for-byte text output. The same lines are what
   COPY ALL copies, whichever view is showing.
3. **A plan travels inside `QueryResult`.** `QueryResult` gains `plan: Option<QueryPlan>`; the
   provider fills `columns`/`rows` with the rendered text lines as well, so the existing Text and
   Table renderers, the result window and the copy path work unchanged.
4. **Explain Analyze is its own command** (`sql.explain_analyse`, toolbar button "Explain Analyze",
   ⌥⇧⌘↵). The plain Explain command is untouched and can never request `ANALYZE` (FR-016).
5. **Safeguard:** Explain Analyze runs only when `analyse_statement` classifies the statement as
   `RowReturning` (`SELECT`, `WITH … SELECT`, `VALUES`). Anything else is refused *before* reaching
   the provider, with the refusal shown as a statement failure in the results and the status bar.
6. **Three views, one segment control.** When the result carries a plan, the results header shows
   `TREE | GRAPH | TEXT` in place of `TABLE | TEXT`. The choice lives on the editor
   (`EditorState::plan_display`), separate from `display`, so switching a table result never
   changes how a plan shows.
7. **Selection and detail.** Clicking a node in TREE or GRAPH selects it (`AppView::plan_selection`)
   and opens a 340 px detail panel on the right: heading, cost share, estimated/actual figures,
   every server property, and that node's own text lines. Clicking the selected node again clears
   the selection. A new result clears it.
8. **Metrics** (computed by the model, not the view):
   - exclusive cost = `max(0, total_cost − Σ children.total_cost)`
   - share = exclusive cost ÷ Σ exclusive costs (0 when the sum is 0)
   - hottest node = the largest exclusive cost (first on ties), only when it is > 0
   - estimate ratio = `(actual rows × loops) ÷ plan rows` when `ANALYZE` ran and plan rows > 0;
     the views flag ≥ 2 (`↑N×`) and ≤ 0.5 (`↓N×`) in amber.
9. **Graph geometry is computed by the model** as columns (depth) and rows (leaf order; an inner
   node sits at the mean of its first and last child); the view multiplies by pixel pitches and
   draws elbow connectors from thin quads, with thickness proportional to `log10(rows)`.
10. **No virtualisation for plans.** Plans are tens of nodes, not hundreds of thousands of rows;
    the tree and graph render every node and rely on the pane's scrolling.

## Colours and dimensions (from `src/ui.rs`)

Hottest node: 3 px `RED` left edge (the error card's strip) and a `RED` cost bar. Share ≥ 5 %:
`WARN` bar. Otherwise `FAINT`. Row height `RESULT_ROW_HEIGHT` (46). Graph node 132 × 118, column
pitch 148, row pitch 160, connectors `PANEL_HIGH`. Detail panel 340 px, `border_l` `BORDER`.

## Layering

```
ui.rs            plan_segments · plan_tree · plan_graph · plan_detail · plan_selection
application.rs   command::EXPLAIN_ANALYSE · CommandService::explain_analyse · PlanDisplay
database.rs      DatabaseProvider::explain(sql, analyse) -> QueryResult
plan.rs          QueryPlan · PlanNode · PlanRow metrics · as_text · graph_layout
postgres.rs      explain(): runs EXPLAIN (… FORMAT JSON), delegates to postgres/plan.rs
postgres/plan.rs parse_plan(json) — the only place that knows PostgreSQL's JSON keys
```
