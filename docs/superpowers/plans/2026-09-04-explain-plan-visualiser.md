# Explain Plan Visualiser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render `EXPLAIN` and `EXPLAIN ANALYZE` output as a structured plan — switchable TREE, GRAPH and TEXT views with a per-node detail panel — and add Explain Analyze as its own, guarded command.

**Architecture:** A GPUI-independent `QueryPlan` model in `src/plan.rs` computes per-node metrics, renders the text form, and lays the graph out as columns/rows. The PostgreSQL provider gains `explain(sql, analyse)`, which runs `EXPLAIN (… FORMAT JSON)` once and parses the JSON in `src/postgres/plan.rs`. `QueryResult` carries `plan: Option<QueryPlan>` alongside the rendered text lines, so every existing result path keeps working; `src/ui.rs` adds the three views over it.

**Tech Stack:** Rust edition 2024, GPUI 0.2.2, tokio-postgres, serde_json (with `preserve_order`), async-trait.

**Spec:** [`docs/superpowers/specs/2026-09-04-explain-plan-visualiser-design.md`](../specs/2026-09-04-explain-plan-visualiser-design.md) — read it first; the visual reference it links is the design canvas.

## Global Constraints

- **British spelling** in code, comments and documentation: `analyse`, `visualiser`, `colour`. The SQL keyword stays `ANALYZE`, and the toolbar label is the keyword: "Explain Analyze".
- **Author is Paul Snow; version 0.0.0.**
- **Cite the requirement** in doc comments: `FR3-019` (Explain Analyze), `FR3-020` (Plan View), `FR-016` (plain Explain never analyses), `§15.2`.
- **TDD**: write the failing test, run it to see it fail, implement minimally, run it to see it pass, commit.
- **Explain (plain) must never send `ANALYZE`** (FR-016). Only `CommandService::explain_analyse` may pass `analyse = true`.
- **Explain Analyze runs only `StatementKind::RowReturning` statements**; anything else is refused before the provider is called.
- **PostgreSQL JSON keys live only in `src/postgres/plan.rs`.** `src/plan.rs` and `src/ui.rs` never see a JSON key or a `serde_json::Value`.
- **GPUI holds no database logic.** Views render `QueryPlan` values via `plan.rows()` and `plan.graph_layout()`; they compute no metrics themselves.
- **Credentials and statement text are never logged** beyond what `execute` already logs.
- **After every task**: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must all be clean before committing.
- **Commit messages**: imperative summary line, a body explaining why, no co-author or tool trailers (see recent `git log` for the house style).
- All commands below run from the repository root `/home/paul/gitHUB/rusty-sql-tool`.

## File Structure

| File | Responsibility |
|---|---|
| `src/plan.rs` (new) | `QueryPlan`, `PlanNode`, `ActualStatistics`; `rows()` metrics; `as_text()`; `graph_layout()`; `into_result()`. No GPUI, no driver, no JSON. |
| `src/postgres/plan.rs` (new) | `parse_plan(json: &str) -> Result<QueryPlan, QueryError>` — the only code that knows PostgreSQL's JSON keys. |
| `src/postgres.rs` | `DatabaseProvider::explain` implementation: runs the statement, parses, wraps. Adds `pub mod plan;`. |
| `src/database.rs` | `DatabaseProvider::explain(sql, analyse)` trait method. |
| `src/result.rs` | `QueryResult::plan: Option<QueryPlan>`. |
| `src/sql.rs` | `prepare_explain(sql, analyse)` builds the `EXPLAIN (…, FORMAT JSON)` statement. |
| `src/application.rs` | `command::EXPLAIN_ANALYSE`, `PlanDisplay`, `EditorState::plan_display`, `CommandService::explain_analyse`. |
| `src/ui.rs` | Toolbar button, shortcut, `RunMode::ExplainAnalyse`, `plan_segments`, `plan_tree`, `plan_graph`, `plan_detail`, `plan_selection`. |
| `src/lib.rs` | `pub mod plan;` |
| `tests/postgres_smoke.rs` | Live `explain`/`explain_analyse` assertions (ignored by default). |
| `CLAUDE.md`, `README.md` | Module map, commands, shortcuts. |

## Shared test fixture

Task 1 defines `crate::plan::sample_plan()` (`#[cfg(test)] pub(crate)`) — the seven-node plan from the design canvas. Task 2's parser test asserts `parse_plan(SAMPLE_PLAN_JSON) == sample_plan()`, and the UI tests hand `sample_plan().into_result(…)` back from the fake provider. Keep all three in step: if you change a number in one, change it in the others.

Exclusive costs of the sample, used by several tests: Limit 0 (its total is below its child's), Sort 119.30, HashAggregate 192.10, Hash Join 268.11, Seq Scan on orders 4102.00, Hash 0, Seq Scan on customer 152.00. Sum 4833.51. The Seq Scan on orders is the hottest node with share 4102 ÷ 4833.51 ≈ 0.8487, and its estimate ratio is 48317 ÷ 21092 ≈ 2.29.

---

### Task 1: The plan model — `src/plan.rs`

**Files:**
- Create: `src/plan.rs`
- Modify: `src/lib.rs` (add `pub mod plan;` beside `pub mod definition;`)

**Interfaces:**
- Produces:
  - `pub struct QueryPlan { pub root: PlanNode, pub analysed: bool, pub planning_time_ms: Option<f64>, pub execution_time_ms: Option<f64> }`
  - `pub struct PlanNode { pub operation: String, pub target: Option<String>, pub startup_cost: f64, pub total_cost: f64, pub plan_rows: f64, pub plan_width: u64, pub actual: Option<ActualStatistics>, pub properties: Vec<(String, String)>, pub children: Vec<PlanNode> }`
  - `pub struct ActualStatistics { pub startup_time_ms: f64, pub total_time_ms: f64, pub rows: f64, pub loops: f64 }`
  - `impl PlanNode { pub fn heading(&self) -> String; pub fn summary_line(&self) -> String; pub fn text_lines(&self, heading_column: usize) -> Vec<String> }`
  - `pub struct PlanRow<'a> { pub index: usize, pub depth: usize, pub node: &'a PlanNode, pub exclusive_cost: f64, pub share: f64, pub hottest: bool, pub estimate_ratio: Option<f64> }`
  - `impl QueryPlan { pub fn rows(&self) -> Vec<PlanRow<'_>>; pub fn node_count(&self) -> usize; pub fn node(&self, index: usize) -> Option<&PlanNode>; pub fn as_text(&self) -> String; pub fn graph_layout(&self) -> GraphLayout; pub fn into_result(self, execution_time: Duration) -> QueryResult }`
  - `pub struct GraphLayout { pub placements: Vec<GraphPlacement>, pub edges: Vec<GraphEdge>, pub columns: usize, pub leaf_rows: usize }`, `pub struct GraphPlacement { pub index: usize, pub column: usize, pub row: f32 }`, `pub struct GraphEdge { pub parent: usize, pub child: usize, pub rows: f64 }`
  - `#[cfg(test)] pub(crate) fn sample_plan() -> QueryPlan`
- Consumes: `crate::result::{QueryResult, Column, CellValue, ExecutionStatus}` — `into_result` depends on Task 3 adding `plan` to `QueryResult`. **Write `into_result` in Task 3, not here.**

- [ ] **Step 1: Write the failing tests**

Create `src/plan.rs` containing only the test module and the fixture for now:

```rust
//! The execution plan model (FR3-020). Engine-neutral: the PostgreSQL provider builds it, the
//! views render it, and neither side sees the other.

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn rows_are_in_preorder_with_their_depth() {
        let plan = sample_plan();
        let rows = plan.rows();
        let headings: Vec<(usize, String)> = rows
            .iter()
            .map(|row| (row.depth, row.node.heading()))
            .collect();
        assert_eq!(
            headings,
            vec![
                (0, "Limit".to_owned()),
                (1, "Sort".to_owned()),
                (2, "HashAggregate".to_owned()),
                (3, "Hash Join".to_owned()),
                (4, "Seq Scan on orders o".to_owned()),
                (4, "Hash".to_owned()),
                (5, "Seq Scan on customer c".to_owned()),
            ]
        );
        assert_eq!(rows.iter().map(|row| row.index).collect::<Vec<_>>(), (0..7).collect::<Vec<_>>());
        assert_eq!(plan.node_count(), 7);
        assert_eq!(plan.node(4).map(PlanNode::heading).as_deref(), Some("Seq Scan on orders o"));
        assert!(plan.node(7).is_none());
    }

    /// Exclusive cost is what a node itself cost; a node cheaper than its child (Limit) is zero,
    /// never negative.
    #[test]
    fn exclusive_cost_and_share_are_derived_from_children() {
        let plan = sample_plan();
        let rows = plan.rows();
        let exclusive: Vec<f64> = rows.iter().map(|row| row.exclusive_cost).collect();
        for (actual, expected) in exclusive.iter().zip([0.0, 119.30, 192.10, 268.11, 4102.00, 0.0, 152.00]) {
            assert_close(*actual, expected);
        }
        assert_close(rows.iter().map(|row| row.share).sum::<f64>(), 1.0);
        assert_close(rows[4].share, 4102.00 / 4833.51);
    }

    #[test]
    fn the_hottest_node_is_the_largest_exclusive_cost() {
        let plan = sample_plan();
        let hottest: Vec<usize> = plan.rows().iter().filter(|row| row.hottest).map(|row| row.index).collect();
        assert_eq!(hottest, vec![4]);
    }

    #[test]
    fn a_plan_with_no_cost_has_no_hottest_node_and_zero_shares() {
        let plan = QueryPlan {
            root: PlanNode {
                operation: "Result".into(),
                ..PlanNode::default()
            },
            ..QueryPlan::default()
        };
        let rows = plan.rows();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].hottest);
        assert_close(rows[0].share, 0.0);
    }

    #[test]
    fn estimate_ratio_compares_actual_rows_over_all_loops_with_the_estimate() {
        let plan = sample_plan();
        let rows = plan.rows();
        assert_close(rows[4].estimate_ratio.unwrap(), 48317.0 / 21092.0);
        assert_close(rows[6].estimate_ratio.unwrap(), 1.0);

        let mut looped = sample_plan();
        looped.root.actual = Some(ActualStatistics {
            rows: 5.0,
            loops: 4.0,
            ..looped.root.actual.unwrap()
        });
        assert_close(looped.rows()[0].estimate_ratio.unwrap(), 2.0);

        let mut plain = sample_plan();
        plain.analysed = false;
        plain.root.actual = None;
        assert!(plain.rows()[0].estimate_ratio.is_none());
    }

    #[test]
    fn text_form_follows_the_postgresql_layout() {
        let text = sample_plan().as_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Limit  (cost=4821.14..4821.16 rows=10 width=48) (actual time=38.205..38.210 rows=10 loops=1)"
        );
        assert_eq!(
            lines[1],
            "  ->  Sort  (cost=4821.14..4833.51 rows=4948 width=48) (actual time=38.203..38.205 rows=10 loops=1)"
        );
        assert_eq!(lines[2], "        Sort Key: (sum(o.total)) DESC");
        assert_eq!(
            lines[6],
            "        ->  HashAggregate  (cost=4680.30..4714.21 rows=4948 width=48) (actual time=35.900..36.900 rows=4948 loops=1)"
        );
        assert_eq!(lines[7], "              Group Key: c.name");
        assert!(lines.contains(&"                    ->  Seq Scan on orders o  (cost=0.00..4102.00 rows=21092 width=24) (actual time=0.031..18.720 rows=48317 loops=1)"));
        assert_eq!(lines[lines.len() - 2], "Planning Time: 0.412 ms");
        assert_eq!(lines[lines.len() - 1], "Execution Time: 38.402 ms");
    }

    #[test]
    fn a_plain_plan_has_no_actual_clause_and_no_execution_time() {
        let mut plan = sample_plan();
        plan.analysed = false;
        plan.execution_time_ms = None;
        plan.root.actual = None;
        let text = plan.as_text();
        assert!(text.starts_with("Limit  (cost=4821.14..4821.16 rows=10 width=48)\n"));
        assert!(!text.contains("Execution Time"));
        assert!(text.ends_with("Planning Time: 0.412 ms"));
    }

    #[test]
    fn a_nodes_own_text_lines_carry_its_properties_but_not_its_children() {
        let plan = sample_plan();
        let lines = plan.node(4).unwrap().text_lines(0);
        assert_eq!(
            lines,
            vec![
                "Seq Scan on orders o  (cost=0.00..4102.00 rows=21092 width=24) (actual time=0.031..18.720 rows=48317 loops=1)".to_owned(),
                "  Filter: (placed_at >= (now() - '30 days'::interval))".to_owned(),
                "  Rows Removed by Filter: 151683".to_owned(),
            ]
        );
    }

    /// Leaves take consecutive rows; an inner node sits at the mean of its first and last child.
    #[test]
    fn graph_layout_places_nodes_by_depth_and_leaf_order() {
        let layout = sample_plan().graph_layout();
        let placed: Vec<(usize, usize, f32)> = layout
            .placements
            .iter()
            .map(|placement| (placement.index, placement.column, placement.row))
            .collect();
        assert_eq!(
            placed,
            vec![
                (0, 0, 0.5),
                (1, 1, 0.5),
                (2, 2, 0.5),
                (3, 3, 0.5),
                (4, 4, 0.0),
                (5, 4, 1.0),
                (6, 5, 1.0),
            ]
        );
        assert_eq!(layout.columns, 6);
        assert_eq!(layout.leaf_rows, 2);
        let edges: Vec<(usize, usize)> = layout.edges.iter().map(|edge| (edge.parent, edge.child)).collect();
        assert_eq!(edges, vec![(0, 1), (1, 2), (2, 3), (3, 4), (3, 5), (5, 6)]);
        // Edge weight is what flowed up the edge: actual rows when analysed.
        assert_close(layout.edges[3].rows, 48317.0);
    }

    #[test]
    fn graph_edges_fall_back_to_estimated_rows_for_a_plain_plan() {
        let mut plan = sample_plan();
        plan.analysed = false;
        fn strip(node: &mut PlanNode) {
            node.actual = None;
            node.children.iter_mut().for_each(strip);
        }
        strip(&mut plan.root);
        assert_close(plan.graph_layout().edges[3].rows, 21092.0);
    }
}
```

Then add the fixture **outside** `mod tests` but still `#[cfg(test)]`, so `src/ui.rs` tests can reach it:

```rust
/// The seven-node `EXPLAIN ANALYZE` from the design canvas. Task 2's parser test proves the
/// PostgreSQL JSON for this plan parses to exactly this value, so the UI tests can use it without
/// touching the provider.
#[cfg(test)]
pub(crate) fn sample_plan() -> QueryPlan {
    fn actual(startup: f64, total: f64, rows: f64) -> Option<ActualStatistics> {
        Some(ActualStatistics {
            startup_time_ms: startup,
            total_time_ms: total,
            rows,
            loops: 1.0,
        })
    }
    fn props(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }
    let customer = PlanNode {
        operation: "Seq Scan".into(),
        target: Some("customer c".into()),
        startup_cost: 0.0,
        total_cost: 152.0,
        plan_rows: 4960.0,
        plan_width: 20,
        actual: actual(0.012, 0.84, 4960.0),
        properties: Vec::new(),
        children: Vec::new(),
    };
    let hash = PlanNode {
        operation: "Hash".into(),
        target: None,
        startup_cost: 152.0,
        total_cost: 152.0,
        plan_rows: 4960.0,
        plan_width: 20,
        actual: actual(1.61, 1.61, 4960.0),
        properties: props(&[
            ("Hash Buckets", "8192"),
            ("Original Hash Buckets", "8192"),
            ("Hash Batches", "1"),
            ("Original Hash Batches", "1"),
            ("Peak Memory Usage", "297"),
        ]),
        children: vec![customer],
    };
    let orders = PlanNode {
        operation: "Seq Scan".into(),
        target: Some("orders o".into()),
        startup_cost: 0.0,
        total_cost: 4102.0,
        plan_rows: 21092.0,
        plan_width: 24,
        actual: actual(0.031, 18.72, 48317.0),
        properties: props(&[
            ("Filter", "(placed_at >= (now() - '30 days'::interval))"),
            ("Rows Removed by Filter", "151683"),
        ]),
        children: Vec::new(),
    };
    let join = PlanNode {
        operation: "Hash Join".into(),
        target: None,
        startup_cost: 214.0,
        total_cost: 4522.11,
        plan_rows: 21092.0,
        plan_width: 24,
        actual: actual(1.7, 29.44, 48317.0),
        properties: props(&[
            ("Join Type", "Inner"),
            ("Inner Unique", "true"),
            ("Hash Cond", "(o.customer_id = c.id)"),
        ]),
        children: vec![orders, hash],
    };
    let aggregate = PlanNode {
        operation: "HashAggregate".into(),
        target: None,
        startup_cost: 4680.30,
        total_cost: 4714.21,
        plan_rows: 4948.0,
        plan_width: 48,
        actual: actual(35.9, 36.9, 4948.0),
        properties: props(&[
            ("Group Key", "c.name"),
            ("Batches", "1"),
            ("Peak Memory Usage", "721"),
        ]),
        children: vec![join],
    };
    let sort = PlanNode {
        operation: "Sort".into(),
        target: None,
        startup_cost: 4821.14,
        total_cost: 4833.51,
        plan_rows: 4948.0,
        plan_width: 48,
        actual: actual(38.203, 38.205, 10.0),
        properties: props(&[
            ("Sort Key", "(sum(o.total)) DESC"),
            ("Sort Method", "top-N heapsort"),
            ("Sort Space Used", "26"),
            ("Sort Space Type", "Memory"),
        ]),
        children: vec![aggregate],
    };
    QueryPlan {
        root: PlanNode {
            operation: "Limit".into(),
            target: None,
            startup_cost: 4821.14,
            total_cost: 4821.16,
            plan_rows: 10.0,
            plan_width: 48,
            actual: actual(38.205, 38.21, 10.0),
            properties: Vec::new(),
            children: vec![sort],
        },
        analysed: true,
        planning_time_ms: Some(0.412),
        execution_time_ms: Some(38.402),
    }
}
```

Add `pub mod plan;` to `src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test plan::tests`
Expected: compilation errors — `QueryPlan`, `PlanNode`, `ActualStatistics` not found.

- [ ] **Step 3: Implement the model**

Insert above the fixture in `src/plan.rs`:

```rust
/// One execution plan (FR3-020). `analysed` records whether `ANALYZE` ran, which is what decides
/// whether `actual` figures exist and whether the run had side effects (FR3-019).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryPlan {
    pub root: PlanNode,
    pub analysed: bool,
    pub planning_time_ms: Option<f64>,
    pub execution_time_ms: Option<f64>,
}

/// One plan node. `operation` is the node type as PostgreSQL names it ("Seq Scan", "Hash Join");
/// `target` is the relation, alias or index clause that follows it in the text form
/// ("orders o", "using customer_pkey on customer c"). `properties` are every remaining detail the
/// server reported, in the server's order, already formatted for display.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    pub operation: String,
    pub target: Option<String>,
    pub startup_cost: f64,
    pub total_cost: f64,
    pub plan_rows: f64,
    pub plan_width: u64,
    pub actual: Option<ActualStatistics>,
    pub properties: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
}

/// What `ANALYZE` measured. `rows` is per loop, as PostgreSQL reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActualStatistics {
    pub startup_time_ms: f64,
    pub total_time_ms: f64,
    pub rows: f64,
    pub loops: f64,
}

/// A node laid out for the tree view, with the metrics §15.2 asks for: cost, estimates against
/// actuals, and which node is the expensive one.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanRow<'a> {
    pub index: usize,
    pub depth: usize,
    pub node: &'a PlanNode,
    /// `total_cost` less the children's, floored at zero.
    pub exclusive_cost: f64,
    /// This node's exclusive cost as a fraction of the plan's total exclusive cost.
    pub share: f64,
    pub hottest: bool,
    /// `(actual rows × loops) ÷ plan rows`, when `ANALYZE` ran and there is an estimate.
    pub estimate_ratio: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphPlacement {
    pub index: usize,
    pub column: usize,
    pub row: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdge {
    pub parent: usize,
    pub child: usize,
    /// The rows that flowed up this edge: actual rows over all loops when analysed, else the
    /// estimate. Views scale the edge by it.
    pub rows: f64,
}

/// The graph view's geometry in abstract units: `column` is depth, `row` is leaf order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphLayout {
    pub placements: Vec<GraphPlacement>,
    pub edges: Vec<GraphEdge>,
    pub columns: usize,
    pub leaf_rows: usize,
}

impl PlanNode {
    /// "Seq Scan on orders o" — the operation and its target, as the text form prints them. An
    /// index target already carries its own wording ("using customer_pkey on customer").
    pub fn heading(&self) -> String {
        match &self.target {
            Some(target) if target.starts_with("using ") => format!("{} {target}", self.operation),
            Some(target) => format!("{} on {target}", self.operation),
            None => self.operation.clone(),
        }
    }

    /// The node's first text line: heading, cost clause, and the actual clause when there is one.
    pub fn summary_line(&self) -> String {
        let mut line = format!(
            "{}  (cost={:.2}..{:.2} rows={:.0} width={})",
            self.heading(),
            self.startup_cost,
            self.total_cost,
            self.plan_rows,
            self.plan_width
        );
        if let Some(actual) = &self.actual {
            line.push_str(&format!(
                " (actual time={:.3}..{:.3} rows={:.0} loops={:.0})",
                actual.startup_time_ms, actual.total_time_ms, actual.rows, actual.loops
            ));
        }
        line
    }

    /// This node's own lines — summary and properties — with the heading starting at
    /// `heading_column` and properties two columns further in. Children are not included; the
    /// detail panel shows one node, and `as_text` recurses itself.
    pub fn text_lines(&self, heading_column: usize) -> Vec<String> {
        let indent = " ".repeat(heading_column);
        let mut lines = vec![format!("{indent}{}", self.summary_line())];
        for (key, value) in &self.properties {
            lines.push(format!("{indent}  {key}: {value}"));
        }
        lines
    }

    fn rows_over_all_loops(&self) -> f64 {
        match &self.actual {
            Some(actual) => actual.rows * actual.loops,
            None => self.plan_rows,
        }
    }

    fn exclusive_cost(&self) -> f64 {
        let children: f64 = self.children.iter().map(|child| child.total_cost).sum();
        (self.total_cost - children).max(0.0)
    }
}

impl QueryPlan {
    /// Every node in preorder, with depth and the derived metrics.
    pub fn rows(&self) -> Vec<PlanRow<'_>> {
        let mut rows = Vec::new();
        collect_rows(&self.root, 0, &mut rows);
        let total: f64 = rows.iter().map(|row| row.exclusive_cost).sum();
        let hottest = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.exclusive_cost > 0.0)
            .max_by(|(_, a), (_, b)| {
                // First on ties: `max_by` keeps the last maximum, so compare in reverse order of
                // index when the costs are equal.
                a.exclusive_cost
                    .partial_cmp(&b.exclusive_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.index.cmp(&a.index))
            })
            .map(|(position, _)| position);
        for (position, row) in rows.iter_mut().enumerate() {
            row.share = if total > 0.0 {
                row.exclusive_cost / total
            } else {
                0.0
            };
            row.hottest = hottest == Some(position);
        }
        rows
    }

    pub fn node_count(&self) -> usize {
        count_nodes(&self.root)
    }

    /// The node at `index` in preorder — the index `rows()` and `graph_layout()` report.
    pub fn node(&self, index: usize) -> Option<&PlanNode> {
        let mut remaining = index;
        find_node(&self.root, &mut remaining)
    }

    /// The plan in PostgreSQL's text layout: `->` for children, properties indented under their
    /// node, timings last. Rendered from the model so `ANALYZE` executes exactly once (§15.2).
    pub fn as_text(&self) -> String {
        let mut lines = Vec::new();
        write_text(&self.root, 0, true, &mut lines);
        if let Some(planning) = self.planning_time_ms {
            lines.push(format!("Planning Time: {planning:.3} ms"));
        }
        if let Some(execution) = self.execution_time_ms {
            lines.push(format!("Execution Time: {execution:.3} ms"));
        }
        lines.join("\n")
    }

    /// Columns by depth, rows by leaf order; an inner node sits between its first and last child.
    pub fn graph_layout(&self) -> GraphLayout {
        let mut layout = GraphLayout::default();
        let mut next_index = 0;
        let mut next_leaf_row = 0usize;
        place(&self.root, 0, &mut next_index, &mut next_leaf_row, &mut layout);
        layout.leaf_rows = next_leaf_row;
        layout.columns = layout
            .placements
            .iter()
            .map(|placement| placement.column + 1)
            .max()
            .unwrap_or(0);
        layout
    }
}

fn collect_rows<'a>(node: &'a PlanNode, depth: usize, rows: &mut Vec<PlanRow<'a>>) {
    let estimate_ratio = node.actual.filter(|_| node.plan_rows > 0.0).map(|actual| {
        actual.rows * actual.loops / node.plan_rows
    });
    rows.push(PlanRow {
        index: rows.len(),
        depth,
        node,
        exclusive_cost: node.exclusive_cost(),
        share: 0.0,
        hottest: false,
        estimate_ratio,
    });
    for child in &node.children {
        collect_rows(child, depth + 1, rows);
    }
}

fn count_nodes(node: &PlanNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

fn find_node<'a>(node: &'a PlanNode, remaining: &mut usize) -> Option<&'a PlanNode> {
    if *remaining == 0 {
        return Some(node);
    }
    *remaining -= 1;
    node.children
        .iter()
        .find_map(|child| find_node(child, remaining))
}

/// PostgreSQL's layout: a child's arrow sits two columns in from its parent's heading, the child's
/// heading four further on, and every property two columns in from its node's heading.
fn write_text(node: &PlanNode, heading_column: usize, root: bool, lines: &mut Vec<String>) {
    if root {
        lines.extend(node.text_lines(0));
    } else {
        let arrow_column = heading_column - 4;
        let mut own = node.text_lines(heading_column);
        own[0] = format!("{}->  {}", " ".repeat(arrow_column), node.summary_line());
        lines.extend(own);
    }
    for child in &node.children {
        write_text(child, heading_column + 6, false, lines);
    }
}

fn place(
    node: &PlanNode,
    depth: usize,
    next_index: &mut usize,
    next_leaf_row: &mut usize,
    layout: &mut GraphLayout,
) -> f32 {
    let index = *next_index;
    *next_index += 1;
    // Reserve the slot now so placements stay in preorder; the row is filled in below.
    layout.placements.push(GraphPlacement {
        index,
        column: depth,
        row: 0.0,
    });
    let row = if node.children.is_empty() {
        let row = *next_leaf_row as f32;
        *next_leaf_row += 1;
        row
    } else {
        let mut first = None;
        let mut last = 0.0;
        for child in &node.children {
            let child_index = *next_index;
            let child_row = place(child, depth + 1, next_index, next_leaf_row, layout);
            layout.edges.push(GraphEdge {
                parent: index,
                child: child_index,
                rows: child.rows_over_all_loops(),
            });
            first.get_or_insert(child_row);
            last = child_row;
        }
        (first.unwrap_or(0.0) + last) / 2.0
    };
    layout.placements[index].row = row;
    row
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test plan::tests`
Expected: all 11 tests PASS. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings`. If clippy complains about `edges` ordering in `place` (it pushes the edge after the child's subtree), that is intended — edges come out in preorder of the child, which is what the layout test asserts.

- [ ] **Step 5: Commit**

```bash
git add src/plan.rs src/lib.rs
git commit -m "Model execution plans with per-node metrics and text form" -m "Adds the engine-neutral QueryPlan that FR3-020 renders: preorder rows carrying exclusive cost, share and estimate ratio, the plan in PostgreSQL's text layout, and a column/row layout for the graph view. Nothing here knows about JSON or GPUI."
```

---

### Task 2: Parse PostgreSQL's JSON plan — `src/postgres/plan.rs`

**Files:**
- Create: `src/postgres/plan.rs`
- Modify: `src/postgres.rs` (add `pub mod plan;` next to `mod catalogue;`), `Cargo.toml` (serde_json `preserve_order`)

**Interfaces:**
- Consumes: `crate::plan::{QueryPlan, PlanNode, ActualStatistics}`, `crate::result::QueryError`.
- Produces: `pub fn parse_plan(json: &str) -> Result<QueryPlan, QueryError>`; `#[cfg(test)] pub(crate) const SAMPLE_PLAN_JSON: &str`.

- [ ] **Step 1: Enable key order**

In `Cargo.toml` change `serde_json = "1.0.151"` to `serde_json = { version = "1.0.151", features = ["preserve_order"] }`. Property order in the detail panel and text view must match the server's, and `serde_json::Map` sorts keys alphabetically without this feature.

- [ ] **Step 2: Write the failing tests**

Create `src/postgres/plan.rs`:

```rust
//! Converts `EXPLAIN (FORMAT JSON)` output into the plan model. The only code that knows
//! PostgreSQL's key names (§38: engine specifics stay in the provider).

use serde_json::Value;

use crate::plan::{ActualStatistics, PlanNode, QueryPlan};
use crate::result::QueryError;

#[cfg(test)]
pub(crate) const SAMPLE_PLAN_JSON: &str = r#"[{"Plan":{"Node Type":"Limit","Parallel Aware":false,"Async Capable":false,"Startup Cost":4821.14,"Total Cost":4821.16,"Plan Rows":10,"Plan Width":48,"Actual Startup Time":38.205,"Actual Total Time":38.210,"Actual Rows":10,"Actual Loops":1,"Plans":[{"Node Type":"Sort","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Startup Cost":4821.14,"Total Cost":4833.51,"Plan Rows":4948,"Plan Width":48,"Actual Startup Time":38.203,"Actual Total Time":38.205,"Actual Rows":10,"Actual Loops":1,"Sort Key":["(sum(o.total)) DESC"],"Sort Method":"top-N heapsort","Sort Space Used":26,"Sort Space Type":"Memory","Plans":[{"Node Type":"Aggregate","Strategy":"Hashed","Partial Mode":"Simple","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Startup Cost":4680.30,"Total Cost":4714.21,"Plan Rows":4948,"Plan Width":48,"Actual Startup Time":35.9,"Actual Total Time":36.9,"Actual Rows":4948,"Actual Loops":1,"Group Key":["c.name"],"Batches":1,"Peak Memory Usage":721,"Plans":[{"Node Type":"Hash Join","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Join Type":"Inner","Startup Cost":214.00,"Total Cost":4522.11,"Plan Rows":21092,"Plan Width":24,"Actual Startup Time":1.7,"Actual Total Time":29.44,"Actual Rows":48317,"Actual Loops":1,"Inner Unique":true,"Hash Cond":"(o.customer_id = c.id)","Plans":[{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Relation Name":"orders","Alias":"o","Startup Cost":0.00,"Total Cost":4102.00,"Plan Rows":21092,"Plan Width":24,"Actual Startup Time":0.031,"Actual Total Time":18.72,"Actual Rows":48317,"Actual Loops":1,"Filter":"(placed_at >= (now() - '30 days'::interval))","Rows Removed by Filter":151683},{"Node Type":"Hash","Parent Relationship":"Inner","Parallel Aware":false,"Async Capable":false,"Startup Cost":152.00,"Total Cost":152.00,"Plan Rows":4960,"Plan Width":20,"Actual Startup Time":1.61,"Actual Total Time":1.61,"Actual Rows":4960,"Actual Loops":1,"Hash Buckets":8192,"Original Hash Buckets":8192,"Hash Batches":1,"Original Hash Batches":1,"Peak Memory Usage":297,"Plans":[{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Relation Name":"customer","Alias":"c","Startup Cost":0.00,"Total Cost":152.00,"Plan Rows":4960,"Plan Width":20,"Actual Startup Time":0.012,"Actual Total Time":0.84,"Actual Rows":4960,"Actual Loops":1}]}]}]}]},"Planning Time":0.412,"Triggers":[],"Execution Time":38.402}]"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::sample_plan;

    #[test]
    fn the_sample_json_parses_to_the_sample_plan() {
        assert_eq!(parse_plan(SAMPLE_PLAN_JSON).unwrap(), sample_plan());
    }

    #[test]
    fn a_plain_explain_has_no_actual_figures_and_is_not_analysed() {
        let json = r#"[{"Plan":{"Node Type":"Result","Startup Cost":0.00,"Total Cost":0.01,"Plan Rows":1,"Plan Width":4},"Planning Time":0.02}]"#;
        let plan = parse_plan(json).unwrap();
        assert!(!plan.analysed);
        assert_eq!(plan.root.operation, "Result");
        assert!(plan.root.actual.is_none());
        assert_eq!(plan.execution_time_ms, None);
        assert_eq!(plan.planning_time_ms, Some(0.02));
    }

    #[test]
    fn scan_targets_follow_the_text_forms_wording() {
        let json = r#"[{"Plan":{"Node Type":"Index Scan","Index Name":"customer_pkey","Relation Name":"customer","Alias":"customer","Startup Cost":0.15,"Total Cost":8.17,"Plan Rows":1,"Plan Width":36}}]"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(plan.root.heading(), "Index Scan using customer_pkey on customer");

        let json = r#"[{"Plan":{"Node Type":"Subquery Scan","Alias":"sub","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4}}]"#;
        assert_eq!(parse_plan(json).unwrap().root.heading(), "Subquery Scan on sub");
    }

    #[test]
    fn aggregate_strategy_and_partial_mode_name_the_operation() {
        for (strategy, mode, expected) in [
            ("Hashed", "Simple", "HashAggregate"),
            ("Sorted", "Partial", "Partial GroupAggregate"),
            ("Plain", "Finalize", "Finalize Aggregate"),
            ("Mixed", "Simple", "MixedAggregate"),
        ] {
            let json = format!(
                r#"[{{"Plan":{{"Node Type":"Aggregate","Strategy":"{strategy}","Partial Mode":"{mode}","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4}}}}]"#
            );
            let plan = parse_plan(&json).unwrap();
            assert_eq!(plan.root.operation, expected);
            assert!(plan.root.properties.is_empty(), "strategy and mode are consumed");
        }
    }

    #[test]
    fn property_values_are_formatted_for_display_in_server_order() {
        let json = r#"[{"Plan":{"Node Type":"Sort","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4,"Sort Key":["a","b DESC"],"Workers":{"Number":2},"Inner Unique":false,"Sort Space Used":26}}]"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(
            plan.root.properties,
            vec![
                ("Sort Key".to_owned(), "a, b DESC".to_owned()),
                ("Workers".to_owned(), r#"{"Number":2}"#.to_owned()),
                ("Inner Unique".to_owned(), "false".to_owned()),
                ("Sort Space Used".to_owned(), "26".to_owned()),
            ]
        );
    }

    #[test]
    fn malformed_output_is_an_error_rather_than_a_panic() {
        for json in ["", "not json", "[]", "[{}]", r#"[{"Plan":{"Startup Cost":1}}]"#] {
            let error = parse_plan(json).unwrap_err();
            assert!(
                error.message.starts_with("Could not read the execution plan"),
                "{json:?} gave {}",
                error.message
            );
        }
    }
}
```

Add `pub mod plan;` to `src/postgres.rs` beside `mod catalogue;`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test postgres::plan::tests`
Expected: compilation error — `parse_plan` not found.

- [ ] **Step 4: Implement the parser**

Insert between the imports and the fixture in `src/postgres/plan.rs`:

```rust
/// Keys the model represents with fields of its own, so they must not also appear as properties.
const CONSUMED_KEYS: &[&str] = &[
    "Node Type",
    "Relation Name",
    "Alias",
    "Index Name",
    "Startup Cost",
    "Total Cost",
    "Plan Rows",
    "Plan Width",
    "Actual Startup Time",
    "Actual Total Time",
    "Actual Rows",
    "Actual Loops",
    "Plans",
    "Strategy",
    "Partial Mode",
    // Structural noise the text form does not print either.
    "Parent Relationship",
    "Parallel Aware",
    "Async Capable",
];

/// `EXPLAIN (FORMAT JSON)` returns a one-element array holding the plan and its timings.
pub fn parse_plan(json: &str) -> Result<QueryPlan, QueryError> {
    let document: Value = serde_json::from_str(json).map_err(|error| unreadable(&error.to_string()))?;
    let top = document
        .as_array()
        .and_then(|array| array.first())
        .and_then(Value::as_object)
        .ok_or_else(|| unreadable("expected a one-element array"))?;
    let root = top
        .get("Plan")
        .and_then(Value::as_object)
        .ok_or_else(|| unreadable("no Plan object"))?;
    let root = parse_node(root)?;
    Ok(QueryPlan {
        analysed: root.actual.is_some(),
        root,
        planning_time_ms: top.get("Planning Time").and_then(Value::as_f64),
        execution_time_ms: top.get("Execution Time").and_then(Value::as_f64),
    })
}

fn parse_node(object: &serde_json::Map<String, Value>) -> Result<PlanNode, QueryError> {
    let string = |key: &str| object.get(key).and_then(Value::as_str).map(ToOwned::to_owned);
    let number = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| unreadable(&format!("{key} is missing")))
    };
    let node_type = string("Node Type").ok_or_else(|| unreadable("Node Type is missing"))?;
    let operation = operation_name(&node_type, string("Strategy").as_deref(), string("Partial Mode").as_deref());
    let actual = match (
        object.get("Actual Startup Time").and_then(Value::as_f64),
        object.get("Actual Total Time").and_then(Value::as_f64),
        object.get("Actual Rows").and_then(Value::as_f64),
        object.get("Actual Loops").and_then(Value::as_f64),
    ) {
        (Some(startup_time_ms), Some(total_time_ms), Some(rows), Some(loops)) => Some(ActualStatistics {
            startup_time_ms,
            total_time_ms,
            rows,
            loops,
        }),
        _ => None,
    };
    let children = match object.get("Plans") {
        Some(Value::Array(plans)) => plans
            .iter()
            .map(|plan| {
                plan.as_object()
                    .ok_or_else(|| unreadable("a child plan is not an object"))
                    .and_then(parse_node)
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    let properties = object
        .iter()
        .filter(|(key, _)| !CONSUMED_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), display_value(value)))
        .collect();
    Ok(PlanNode {
        operation,
        target: target(
            string("Index Name").as_deref(),
            string("Relation Name").as_deref(),
            string("Alias").as_deref(),
        ),
        startup_cost: number("Startup Cost")?,
        total_cost: number("Total Cost")?,
        plan_rows: number("Plan Rows")?,
        plan_width: number("Plan Width")? as u64,
        actual,
        properties,
        children,
    })
}

/// The text form's name for an aggregate: strategy and partial mode fold into the node type.
fn operation_name(node_type: &str, strategy: Option<&str>, partial_mode: Option<&str>) -> String {
    let base = match (node_type, strategy) {
        ("Aggregate", Some("Hashed")) => "HashAggregate".to_owned(),
        ("Aggregate", Some("Sorted")) => "GroupAggregate".to_owned(),
        ("Aggregate", Some("Mixed")) => "MixedAggregate".to_owned(),
        _ => node_type.to_owned(),
    };
    match partial_mode {
        Some(mode) if mode != "Simple" => format!("{mode} {base}"),
        _ => base,
    }
}

/// "using customer_pkey on customer c" — the clause the text form appends to a scan.
fn target(index: Option<&str>, relation: Option<&str>, alias: Option<&str>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(index) = index {
        parts.push(format!("using {index}"));
    }
    match (relation, alias) {
        (Some(relation), Some(alias)) if alias != relation => parts.push(format!("{relation} {alias}")),
        (Some(relation), _) => parts.push(relation.to_owned()),
        (None, Some(alias)) => parts.push(alias.to_owned()),
        (None, None) => {}
    }
    (!parts.is_empty()).then(|| parts.join(" on "))
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn unreadable(detail: &str) -> QueryError {
    QueryError {
        message: format!("Could not read the execution plan: {detail}"),
        severity: None,
        code: None,
        detail: None,
        hint: None,
        position: None,
    }
}
```

Note on `target`: the parts are joined with `" on "`, so an index scan's target is
`using customer_pkey on customer` and a plain scan's is `orders o`. `PlanNode::heading` (Task 1)
prints `"{operation} {target}"` when the target starts with `using ` and `"{operation} on {target}"`
otherwise, which gives `Index Scan using customer_pkey on customer` and `Seq Scan on orders o`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test plan` (covers both modules)
Expected: PASS. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/postgres.rs src/postgres/plan.rs src/plan.rs
git commit -m "Parse EXPLAIN (FORMAT JSON) output into the plan model" -m "PostgreSQL's JSON keys are read in one place in the provider. Aggregate strategy and partial mode fold into the operation name and scan targets take the text form's wording, so the rendered plan reads as the server would have printed it. serde_json keeps key order so properties stay in the server's order."
```

---

### Task 3: `explain` on the provider; the plan travels in `QueryResult`

**Files:**
- Modify: `src/result.rs` (add `plan` field), `src/database.rs` (trait method), `src/sql.rs:238-241` (`prepare_explain`), `src/postgres.rs` (implement `explain`), `src/plan.rs` (`into_result`), `src/application.rs` tests (`FakeProvider`), `src/ui.rs` tests (`UiTestProvider`)

**Interfaces:**
- Produces:
  - `QueryResult { …, pub plan: Option<QueryPlan> }`
  - `DatabaseProvider::explain(&self, sql: &str, analyse: bool) -> Result<QueryResult, QueryError>`
  - `sql::prepare_explain(sql: &str, analyse: bool) -> String`
  - `QueryPlan::into_result(self, execution_time: Duration) -> QueryResult`
- Consumes: Task 2's `parse_plan`.

- [ ] **Step 1: Write the failing tests**

In `src/sql.rs` tests, find the existing `prepare_explain` test (grep `prepare_explain` in the `mod tests`) and replace it with:

```rust
    /// FR-016: the plain form never analyses. Both forms ask for JSON so the provider can build
    /// the plan model from one execution.
    #[test]
    fn explain_forms_request_json_and_only_analyse_when_asked() {
        assert_eq!(
            prepare_explain("  SELECT 1;  ", false),
            "EXPLAIN (FORMAT JSON) SELECT 1;"
        );
        assert_eq!(
            prepare_explain("SELECT 1", true),
            "EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1"
        );
    }
```

In `src/plan.rs` tests add:

```rust
    #[test]
    fn a_plan_result_carries_the_text_lines_and_the_plan() {
        let result = sample_plan().into_result(std::time::Duration::from_millis(38));
        assert_eq!(result.columns.len(), 1);
        assert_eq!(result.columns[0].name, "QUERY PLAN");
        assert_eq!(result.rows.len(), sample_plan().as_text().lines().count());
        assert_eq!(
            result.rows[0][0],
            crate::result::CellValue::Text(sample_plan().as_text().lines().next().unwrap().to_owned())
        );
        assert_eq!(result.plan, Some(sample_plan()));
        assert_eq!(result.execution_time, std::time::Duration::from_millis(38));
        assert_eq!(result.status, crate::result::ExecutionStatus::Completed);
        // The text view prints the rows verbatim, one per line, and then its status line.
        assert!(result.as_text().starts_with("QUERY PLAN\nLimit  (cost="));
    }
```

In `src/postgres.rs` tests add (the provider's own `explain` needs a server, so this covers the pure conversion it uses):

```rust
    #[test]
    fn a_json_cell_becomes_a_plan_result() {
        let result = QueryResult {
            columns: vec![Column {
                name: "QUERY PLAN".into(),
                database_type: "json".into(),
                nullable: None,
            }],
            rows: vec![vec![CellValue::Json(plan::SAMPLE_PLAN_JSON.into())]],
            execution_time: Duration::from_millis(40),
            notices: vec!["NOTICE: hello".into()],
            ..QueryResult::default()
        };
        let converted = plan_result(result).unwrap();
        assert_eq!(converted.plan, Some(crate::plan::sample_plan()));
        assert_eq!(converted.execution_time, Duration::from_millis(40));
        assert_eq!(converted.notices, vec!["NOTICE: hello".to_owned()]);
        assert!(!converted.rows.is_empty());
    }

    #[test]
    fn a_result_without_a_json_cell_is_an_error() {
        let error = plan_result(QueryResult::default()).unwrap_err();
        assert!(error.message.starts_with("Could not read the execution plan"));
    }
```

(Add `use std::time::Duration;` and `use crate::result::{CellValue, Column};` to that test module if they are not already imported.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test explain_forms_request_json && cargo test a_plan_result_carries && cargo test a_json_cell_becomes`
Expected: compilation errors (wrong arity on `prepare_explain`, no `into_result`, no `plan_result`, no `plan` field).

- [ ] **Step 3: Implement**

`src/result.rs` — add the field and import:

```rust
use crate::plan::QueryPlan;
…
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<CellValue>>,
    pub affected_rows: Option<u64>,
    pub execution_time: Duration,
    pub status: ExecutionStatus,
    pub command_tag: Option<String>,
    pub notices: Vec<String>,
    pub automatic_limit: Option<u32>,
    /// The structured plan when the statement was an `EXPLAIN` (FR3-020). The text lines are in
    /// `rows` as well, so every existing renderer and the copy path keep working.
    pub plan: Option<QueryPlan>,
}
```

`src/sql.rs`:

```rust
/// Builds the EXPLAIN statement. Only `CommandService::explain_analyse` passes `analyse = true`;
/// plain Explain cannot request ANALYZE (FR-016). JSON output is what the provider parses (FR3-020).
pub fn prepare_explain(sql: &str, analyse: bool) -> String {
    let options = if analyse {
        "ANALYZE, FORMAT JSON"
    } else {
        "FORMAT JSON"
    };
    format!("EXPLAIN ({options}) {}", sql.trim())
}
```

`src/database.rs` — add to the trait after `execute`:

```rust
    /// Runs `EXPLAIN` — with `ANALYZE` only when `analyse` is set (FR3-019) — and returns the
    /// plan as a result carrying both the text lines and the structured plan (FR3-020).
    async fn explain(&self, sql: &str, analyse: bool) -> Result<QueryResult, QueryError>;
```

`src/plan.rs` — add to `impl QueryPlan`:

```rust
    /// The plan as a result: one `QUERY PLAN` text column holding the text lines, plus the plan
    /// itself, so the text view, the result window and copying need nothing new.
    pub fn into_result(self, execution_time: Duration) -> QueryResult {
        let rows = self
            .as_text()
            .lines()
            .map(|line| vec![CellValue::Text(line.to_owned())])
            .collect();
        QueryResult {
            columns: vec![Column {
                name: "QUERY PLAN".into(),
                database_type: "text".into(),
                nullable: None,
            }],
            rows,
            execution_time,
            status: ExecutionStatus::Completed,
            plan: Some(self),
            ..QueryResult::default()
        }
    }
```

with `use std::time::Duration; use crate::result::{CellValue, Column, ExecutionStatus, QueryResult};` at the top of `src/plan.rs`.

`src/postgres.rs` — add a free function and the trait method:

```rust
/// Turns the single JSON cell `EXPLAIN (FORMAT JSON)` returns into a plan result, keeping the
/// timing and notices of the execution that produced it.
fn plan_result(result: QueryResult) -> Result<QueryResult, QueryError> {
    let json = match result.rows.first().and_then(|row| row.first()) {
        Some(CellValue::Json(json)) | Some(CellValue::Text(json)) => json,
        _ => {
            return Err(simple_error(
                "Could not read the execution plan: the server returned no plan",
            ));
        }
    };
    let mut converted = plan::parse_plan(json)?.into_result(result.execution_time);
    converted.notices = result.notices;
    Ok(converted)
}
```

and inside `impl DatabaseProvider for PostgresProvider`, after `execute`:

```rust
    async fn explain(&self, sql: &str, analyse: bool) -> Result<QueryResult, QueryError> {
        let statement = crate::sql::prepare_explain(sql, analyse);
        let result = self.execute(&statement).await?;
        plan_result(result)
    }
```

`simple_error` already exists at `src/postgres.rs:672`; check its signature (`fn simple_error(message: &str) -> QueryError`) and reuse it.

Update the fakes so the crate compiles:

`src/application.rs` `FakeProvider` — add a recorded flag and the method:

```rust
        async fn explain(&self, sql: &str, analyse: bool) -> Result<QueryResult, QueryError> {
            self.statements
                .lock()
                .unwrap()
                .push(format!("EXPLAIN analyse={analyse} {sql}"));
            if sql.contains("CANCELLED") {
                return Err(test_error("cancelled", Some("57014")));
            }
            Ok(QueryResult::default())
        }
```

and change the existing `explain_is_plain_explain` test's assertion to:

```rust
        let sql = &provider.statements.lock().unwrap()[0];
        assert_eq!(sql, "EXPLAIN analyse=false SELECT 1;");
```

`src/ui.rs` `UiTestProvider` — add:

```rust
        async fn explain(&self, _: &str, analyse: bool) -> Result<QueryResult, QueryError> {
            while self.blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let mut plan = crate::plan::sample_plan();
            if !analyse {
                plan.analysed = false;
                plan.execution_time_ms = None;
                fn strip(node: &mut crate::plan::PlanNode) {
                    node.actual = None;
                    node.children.iter_mut().for_each(strip);
                }
                strip(&mut plan.root);
            }
            Ok(plan.into_result(std::time::Duration::from_millis(38)))
        }
```

`CommandService::explain` in `src/application.rs` currently calls `prepare_explain(sql)` then `execute`; change it now to keep the build green:

```rust
        match self.provider.explain(sql, false).await {
```

(remove the `let explained = prepare_explain(sql);` line and the `prepare_explain` import). Task 4 finishes this method.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS, including the amended `explain_is_plain_explain`. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add src/result.rs src/database.rs src/sql.rs src/plan.rs src/postgres.rs src/application.rs src/ui.rs
git commit -m "Return execution plans from the provider as structured results" -m "DatabaseProvider gains explain(sql, analyse). PostgreSQL runs EXPLAIN (FORMAT JSON) once, parses the single JSON cell, and hands back a QueryResult whose rows are the text lines and whose plan field is the model, so the text view, result window and copying work unchanged while the new views have the structure they need."
```

---

### Task 4: The Explain Analyze command and its safeguard — `src/application.rs`

**Files:**
- Modify: `src/application.rs`

**Interfaces:**
- Produces:
  - `command::EXPLAIN_ANALYSE: &str = "sql.explain_analyse"`
  - `pub enum PlanDisplay { #[default] Tree, Graph, Text }`
  - `EditorState::plan_display: PlanDisplay`
  - `CommandService::explain_analyse(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError>`
- Consumes: `sql::{analyse_statement, StatementKind}`, `DatabaseProvider::explain`.

- [ ] **Step 1: Write the failing tests**

Add to `src/application.rs` `mod tests`:

```rust
    /// FR3-019: Explain Analyze is its own command and is the only path that analyses.
    #[tokio::test]
    async fn explain_analyse_runs_analyze_for_a_row_returning_statement() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let mut editor = editor();
        editor.document = "SELECT 1;".into();
        editor.cursor = 3;

        service.explain_analyse(&mut editor).await.unwrap();

        assert_eq!(
            provider.statements.lock().unwrap()[0],
            "EXPLAIN analyse=true SELECT 1;"
        );
        assert_eq!(editor.execution_status, ExecutionStatus::Completed);
        assert_eq!(editor.results.len(), 1);
    }

    /// `ANALYZE` executes the statement, so anything that is not a plain row-returning query is
    /// refused before it reaches the server (§15.1).
    #[tokio::test]
    async fn explain_analyse_refuses_statements_that_are_not_row_returning() {
        for document in [
            "UPDATE customer SET name = 'x';",
            "DELETE FROM customer;",
            "INSERT INTO customer VALUES (1) RETURNING id;",
            "CREATE TABLE t (id int);",
            "SELECT 1 INTO t;",
        ] {
            let provider = Arc::new(FakeProvider::default());
            let service = CommandService::new(provider.clone());
            let mut editor = editor();
            editor.document = document.into();
            editor.cursor = 2;
            editor.results = vec![QueryResult {
                command_tag: Some("PREVIOUS".into()),
                ..QueryResult::default()
            }];

            let error = service.explain_analyse(&mut editor).await.unwrap_err();

            assert!(
                provider.statements.lock().unwrap().is_empty(),
                "{document} must not reach the provider"
            );
            assert_eq!(
                error.message,
                "Explain Analyze runs only row-returning statements (SELECT, WITH … SELECT, VALUES); the statement would execute"
            );
            assert_eq!(editor.execution_status, ExecutionStatus::Failed);
            assert_eq!(editor.error.as_ref(), Some(&error));
            assert_eq!(editor.results[0].command_tag.as_deref(), Some("PREVIOUS"));
        }
    }

    #[tokio::test]
    async fn explain_analyse_cancellation_sets_cancelled_state() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.document = "SELECT 'CANCELLED';".into();
        editor.cursor = 3;

        let error = service.explain_analyse(&mut editor).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("57014"));
        assert_eq!(editor.execution_status, ExecutionStatus::Cancelled);
    }

    #[test]
    fn a_new_editor_shows_plans_as_a_tree() {
        assert_eq!(editor().plan_display, PlanDisplay::Tree);
        assert_eq!(command::EXPLAIN_ANALYSE, "sql.explain_analyse");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test application::tests::explain_analyse`
Expected: compilation errors — no `explain_analyse`, no `PlanDisplay`, no `EXPLAIN_ANALYSE`.

- [ ] **Step 3: Implement**

In `pub mod command` add after `EXPLAIN`:

```rust
    /// FR3-019: `EXPLAIN ANALYZE`, deliberately its own command because it executes the statement.
    pub const EXPLAIN_ANALYSE: &str = "sql.explain_analyse";
```

After `ResultDestination` add:

```rust
/// How a plan result is shown (FR3-020). Separate from `ResultDisplay` so switching a table result
/// between TABLE and TEXT never changes how a plan shows, and vice versa.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlanDisplay {
    #[default]
    Tree,
    Graph,
    Text,
}
```

Add `pub plan_display: PlanDisplay,` to `EditorState` after `display`, and `plan_display: PlanDisplay::Tree,` in `EditorState::new`.

Change the imports line to `use crate::sql::{SqlError, StatementKind, analyse_statement, prepare_statement, relevant_sql, split_statements};`.

Replace `CommandService::explain` and add `explain_analyse`:

```rust
    /// Plain `EXPLAIN` (FR-016): `analyse` is hard-wired to `false` here, so no caller can reach
    /// `ANALYZE` through the ordinary Explain command.
    pub async fn explain(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError> {
        let sql = relevant_sql(&editor.document, editor.selection.clone(), editor.cursor)
            .map_err(query_selection_error)?;
        self.run_explain(editor, sql, false).await
    }

    /// `EXPLAIN ANALYZE` (FR3-019). Because it executes the statement, only a plain row-returning
    /// query may be analysed; anything else is refused before the provider is asked (§15.1).
    pub async fn explain_analyse(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError> {
        let sql = relevant_sql(&editor.document, editor.selection.clone(), editor.cursor)
            .map_err(query_selection_error)?;
        if analyse_statement(sql).kind != StatementKind::RowReturning {
            let error = QueryError {
                message: "Explain Analyze runs only row-returning statements (SELECT, WITH … SELECT, VALUES); the statement would execute".into(),
                severity: None,
                code: None,
                detail: None,
                hint: None,
                position: None,
            };
            editor.execution_status = ExecutionStatus::Failed;
            editor.error = Some(error.clone());
            return Err(error);
        }
        self.run_explain(editor, sql, true).await
    }

    async fn run_explain(
        &self,
        editor: &mut EditorState,
        sql: &str,
        analyse: bool,
    ) -> Result<QueryResult, QueryError> {
        editor.execution_status = ExecutionStatus::Running;
        editor.error = None;
        match self.provider.explain(sql, analyse).await {
            Ok(result) => {
                editor.execution_status = ExecutionStatus::Completed;
                editor.results = vec![result.clone()];
                Ok(result)
            }
            Err(error) => {
                editor.execution_status = failure_status(&error);
                editor.error = Some(error.clone());
                Err(error)
            }
        }
    }
```

Check `relevant_sql` returns `&str` (it does: `src/sql.rs:80`). If `StatementAnalysis::kind` is not `pub`, make it `pub` — grep `pub kind` in `src/sql.rs:14-20`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test application`
Expected: PASS. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/application.rs src/sql.rs
git commit -m "Add Explain Analyze as a guarded command" -m "sql.explain_analyse is its own command ID because ANALYZE executes the statement (FR3-019). It refuses anything analyse_statement cannot classify as row-returning before the provider is called; the refusal lands as a statement failure so earlier results survive. Plain Explain keeps analyse hard-wired to false (FR-016)."
```

---

### Task 5: Wire the command into the UI — toolbar, shortcut, status, header

**Files:**
- Modify: `src/ui.rs` — `RunMode` (≈3081), `run` (≈1235-1330), `dispatch_command` (≈1566), `handle_key` (≈1475-1485), `shortcut_command` (≈4068), toolbar (≈3520-3540), `result_header` (≈3926), `completion_status` (≈4041), the empty-results hint (≈2990), tests.

**Interfaces:**
- Consumes: `command::EXPLAIN_ANALYSE`, `CommandService::explain_analyse`, `QueryResult::plan`, `QueryPlan::node_count`, `QueryPlan::analysed`.

- [ ] **Step 1: Write the failing tests**

In `src/ui.rs` `mod tests`, extend `core_shortcuts_resolve_to_stable_command_ids` with:

```rust
        assert_eq!(
            shortcut_command("enter", true, true, true, false, true),
            Some(command::EXPLAIN_ANALYSE)
        );
```

In `execution_shortcuts_are_refused_while_a_query_is_running` change the modifier list to `[(false, false), (true, false), (false, true), (true, true)]`.

Add new tests:

```rust
    /// FR3-019: the toolbar carries Explain Analyze as its own action, and a refused statement
    /// reports the refusal where every other failure is reported.
    #[gpui::test]
    fn explain_analyze_is_refused_for_a_modifying_statement(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| {
            app.editor.document = "DELETE FROM customer;".into();
            app.editor.cursor = 3;
            app.connect(cx);
        });
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.dispatch_command(command::EXPLAIN_ANALYSE, cx));
        wait_for_execution_status(&view, cx, ExecutionStatus::Failed);

        view.update(cx, |app, _| {
            assert!(app.status.starts_with("Query failed: Explain Analyze runs only"));
            assert!(app.editor.error.is_some());
            assert_eq!(app.editor.document, "DELETE FROM customer;");
        });
    }

    #[gpui::test]
    fn explain_analyze_completes_with_a_plan_and_a_node_count(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1;".into();
            app.editor.cursor = 3;
            app.connect(cx);
        });
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        cx.simulate_keystrokes("cmd-alt-shift-enter");
        wait_for_execution_status(&view, cx, ExecutionStatus::Completed);

        view.update(cx, |app, _| {
            let plan = app.editor.results[0].plan.as_ref().expect("a plan result");
            assert!(plan.analysed);
            assert_eq!(app.status, "Explained in 38 ms · 7 nodes");
        });
    }

    #[gpui::test]
    fn plain_explain_never_analyses(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1;".into();
            app.editor.cursor = 3;
            app.connect(cx);
        });
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.dispatch_command(command::EXPLAIN, cx));
        wait_for_execution_status(&view, cx, ExecutionStatus::Completed);

        view.update(cx, |app, _| {
            let plan = app.editor.results[0].plan.as_ref().expect("a plan result");
            assert!(!plan.analysed);
            assert!(plan.root.actual.is_none());
        });
    }

    #[test]
    fn a_plan_result_reports_nodes_rather_than_rows() {
        let result = crate::plan::sample_plan().into_result(std::time::Duration::from_millis(38));
        assert_eq!(completion_status(&[result]), "Explained in 38 ms · 7 nodes");
    }
```

Check how existing tests spell the keystroke: grep `simulate_keystrokes("cmd-` in `src/ui.rs` and copy the modifier spelling (`cmd-alt-shift-enter` follows GPUI's `ctrl-alt-shift-` ordering; use whatever the existing explain shortcut test uses, e.g. `cmd-alt-enter`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ui::tests::explain_analyze && cargo test core_shortcuts_resolve`
Expected: compilation error on `command::EXPLAIN_ANALYSE` in `shortcut_command` expectations / failing assertions.

- [ ] **Step 3: Implement**

`RunMode` (≈line 3081):

```rust
#[derive(Clone, Copy)]
enum RunMode {
    Current,
    All,
    Explain,
    ExplainAnalyse,
}
```

`run` — status text match:

```rust
        self.status = match mode {
            RunMode::Current => "Running statement…",
            RunMode::All => "Running all statements…",
            RunMode::Explain => "Explaining statement…",
            RunMode::ExplainAnalyse => "Explaining statement with ANALYZE…",
        }
        .into();
```

and inside the `runtime.spawn` match, after the `RunMode::Explain` arm:

```rust
                            RunMode::ExplainAnalyse => service
                                .explain_analyse(&mut editor)
                                .await
                                .map(|_| ())
                                .map_err(|error| RunFailure {
                                    statement_index: None,
                                    error,
                                }),
```

`dispatch_command`: add `command::EXPLAIN_ANALYSE => self.run(RunMode::ExplainAnalyse, cx),` after the `EXPLAIN` arm.

`handle_key` — the `executes_the_document` list becomes:

```rust
            let executes_the_document = matches!(
                command_id,
                command::RUN | command::RUN_ALL | command::EXPLAIN | command::EXPLAIN_ANALYSE
            );
```

`shortcut_command` — insert **before** the `"enter" if shift` arm:

```rust
            "enter" if shift && alt => runnable.then_some(command::EXPLAIN_ANALYSE),
```

Toolbar — after the `"explain"` button add:

```rust
                                    .child(button(
                                        &self.fonts,
                                        "explain-analyse",
                                        "Explain Analyze",
                                        Tone::Neutral,
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.dispatch_command(command::EXPLAIN_ANALYSE, cx)
                                        }),
                                    ))
```

Empty-results hint (≈line 2990): change the string to `"⌘↵ RUN · ⇧⌘↵ RUN ALL · ⌥⌘↵ EXPLAIN · ⌥⇧⌘↵ ANALYZE"`.

`result_header` — replace the `count` computation and the first pill with:

```rust
    let count = match &result.plan {
        Some(plan) => format!("{} nodes", plan.node_count()),
        None if result.columns.is_empty() => {
            format!("{} affected", result.affected_rows.unwrap_or(0))
        }
        None => format!("{} rows", result.rows.len()),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_5()
        .py_3()
        .child(
            div()
                .text_size(px(11.))
                .font_family(fonts.mono.clone())
                .text_color(rgb(MUTED))
                .child(format!("RESULT {}", index + 1)),
        )
        .child(metric_pill(
            fonts,
            format!("{count} · {} ms", result.execution_time.as_millis()),
            ACCENT,
        ))
        // ANALYZE executed the statement; say so where the result is read (FR3-019).
        .children(
            result
                .plan
                .as_ref()
                .filter(|plan| plan.analysed)
                .map(|_| metric_pill(fonts, "EXPLAIN ANALYZE".into(), WARN)),
        )
```

(keep the existing `automatic_limit` and notice pills after it.)

`completion_status`:

```rust
fn completion_status(results: &[QueryResult]) -> String {
    let elapsed: u128 = results
        .iter()
        .map(|result| result.execution_time.as_millis())
        .sum();
    if let [result] = results
        && let Some(plan) = &result.plan
    {
        return format!("Explained in {elapsed} ms · {} nodes", plan.node_count());
    }
    let rows: usize = results.iter().map(|result| result.rows.len()).sum();
    format!("Completed in {elapsed} ms · {rows} rows")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ui::tests`
Expected: PASS. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "Offer Explain Analyze from the toolbar and keyboard" -m "A fourth toolbar action and ⌥⇧⌘↵ dispatch sql.explain_analyse through the same run path as Explain. A plan result reports its node count in the status bar and result header, and an analysed plan carries an amber EXPLAIN ANALYZE pill so the reader knows the statement ran."
```

---

### Task 6: TREE view, segments, node selection and the detail panel

**Files:**
- Modify: `src/ui.rs` — `AppView` fields (grep `result_selection: Option<ResultSelection>` for the struct and its initialiser), `run` completion (≈1300, where `result_selection = None`), `set_display` neighbourhood (≈1771), `display_segments` call site in the results pane header (≈3695), `results_surface` (≈2972-3030), `selectable_lines` (≈780), constants (≈41-77), tests.

**Interfaces:**
- Produces on `AppView`: `plan_selection: Option<usize>`, `fn set_plan_display(&mut self, PlanDisplay, cx)`, `fn select_plan_node(&mut self, index: usize, cx)`, `fn plan_segments(&self, cx) -> impl IntoElement`, `fn plan_surface(&self, result: &QueryResult, plan: &QueryPlan, cx) -> gpui::AnyElement`, `fn plan_tree(&self, plan: &QueryPlan, cx) -> gpui::Div`, `fn plan_detail(&self, plan: &QueryPlan, index: usize) -> gpui::Div`.
- Element ids / debug selectors: `plan-node-{index}` on every tree row; `plan-tree`, `plan-graph`, `plan-text` on the segments.
- Consumes: `PlanDisplay`, `QueryPlan::rows`, `PlanRow`, `QueryPlan::node`.
- Task 7 adds `plan_graph`; until then `PlanDisplay::Graph` renders the tree (Task 7 replaces one match arm).

- [ ] **Step 1: Write the failing tests**

```rust
    /// Opens the app, connects, explains `SELECT 1` and waits for the sample plan to land.
    fn explained_app(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<AppView>, &mut gpui::VisualTestContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1;".into();
            app.editor.cursor = 3;
            app.connect(cx);
        });
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.dispatch_command(command::EXPLAIN_ANALYSE, cx));
        wait_for_execution_status(&view, cx, ExecutionStatus::Completed);
        cx.run_until_parked();
        (view, cx)
    }

    /// FR3-020: a plan renders as one row per node, and the results header offers the three
    /// views in place of TABLE/TEXT.
    #[gpui::test]
    fn a_plan_renders_as_a_tree_with_one_row_per_node(cx: &mut TestAppContext) {
        let (view, cx) = explained_app(cx);

        for index in 0..7 {
            assert!(
                cx.debug_bounds(&format!("plan-node-{index}")).is_some(),
                "node {index} should be rendered"
            );
        }
        assert!(cx.debug_bounds("plan-node-7").is_none());
        assert!(cx.debug_bounds("plan-tree").is_some());
        assert!(cx.debug_bounds("plan-graph").is_some());
        assert!(cx.debug_bounds("plan-text").is_some());
        assert!(cx.debug_bounds("display-table").is_none());
        view.update(cx, |app, _| assert_eq!(app.editor.plan_display, PlanDisplay::Tree));
    }

    #[gpui::test]
    fn clicking_a_node_selects_it_and_clicking_again_clears_it(cx: &mut TestAppContext) {
        let (view, cx) = explained_app(cx);

        let row = cx.debug_bounds("plan-node-4").expect("the hot node row");
        cx.simulate_click(row.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| assert_eq!(app.plan_selection, Some(4)));
        assert!(cx.debug_bounds("plan-detail").is_some());

        let row = cx.debug_bounds("plan-node-4").expect("the row is still there");
        cx.simulate_click(row.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| assert_eq!(app.plan_selection, None));
        assert!(cx.debug_bounds("plan-detail").is_none());
    }

    #[gpui::test]
    fn the_text_segment_shows_the_text_plan_and_a_new_result_clears_the_selection(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = explained_app(cx);
        view.update(cx, |app, cx| app.select_plan_node(2, cx));

        let segment = cx.debug_bounds("plan-text").expect("the TEXT segment");
        cx.simulate_click(segment.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| assert_eq!(app.editor.plan_display, PlanDisplay::Text));
        assert!(cx.debug_bounds("plan-node-0").is_none());
        // Text mode renders result lines; the first is the column name.
        assert!(cx.debug_bounds("result-line-0").is_some());

        view.update(cx, |app, cx| app.dispatch_command(command::EXPLAIN, cx));
        wait_for_execution_status(&view, cx, ExecutionStatus::Completed);
        view.update(cx, |app, _| assert_eq!(app.plan_selection, None));
    }

    /// Whatever view is showing, COPY ALL puts the text plan on the clipboard.
    #[gpui::test]
    fn copying_a_plan_copies_its_text_form(cx: &mut TestAppContext) {
        let (view, cx) = explained_app(cx);
        view.update(cx, |app, cx| app.copy_results(cx));
        let copied = cx.read_from_clipboard().and_then(|item| item.text()).unwrap();
        assert!(copied.starts_with("QUERY PLAN\nLimit  (cost=4821.14..4821.16"));
        assert!(copied.contains("->  Seq Scan on orders o"));
    }
```

`result-line-{index}` is the id `result_line` already stamps on text lines (check `fn result_line` ≈2875); if it uses a different id, use that one.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test a_plan_renders_as_a_tree`
Expected: compilation errors (`plan_selection`, `select_plan_node`, `PlanDisplay` import).

- [ ] **Step 3: Implement**

Import: change the `use crate::application::{…}` line to include `PlanDisplay`; add `use crate::plan::{PlanRow, QueryPlan};`.

Constants (after `RESULT_TEXT_INSET`):

```rust
/// Plan tree: indent per depth, and the widths of its fixed columns.
const PLAN_INDENT: f32 = 22.;
const PLAN_NODE_COLUMN: f32 = 420.;
const PLAN_NUMBER_COLUMN: f32 = 120.;
const PLAN_ACTUAL_COLUMN: f32 = 130.;
const PLAN_TIME_COLUMN: f32 = 96.;
const PLAN_BAR_WIDTH: f32 = 120.;
/// The selected node's detail panel.
const PLAN_DETAIL_WIDTH: f32 = 340.;
```

`AppView` — add `plan_selection: Option<usize>,` beside `result_selection`, and `plan_selection: None,` in the initialiser. In `run`'s completion closure, next to `this.result_selection = None;` add `this.plan_selection = None;`.

Methods, next to `set_display`:

```rust
    fn set_plan_display(&mut self, display: PlanDisplay, cx: &mut Context<Self>) {
        self.editor.plan_display = display;
        cx.notify();
    }

    /// Selects a plan node for the detail panel; selecting it again clears it.
    fn select_plan_node(&mut self, index: usize, cx: &mut Context<Self>) {
        self.plan_selection = if self.plan_selection == Some(index) {
            None
        } else {
            Some(index)
        };
        cx.notify();
    }

    /// The plan the editor is showing, when its result is a plan. Explain always leaves exactly
    /// one result, so the first is the only one to look at.
    fn shown_plan(&self) -> Option<&QueryPlan> {
        self.editor.results.first().and_then(|result| result.plan.as_ref())
    }
```

Segments, next to `display_segments`:

```rust
    /// TREE / GRAPH / TEXT for a plan result, replacing TABLE / TEXT (FR3-020).
    fn plan_segments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.editor.plan_display;
        let mut control = segmented();
        for (id, label, display) in [
            ("plan-tree", "TREE", PlanDisplay::Tree),
            ("plan-graph", "GRAPH", PlanDisplay::Graph),
            ("plan-text", "TEXT", PlanDisplay::Text),
        ] {
            control = control.child(segment(
                &self.fonts,
                id,
                label,
                current == display,
                true,
                cx.listener(move |this, _, _, cx| this.set_plan_display(display, cx)),
            ));
        }
        control
    }
```

Results pane header (≈3695, `.child(self.display_segments(cx))`): replace with

```rust
                                            .child(if self.shown_plan().is_some() {
                                                self.plan_segments(cx).into_any_element()
                                            } else {
                                                self.display_segments(cx).into_any_element()
                                            })
```

`results_surface` — inside the `for (index, result)` loop, before `content = match self.editor.display {`, add:

```rust
            if let Some(plan) = &result.plan
                && self.editor.plan_display != PlanDisplay::Text
            {
                content = content.child(self.plan_surface(plan, cx));
                line_index += result.rows.len();
                continue;
            }
```

(`line_index` still advances so a later error card or copy addressing stays consistent; Text mode falls through to the existing text renderer.)

`selectable_lines` — plans copy their text lines whichever view shows. In the `flat_map` closure, before `match self.editor.display`, add:

```rust
                if result.plan.is_some() {
                    return result
                        .as_text()
                        .lines()
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                }
```

The tree, the surface and the detail panel (new methods on `AppView`, next to `selectable_table`):

```rust
    /// The plan views with the detail panel alongside when a node is selected (§15.2).
    fn plan_surface(&self, plan: &QueryPlan, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = match self.editor.plan_display {
            PlanDisplay::Graph => self.plan_tree(plan, cx), // Task 7 swaps in plan_graph
            _ => self.plan_tree(plan, cx),
        };
        let mut surface = table_shell(&self.fonts).flex_row().items_start();
        surface = surface.child(view);
        if let Some(index) = self.plan_selection.filter(|index| plan.node(*index).is_some()) {
            surface = surface.child(self.plan_detail(plan, index));
        }
        surface.into_any_element()
    }

    /// One row per node, indented by depth, with the metrics as columns and a cost-share bar.
    fn plan_tree(&self, plan: &QueryPlan, cx: &mut Context<Self>) -> gpui::Div {
        let analysed = plan.analysed;
        let mut header = div()
            .flex()
            .flex_none()
            .h(px(RESULT_ROW_HEIGHT))
            .border_b_1()
            .border_color(rgb(BORDER));
        let mut columns = vec![("NODE", PLAN_NODE_COLUMN), ("EST ROWS", PLAN_NUMBER_COLUMN)];
        if analysed {
            columns.push(("ACTUAL ROWS", PLAN_ACTUAL_COLUMN));
            columns.push(("TIME", PLAN_TIME_COLUMN));
        }
        columns.push(("COST", PLAN_NUMBER_COLUMN));
        columns.push(("SHARE", PLAN_BAR_WIDTH + 24.));
        for (name, width) in columns {
            header = header.child(
                div()
                    .w(px(width))
                    .flex_none()
                    .px_4()
                    .py(px(11.))
                    .text_size(px(10.))
                    .text_color(rgb(FAINT))
                    .child(name),
            );
        }
        let mut tree = div().flex().flex_col().flex_none().child(header);
        for row in plan.rows() {
            tree = tree.child(self.plan_tree_row(&row, analysed, cx));
        }
        tree
    }

    fn plan_tree_row(
        &self,
        row: &PlanRow<'_>,
        analysed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let index = row.index;
        let id = SharedString::from(format!("plan-node-{index}"));
        let selector = id.clone();
        let selected = self.plan_selection == Some(index);
        let detail = row
            .node
            .properties
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut element = div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_none()
            .items_center()
            .h(px(RESULT_ROW_HEIGHT))
            .border_b_1()
            .border_color(rgb(BORDER))
            .cursor_pointer()
            .when(selected, |row| row.bg(rgb(ACCENT_SOFT)))
            .when(!selected, |row| row.hover(|style| style.bg(rgb(PANEL_LIGHT))))
            // The error card's red strip marks the node the plan spent most on (§15.2).
            .when(row.hottest, |row| row.border_l(px(3.)).border_color(rgb(RED)))
            .on_click(cx.listener(move |this, _, _, cx| this.select_plan_node(index, cx)))
            .child(
                div()
                    .w(px(PLAN_NODE_COLUMN))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pr_4()
                    .pl(px(16. + PLAN_INDENT * row.depth as f32))
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(13.))
                            .whitespace_nowrap()
                            .child(row.node.heading()),
                    )
                    .when(!detail.is_empty(), |cell| {
                        cell.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(detail),
                        )
                    }),
            )
            .child(plan_number_cell(
                format_count(row.node.plan_rows),
                PLAN_NUMBER_COLUMN,
            ));
        if analysed {
            let (actual_rows, time) = match &row.node.actual {
                Some(actual) => (
                    format_count(actual.rows * actual.loops),
                    format!("{:.2} ms", actual.total_time_ms),
                ),
                None => ("—".to_owned(), "—".to_owned()),
            };
            let mut actual_cell = div()
                .w(px(PLAN_ACTUAL_COLUMN))
                .flex_none()
                .flex()
                .items_baseline()
                .justify_end()
                .gap(px(6.))
                .px_4()
                .text_size(px(13.))
                .child(actual_rows);
            if let Some(flag) = estimate_flag(row.estimate_ratio) {
                actual_cell = actual_cell.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(WARN))
                        .child(flag),
                );
            }
            element = element
                .child(actual_cell)
                .child(plan_number_cell(time, PLAN_TIME_COLUMN));
        }
        element
            .child(plan_number_cell(
                format!("{:.2}", row.node.total_cost),
                PLAN_NUMBER_COLUMN,
            ))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .child(share_bar(row.share, row.hottest, PLAN_BAR_WIDTH)),
            )
    }

    /// Everything about one node: the figures, every server property, and its own text lines.
    fn plan_detail(&self, plan: &QueryPlan, index: usize) -> gpui::Div {
        let Some(node) = plan.node(index) else {
            return div();
        };
        let row = plan.rows().into_iter().find(|row| row.index == index);
        let share = row.as_ref().map_or(0.0, |row| row.share);
        let hottest = row.as_ref().is_some_and(|row| row.hottest);
        let mut subtitle = format!("{:.0}% of plan cost", share * 100.);
        if hottest {
            subtitle.push_str(" · most expensive node");
        }
        let mut pairs: Vec<(String, String)> = vec![
            ("Startup cost".into(), format!("{:.2}", node.startup_cost)),
            ("Total cost".into(), format!("{:.2}", node.total_cost)),
            ("Plan rows".into(), format_count(node.plan_rows)),
            ("Width".into(), node.plan_width.to_string()),
        ];
        if let Some(actual) = &node.actual {
            let mut rows = format_count(actual.rows * actual.loops);
            if let Some(flag) = estimate_flag(row.and_then(|row| row.estimate_ratio)) {
                rows.push_str(&format!(" · {flag}"));
            }
            pairs.push(("Actual rows".into(), rows));
            pairs.push((
                "Actual time".into(),
                format!("{:.3} .. {:.3} ms", actual.startup_time_ms, actual.total_time_ms),
            ));
            pairs.push(("Loops".into(), format!("{:.0}", actual.loops)));
        }
        pairs.extend(node.properties.iter().cloned());
        let mut panel = div()
            .id("plan-detail")
            .debug_selector(|| "plan-detail".to_owned())
            .w(px(PLAN_DETAIL_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(14.))
            .px(px(20.))
            .py(px(18.))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_family(self.fonts.display.clone())
                            .child(node.heading()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_family(self.fonts.body.clone())
                            .text_color(rgb(MUTED))
                            .child(subtitle),
                    ),
            );
        let mut table = div().flex().flex_col();
        for (key, value) in pairs {
            table = table.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .py(px(7.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .text_size(px(12.))
                    .child(div().text_color(rgb(MUTED)).child(key))
                    .child(div().text_right().child(value)),
            );
        }
        panel = panel.child(table);
        let mut raw = div()
            .flex()
            .flex_col()
            .px(px(12.))
            .py(px(10.))
            .rounded(px(CONTROL_RADIUS))
            .bg(rgb(PANEL_LIGHT))
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(rgb(MUTED));
        for line in node.text_lines(0) {
            raw = raw.child(div().whitespace_nowrap().child(line));
        }
        panel
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(FAINT))
                    .child("RAW"),
            )
            .child(raw)
    }
```

Free helpers, next to `metric_pill`:

```rust
/// A right-aligned numeric cell of the plan tree.
fn plan_number_cell(text: String, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_none()
        .flex()
        .justify_end()
        .px_4()
        .text_size(px(13.))
        .whitespace_nowrap()
        .child(text)
}

/// Thousands separated by thin spaces, as the design shows them: `48 317`.
fn format_count(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut grouped = String::new();
    for (position, character) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(character);
    }
    grouped
}

/// `↑2.3×` for an under-estimate of at least 2×, `↓2.0×` for an over-estimate of at least 2×.
fn estimate_flag(ratio: Option<f64>) -> Option<String> {
    let ratio = ratio?;
    if ratio >= 2.0 {
        Some(format!("↑{ratio:.1}×"))
    } else if ratio > 0.0 && ratio <= 0.5 {
        Some(format!("↓{:.1}×", 1.0 / ratio))
    } else {
        None
    }
}

/// The cost-share bar: red for the hottest node, amber for anything at 5 % or more, faint below.
fn share_bar(share: f64, hottest: bool, width: f32) -> gpui::Div {
    let colour = if hottest {
        RED
    } else if share >= 0.05 {
        WARN
    } else {
        FAINT
    };
    div()
        .relative()
        .w(px(width))
        .h(px(6.))
        .rounded_full()
        .bg(rgb(PANEL_LIGHT))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(px(0.))
                .top(px(0.))
                .bottom(px(0.))
                .w(px((width * share as f32).max(2.)))
                .rounded_full()
                .bg(rgb(colour)),
        )
}
```

Add unit tests for the two pure helpers in `mod tests`:

```rust
    #[test]
    fn counts_group_thousands_with_spaces() {
        assert_eq!(format_count(0.0), "0");
        assert_eq!(format_count(999.0), "999");
        assert_eq!(format_count(4960.0), "4 960");
        assert_eq!(format_count(151683.0), "151 683");
    }

    #[test]
    fn estimate_flags_mark_two_fold_misses_in_either_direction() {
        assert_eq!(estimate_flag(None), None);
        assert_eq!(estimate_flag(Some(1.0)), None);
        assert_eq!(estimate_flag(Some(1.9)), None);
        assert_eq!(estimate_flag(Some(48317.0 / 21092.0)).as_deref(), Some("↑2.3×"));
        assert_eq!(estimate_flag(Some(0.25)).as_deref(), Some("↓4.0×"));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ui::tests`
Expected: PASS. If `debug_bounds` cannot find `plan-node-*` rows, confirm the rows are inside the rendered scroll surface (they are children of `results_surface`, which the pane renders only when `pane_visible`; `explained_app` leaves the destination as `Pane` and focus on the editor, so the pane is visible). Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "Render execution plans as a tree with a node detail panel" -m "A plan result shows one row per node — indented by depth, with estimated and actual rows, time, cost and a share bar — and TREE / GRAPH / TEXT segments replace TABLE / TEXT. Clicking a node opens a panel with its figures, every server property and its own text lines; COPY ALL copies the text plan whichever view is showing (FR3-020)."
```

---

### Task 7: GRAPH view

**Files:**
- Modify: `src/ui.rs` — constants, `plan_surface` (the `PlanDisplay::Graph` arm), new `plan_graph`, tests.

**Interfaces:**
- Produces: `fn plan_graph(&self, plan: &QueryPlan, cx) -> gpui::Div`, `fn edge_thickness(rows: f64) -> f32`.
- Consumes: `QueryPlan::graph_layout`, `GraphPlacement`, `GraphEdge`, `select_plan_node`, `share_bar`, `format_count`, `estimate_flag`.
- Element ids: the same `plan-node-{index}` on graph boxes (only one view renders at a time).

- [ ] **Step 1: Write the failing tests**

```rust
    #[gpui::test]
    fn the_graph_view_places_nodes_by_depth_and_selects_on_click(cx: &mut TestAppContext) {
        let (view, cx) = explained_app(cx);
        let segment = cx.debug_bounds("plan-graph").expect("the GRAPH segment");
        cx.simulate_click(segment.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| assert_eq!(app.editor.plan_display, PlanDisplay::Graph));

        let limit = cx.debug_bounds("plan-node-0").expect("root box");
        let sort = cx.debug_bounds("plan-node-1").expect("child box");
        let orders = cx.debug_bounds("plan-node-4").expect("scan box");
        let hash = cx.debug_bounds("plan-node-5").expect("hash box");
        assert!(sort.origin.x > limit.origin.x, "children sit to the right of their parent");
        assert_eq!(orders.origin.x, hash.origin.x, "siblings share a column");
        assert!(hash.origin.y > orders.origin.y, "siblings are stacked in leaf order");
        assert_eq!(limit.size.width, px(PLAN_GRAPH_NODE_WIDTH));

        cx.simulate_click(orders.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| assert_eq!(app.plan_selection, Some(4)));
        assert!(cx.debug_bounds("plan-detail").is_some());
    }

    #[test]
    fn edge_thickness_grows_with_the_logarithm_of_rows() {
        assert_eq!(edge_thickness(0.0), 1.0);
        assert_eq!(edge_thickness(1.0), 1.0);
        assert!(edge_thickness(10.0) > edge_thickness(1.0));
        assert!(edge_thickness(48317.0) > edge_thickness(4960.0));
        assert!(edge_thickness(1e12) <= 8.0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test the_graph_view_places_nodes && cargo test edge_thickness_grows`
Expected: compilation error — `PLAN_GRAPH_NODE_WIDTH`, `edge_thickness` not found; the graph test would also fail because Graph still renders the tree.

- [ ] **Step 3: Implement**

Constants:

```rust
/// Plan graph: node box and the pitch between columns (depth) and rows (leaf order).
const PLAN_GRAPH_NODE_WIDTH: f32 = 132.;
const PLAN_GRAPH_NODE_HEIGHT: f32 = 118.;
const PLAN_GRAPH_COLUMN_PITCH: f32 = 148.;
const PLAN_GRAPH_ROW_PITCH: f32 = 160.;
const PLAN_GRAPH_PADDING: f32 = 24.;
```

Import `use crate::plan::{GraphLayout, PlanRow, QueryPlan};` (extend the existing line).

In `plan_surface` change the `Graph` arm to `PlanDisplay::Graph => self.plan_graph(plan, cx),`.

New method next to `plan_tree`:

```rust
    /// Root at the left, children to the right, connectors as thick as the rows that flowed
    /// along them. Positions come from the model's layout; this only multiplies by pitches.
    fn plan_graph(&self, plan: &QueryPlan, cx: &mut Context<Self>) -> gpui::Div {
        let layout: GraphLayout = plan.graph_layout();
        let rows = plan.rows();
        let position = |index: usize| -> (f32, f32) {
            let placement = &layout.placements[index];
            (
                PLAN_GRAPH_PADDING + placement.column as f32 * PLAN_GRAPH_COLUMN_PITCH,
                PLAN_GRAPH_PADDING + placement.row * PLAN_GRAPH_ROW_PITCH,
            )
        };
        let width = PLAN_GRAPH_PADDING * 2.
            + layout.columns.saturating_sub(1) as f32 * PLAN_GRAPH_COLUMN_PITCH
            + PLAN_GRAPH_NODE_WIDTH;
        let height = PLAN_GRAPH_PADDING * 2.
            + layout.leaf_rows.saturating_sub(1) as f32 * PLAN_GRAPH_ROW_PITCH
            + PLAN_GRAPH_NODE_HEIGHT;
        let mut canvas = div().relative().flex_none().w(px(width)).h(px(height));
        // Connectors first, so the boxes paint over their ends.
        for edge in &layout.edges {
            let (parent_x, parent_y) = position(edge.parent);
            let (child_x, child_y) = position(edge.child);
            let thickness = edge_thickness(edge.rows);
            let start_x = parent_x + PLAN_GRAPH_NODE_WIDTH;
            let start_y = parent_y + PLAN_GRAPH_NODE_HEIGHT / 2.;
            let end_y = child_y + PLAN_GRAPH_NODE_HEIGHT / 2.;
            let elbow_x = start_x + 8.;
            let top = start_y.min(end_y);
            canvas = canvas
                .child(connector(start_x, start_y - thickness / 2., elbow_x - start_x, thickness))
                .child(connector(
                    elbow_x - thickness / 2.,
                    top - thickness / 2.,
                    thickness,
                    (start_y - end_y).abs() + thickness,
                ))
                .child(connector(elbow_x, end_y - thickness / 2., child_x - elbow_x, thickness));
        }
        for row in &rows {
            let (x, y) = position(row.index);
            let index = row.index;
            let id = SharedString::from(format!("plan-node-{index}"));
            let selector = id.clone();
            let selected = self.plan_selection == Some(index);
            let (rows_line, time_line) = match &row.node.actual {
                Some(actual) => (
                    format!("rows {}", format_count(actual.rows * actual.loops)),
                    format!("{:.1} ms", actual.total_time_ms),
                ),
                None => (format!("rows {}", format_count(row.node.plan_rows)), String::new()),
            };
            canvas = canvas.child(
                div()
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(PLAN_GRAPH_NODE_WIDTH))
                    .h(px(PLAN_GRAPH_NODE_HEIGHT))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(rgb(if row.hottest || selected { PANEL_HIGH } else { PANEL_LIGHT }))
                    .border_1()
                    .border_color(rgb(if row.hottest {
                        RED
                    } else if selected {
                        ACCENT
                    } else {
                        BORDER
                    }))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.select_plan_node(index, cx)))
                    .child(
                        div()
                            .h(px(30.))
                            .text_size(px(12.5))
                            .line_height(px(15.))
                            .font_family(self.fonts.display.clone())
                            .overflow_hidden()
                            .child(row.node.operation.clone())
                            .children(row.node.target.clone().map(|target| {
                                div().text_color(rgb(MUTED)).whitespace_nowrap().child(target)
                            })),
                    )
                    .child(div().text_size(px(10.5)).whitespace_nowrap().child(rows_line))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.5))
                            .text_color(rgb(FAINT))
                            .whitespace_nowrap()
                            .child(format!("est {}", format_count(row.node.plan_rows)))
                            .children(estimate_flag(row.estimate_ratio).map(|flag| {
                                div().text_color(rgb(WARN)).child(flag)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.5))
                            .text_color(rgb(MUTED))
                            .whitespace_nowrap()
                            .child(time_line)
                            .child(format!("{:.0}%", row.share * 100.)),
                    )
                    .child(
                        div()
                            .mt_auto()
                            .child(share_bar(row.share, row.hottest, PLAN_GRAPH_NODE_WIDTH - 24.)),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(canvas)
            .child(
                div()
                    .flex()
                    .gap_6()
                    .px(px(PLAN_GRAPH_PADDING))
                    .pb(px(20.))
                    .text_size(px(10.))
                    .text_color(rgb(FAINT))
                    .child("CONNECTOR WIDTH ∝ ROWS")
                    .child("BAR = SHARE OF PLAN COST")
                    .child("TIME IS INCLUSIVE OF CHILDREN"),
            )
    }
```

Free helpers next to `share_bar`:

```rust
/// One straight segment of an elbow connector between two graph nodes.
fn connector(x: f32, y: f32, width: f32, height: f32) -> gpui::Div {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width.max(0.)))
        .h(px(height.max(0.)))
        .rounded_full()
        .bg(rgb(PANEL_HIGH))
}

/// Connector thickness in pixels: 1 px for a row or none, growing with log10 of the rows, capped
/// so a hundred-million-row scan does not paint a wall.
fn edge_thickness(rows: f64) -> f32 {
    (1.0 + rows.max(1.0).log10() as f32 * 1.2).clamp(1.0, 8.0)
}
```

The tree's node column in `plan_tree_row` uses `whitespace_nowrap()` on the heading; the graph's `.mt_auto()` needs the box to be a flex column, which it is.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ui::tests`
Expected: PASS. Then `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Check it by eye**

Run `cargo run` against a live database (any `.env` with `DATABASE_URL`), type a join query, press ⌥⇧⌘↵, and switch TREE → GRAPH → TEXT. Confirm: the hottest node has the red strip/border, the pane scrolls sideways when the graph is wider than the pane, and clicking a box opens the detail panel. Fix anything that looks wrong before committing.

- [ ] **Step 6: Commit**

```bash
git add src/ui.rs
git commit -m "Draw execution plans as a node graph" -m "GRAPH lays the plan out root-left from the model's column/row placement, joins parent to child with elbow connectors whose thickness follows the rows that flowed along them, and shares the node selection and detail panel with the tree."
```

---

### Task 8: Live acceptance test and documentation

**Files:**
- Modify: `tests/postgres_smoke.rs` (≈line 94-101), `CLAUDE.md`, `README.md`

- [ ] **Step 1: Extend the live smoke test**

Replace the existing plain-explain block (`editor.document = "SELECT 1;"` … `assert!(!explained.rows.is_empty());`) with:

```rust
    editor.document = "SELECT 1;".into();
    editor.cursor = 3;
    let explained = service
        .explain(&mut editor)
        .await
        .expect("plain EXPLAIN should execute");
    let plan = explained.plan.as_ref().expect("EXPLAIN should yield a plan");
    assert!(!plan.analysed, "FR-016: plain Explain must not analyse");
    assert!(plan.root.actual.is_none());
    assert!(plan.planning_time_ms.is_some());
    assert!(!explained.rows.is_empty(), "the text lines travel with the plan");

    editor.document = "SELECT generate_series(1, 1000) AS number ORDER BY number DESC;".into();
    editor.cursor = 3;
    let analysed = service
        .explain_analyse(&mut editor)
        .await
        .expect("EXPLAIN ANALYZE of a SELECT should execute");
    let plan = analysed.plan.as_ref().expect("EXPLAIN ANALYZE should yield a plan");
    assert!(plan.analysed);
    assert!(plan.execution_time_ms.is_some());
    assert!(plan.node_count() >= 2, "a sort over a function scan");
    assert!(
        plan.rows().iter().any(|row| row.hottest),
        "some node must be the most expensive"
    );

    editor.document = "CREATE TEMP TABLE smoke_refused (id int);".into();
    editor.cursor = 3;
    let refused = service
        .explain_analyse(&mut editor)
        .await
        .expect_err("FR3-019: ANALYZE of DDL is refused before execution");
    assert!(refused.message.starts_with("Explain Analyze runs only"));
```

- [ ] **Step 2: Run the live test if a server is available**

Run: `RUSTY_SQL_TEST_DATABASE_URL=postgres://… cargo test --test postgres_smoke -- --ignored`
Expected: PASS. Without a server, run `cargo test --test postgres_smoke` (compiles, tests stay ignored) and say so in the commit body.

- [ ] **Step 3: Update `CLAUDE.md`**

- Repository state: after the Phase 2 sentence add "Phase 3 Milestone 8 (enhanced `EXPLAIN` — FR3-019, FR3-020) is implemented on top; the rest of Phase 3 remains a proposal." Correct the stale "`main` still holds only the PRDs" clause — `main` carries the implementation.
- Module map: add rows `src/plan.rs | model | QueryPlan, PlanNode, per-node metrics, text form, graph layout` and `src/postgres/plan.rs | provider | EXPLAIN (FORMAT JSON) parsing into the plan model`.
- Commands section: add `sql.explain_analyse` to the list of command IDs; note the shortcut ⌥⇧⌘↵.
- Invariants: extend the Explain bullet: "**Explain uses plain `EXPLAIN`, never `EXPLAIN ANALYZE`**; Explain Analyze is a separate command that refuses anything but row-returning statements before execution."
- Gotchas: "`src/ui.rs` is ~5 000 lines" → update to the current `wc -l`.

- [ ] **Step 4: Update `README.md`**

Grep `Explain` in `README.md`; where the toolbar or shortcuts are listed, add Explain Analyze (⌥⇧⌘↵) and one sentence: "Plans render as a tree, a graph or text; click a node for its detail."

- [ ] **Step 5: Final verification**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean, all green.

- [ ] **Step 6: Commit**

```bash
git add tests/postgres_smoke.rs CLAUDE.md README.md
git commit -m "Cover enhanced EXPLAIN against a live server and document it" -m "The smoke test now asserts a plain Explain carries a plan without actual figures, an Explain Analyze of a SELECT carries execution time and a hottest node, and DDL is refused before it runs. CLAUDE.md and the README describe the new command, module and views."
```

---

## Self-review

**Spec coverage.** §15.1 separate command with its own ID → Task 4; refusal of non-row-returning statements → Task 4 (guard) and Task 5 (surfaced); blocked on read-only/production profiles → explicitly out of scope (spec). §15.2 node tree with cost and rows → Task 6; actual vs estimated → Tasks 1 and 6; highlighting the most expensive nodes → Tasks 1, 6, 7; raw text plan remains available → Tasks 1, 3, 6 (TEXT segment, COPY ALL). FR3-020 "structured node tree" → Task 6; graph → Task 7; detail panel (spec decision 7) → Task 6.

**Placeholder scan.** Every code step carries the code. The one intentional forward reference — `plan_surface`'s Graph arm rendering the tree until Task 7 — is stated in both tasks.

**Type consistency.** `QueryPlan::rows()` returns `Vec<PlanRow<'_>>` everywhere; `PlanRow` fields `index/depth/node/exclusive_cost/share/hottest/estimate_ratio` are used with those names in Tasks 1, 6, 7. `graph_layout()` returns `GraphLayout { placements, edges, columns, leaf_rows }` in Tasks 1 and 7. `into_result(Duration)` in Tasks 3, 5, 6. `prepare_explain(sql, analyse)` in Tasks 3 and 4 (Task 4 stops importing it). `DatabaseProvider::explain(sql, analyse)` implemented by `PostgresProvider`, `FakeProvider`, `UiTestProvider` in Task 3. `PlanNode::heading` (Task 1) and `target()` (Task 2) agree on the `using … on …` wording.
