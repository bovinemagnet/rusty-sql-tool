//! The execution plan model (FR3-020). Engine-neutral: the PostgreSQL provider builds it, the
//! views render it, and neither side sees the other.

use std::time::Duration;

use crate::result::{CellValue, Column, ExecutionStatus, QueryResult};

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
        place(
            &self.root,
            0,
            &mut next_index,
            &mut next_leaf_row,
            &mut layout,
        );
        layout.leaf_rows = next_leaf_row;
        layout.columns = layout
            .placements
            .iter()
            .map(|placement| placement.column + 1)
            .max()
            .unwrap_or(0);
        layout
    }

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
}

fn collect_rows<'a>(node: &'a PlanNode, depth: usize, rows: &mut Vec<PlanRow<'a>>) {
    let estimate_ratio = node
        .actual
        .filter(|_| node.plan_rows > 0.0)
        .map(|actual| actual.rows * actual.loops / node.plan_rows);
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
            // Push before recursing so edges come out in preorder of the child, matching the
            // placements.
            layout.edges.push(GraphEdge {
                parent: index,
                child: child_index,
                rows: child.rows_over_all_loops(),
            });
            let child_row = place(child, depth + 1, next_index, next_leaf_row, layout);
            first.get_or_insert(child_row);
            last = child_row;
        }
        (first.unwrap_or(0.0) + last) / 2.0
    };
    layout.placements[index].row = row;
    row
}

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
        assert_eq!(
            rows.iter().map(|row| row.index).collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
        assert_eq!(plan.node_count(), 7);
        assert_eq!(
            plan.node(4).map(PlanNode::heading).as_deref(),
            Some("Seq Scan on orders o")
        );
        assert!(plan.node(7).is_none());
    }

    /// Exclusive cost is what a node itself cost; a node cheaper than its child (Limit) is zero,
    /// never negative.
    #[test]
    fn exclusive_cost_and_share_are_derived_from_children() {
        let plan = sample_plan();
        let rows = plan.rows();
        let exclusive: Vec<f64> = rows.iter().map(|row| row.exclusive_cost).collect();
        for (actual, expected) in exclusive
            .iter()
            .zip([0.0, 119.30, 192.10, 268.11, 4102.00, 0.0, 152.00])
        {
            assert_close(*actual, expected);
        }
        assert_close(rows.iter().map(|row| row.share).sum::<f64>(), 1.0);
        assert_close(rows[4].share, 4102.00 / 4833.51);
    }

    #[test]
    fn the_hottest_node_is_the_largest_exclusive_cost() {
        let plan = sample_plan();
        let hottest: Vec<usize> = plan
            .rows()
            .iter()
            .filter(|row| row.hottest)
            .map(|row| row.index)
            .collect();
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
        let edges: Vec<(usize, usize)> = layout
            .edges
            .iter()
            .map(|edge| (edge.parent, edge.child))
            .collect();
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

    #[test]
    fn a_plan_result_carries_the_text_lines_and_the_plan() {
        let result = sample_plan().into_result(std::time::Duration::from_millis(38));
        assert_eq!(result.columns.len(), 1);
        assert_eq!(result.columns[0].name, "QUERY PLAN");
        assert_eq!(result.rows.len(), sample_plan().as_text().lines().count());
        assert_eq!(
            result.rows[0][0],
            crate::result::CellValue::Text(
                sample_plan().as_text().lines().next().unwrap().to_owned()
            )
        );
        assert_eq!(result.plan, Some(sample_plan()));
        assert_eq!(result.execution_time, std::time::Duration::from_millis(38));
        assert_eq!(result.status, crate::result::ExecutionStatus::Completed);
        // The text view prints the rows verbatim, one per line, and then its status line.
        assert!(result.as_text().starts_with("QUERY PLAN\nLimit  (cost="));
    }
}

/// The seven-node `EXPLAIN ANALYZE` from the design canvas. Task 2's parser test proves the
/// PostgreSQL JSON for this plan parses to exactly this value, so the UI tests can use it without
/// touching the provider.
#[cfg(test)]
// Deliberately follows `mod tests`, not before it: `src/ui.rs` also needs this fixture, and it
// belongs next to the tests that document its shape.
#[allow(clippy::items_after_test_module)]
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
