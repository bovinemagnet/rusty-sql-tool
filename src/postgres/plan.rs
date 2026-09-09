//! Converts `EXPLAIN (FORMAT JSON)` output into the plan model. The only code that knows
//! PostgreSQL's key names (§38: engine specifics stay in the provider).

use serde_json::Value;

use crate::plan::{ActualStatistics, PlanNode, QueryPlan};
use crate::result::QueryError;

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
    // Folded into `operation_name` as the "Parallel " prefix, the way `Strategy`/`Partial Mode`
    // fold in above — not structural noise, but not a bare property either.
    "Parallel Aware",
    // Structural noise the text form does not print either.
    "Parent Relationship",
    "Async Capable",
];

/// PostgreSQL renders a utility statement (`CREATE INDEX`, `VACUUM`, `SET`, `ALTER TABLE`, …) as
/// `ExplainDummyGroup("Utility Statement", …)`, whose JSON is a bare string inside the array
/// rather than a plan object — well-formed, but not something `parse_plan` can read as a plan.
/// The caller uses this to fall back to the original text result instead of treating it as an
/// error (FR3-020: plain Explain of a utility statement is not a regression).
pub fn is_utility_statement(json: &str) -> bool {
    let Ok(document) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    matches!(
        document.as_array().and_then(|array| array.first()),
        Some(Value::String(_))
    )
}

/// `EXPLAIN (FORMAT JSON)` returns a one-element array holding the plan and its timings.
#[allow(clippy::result_large_err)]
pub fn parse_plan(json: &str) -> Result<QueryPlan, QueryError> {
    let document: Value =
        serde_json::from_str(json).map_err(|error| unreadable(&error.to_string()))?;
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

#[allow(clippy::result_large_err)]
fn parse_node(object: &serde_json::Map<String, Value>) -> Result<PlanNode, QueryError> {
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    };
    let number = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| unreadable(&format!("{key} is missing")))
    };
    let node_type = string("Node Type").ok_or_else(|| unreadable("Node Type is missing"))?;
    let operation = operation_name(
        &node_type,
        string("Strategy").as_deref(),
        string("Partial Mode").as_deref(),
        object
            .get("Parallel Aware")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    let actual = match (
        object.get("Actual Startup Time").and_then(Value::as_f64),
        object.get("Actual Total Time").and_then(Value::as_f64),
        object.get("Actual Rows").and_then(Value::as_f64),
        object.get("Actual Loops").and_then(Value::as_f64),
    ) {
        (Some(startup_time_ms), Some(total_time_ms), Some(rows), Some(loops)) => {
            Some(ActualStatistics {
                startup_time_ms,
                total_time_ms,
                rows,
                loops,
            })
        }
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

/// The text form's name for a node: strategy and partial mode fold into the node type for an
/// aggregate, and a parallel-aware node gets the "Parallel " prefix PostgreSQL's text form prints
/// (`Parallel Seq Scan on orders`) — otherwise the hottest node in a parallel plan reads as an
/// ordinary scan.
fn operation_name(
    node_type: &str,
    strategy: Option<&str>,
    partial_mode: Option<&str>,
    parallel_aware: bool,
) -> String {
    let base = match (node_type, strategy) {
        ("Aggregate", Some("Hashed")) => "HashAggregate".to_owned(),
        ("Aggregate", Some("Sorted")) => "GroupAggregate".to_owned(),
        ("Aggregate", Some("Mixed")) => "MixedAggregate".to_owned(),
        _ => node_type.to_owned(),
    };
    let base = match partial_mode {
        Some(mode) if mode != "Simple" => format!("{mode} {base}"),
        _ => base,
    };
    if parallel_aware {
        format!("Parallel {base}")
    } else {
        base
    }
}

/// "using customer_pkey on customer c" — the clause the text form appends to a scan. The `using`
/// wording only applies when a relation is also present (`Index Scan`); a `Bitmap Index Scan`
/// carries an `Index Name` with no `Relation Name`, and PostgreSQL prints that bare, as
/// "Bitmap Index Scan on tenk1_unique1", not "... using tenk1_unique1".
fn target(index: Option<&str>, relation: Option<&str>, alias: Option<&str>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(index) = index {
        if relation.is_some() {
            parts.push(format!("using {index}"));
        } else {
            parts.push(index.to_owned());
        }
    }
    match (relation, alias) {
        (Some(relation), Some(alias)) if alias != relation => {
            parts.push(format!("{relation} {alias}"))
        }
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

#[cfg(test)]
pub(crate) const SAMPLE_PLAN_JSON: &str = r#"[{"Plan":{"Node Type":"Limit","Parallel Aware":false,"Async Capable":false,"Startup Cost":4821.14,"Total Cost":4821.16,"Plan Rows":10,"Plan Width":48,"Actual Startup Time":38.205,"Actual Total Time":38.210,"Actual Rows":10,"Actual Loops":1,"Plans":[{"Node Type":"Sort","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Startup Cost":4821.14,"Total Cost":4833.51,"Plan Rows":4948,"Plan Width":48,"Actual Startup Time":38.203,"Actual Total Time":38.205,"Actual Rows":10,"Actual Loops":1,"Sort Key":["(sum(o.total)) DESC"],"Sort Method":"top-N heapsort","Sort Space Used":26,"Sort Space Type":"Memory","Plans":[{"Node Type":"Aggregate","Strategy":"Hashed","Partial Mode":"Simple","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Startup Cost":4680.30,"Total Cost":4714.21,"Plan Rows":4948,"Plan Width":48,"Actual Startup Time":35.9,"Actual Total Time":36.9,"Actual Rows":4948,"Actual Loops":1,"Group Key":["c.name"],"Batches":1,"Peak Memory Usage":721,"Plans":[{"Node Type":"Hash Join","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Join Type":"Inner","Startup Cost":214.00,"Total Cost":4522.11,"Plan Rows":21092,"Plan Width":24,"Actual Startup Time":1.7,"Actual Total Time":29.44,"Actual Rows":48317,"Actual Loops":1,"Inner Unique":true,"Hash Cond":"(o.customer_id = c.id)","Plans":[{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Relation Name":"orders","Alias":"o","Startup Cost":0.00,"Total Cost":4102.00,"Plan Rows":21092,"Plan Width":24,"Actual Startup Time":0.031,"Actual Total Time":18.72,"Actual Rows":48317,"Actual Loops":1,"Filter":"(placed_at >= (now() - '30 days'::interval))","Rows Removed by Filter":151683},{"Node Type":"Hash","Parent Relationship":"Inner","Parallel Aware":false,"Async Capable":false,"Startup Cost":152.00,"Total Cost":152.00,"Plan Rows":4960,"Plan Width":20,"Actual Startup Time":1.61,"Actual Total Time":1.61,"Actual Rows":4960,"Actual Loops":1,"Hash Buckets":8192,"Original Hash Buckets":8192,"Hash Batches":1,"Original Hash Batches":1,"Peak Memory Usage":297,"Plans":[{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Async Capable":false,"Relation Name":"customer","Alias":"c","Startup Cost":0.00,"Total Cost":152.00,"Plan Rows":4960,"Plan Width":20,"Actual Startup Time":0.012,"Actual Total Time":0.84,"Actual Rows":4960,"Actual Loops":1}]}]}]}]}]},"Planning Time":0.412,"Execution Time":38.402}]"#;

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
        // PostgreSQL only emits "Planning Time" under summary (`es->summary = summary_set ?
        // summary_set : es->analyze`), which a plain, non-ANALYZE EXPLAIN never sets, so this
        // fixture carries no "Planning Time" key — a real server never sends one here.
        let json = r#"[{"Plan":{"Node Type":"Result","Startup Cost":0.00,"Total Cost":0.01,"Plan Rows":1,"Plan Width":4}}]"#;
        let plan = parse_plan(json).unwrap();
        assert!(!plan.analysed);
        assert_eq!(plan.root.operation, "Result");
        assert!(plan.root.actual.is_none());
        assert_eq!(plan.execution_time_ms, None);
        assert_eq!(plan.planning_time_ms, None);
    }

    #[test]
    fn scan_targets_follow_the_text_forms_wording() {
        let json = r#"[{"Plan":{"Node Type":"Index Scan","Index Name":"customer_pkey","Relation Name":"customer","Alias":"customer","Startup Cost":0.15,"Total Cost":8.17,"Plan Rows":1,"Plan Width":36}}]"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(
            plan.root.heading(),
            "Index Scan using customer_pkey on customer"
        );

        let json = r#"[{"Plan":{"Node Type":"Subquery Scan","Alias":"sub","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4}}]"#;
        assert_eq!(
            parse_plan(json).unwrap().root.heading(),
            "Subquery Scan on sub"
        );

        // A Bitmap Index Scan carries an Index Name but no Relation Name; PostgreSQL prints it
        // bare ("on tenk1_unique1"), not with the "using" wording an Index Scan gets.
        let json = r#"[{"Plan":{"Node Type":"Bitmap Index Scan","Index Name":"tenk1_unique1","Startup Cost":0.0,"Total Cost":4.27,"Plan Rows":1,"Plan Width":0}}]"#;
        assert_eq!(
            parse_plan(json).unwrap().root.heading(),
            "Bitmap Index Scan on tenk1_unique1"
        );
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
            assert!(
                plan.root.properties.is_empty(),
                "strategy and mode are consumed"
            );
        }
    }

    /// `Parallel Aware` is folded into the operation name as the "Parallel " prefix, matching
    /// PostgreSQL's text form (`Parallel Seq Scan on orders`), rather than dropped as structural
    /// noise like `Parent Relationship`/`Async Capable`.
    #[test]
    fn a_parallel_aware_node_gets_the_parallel_prefix() {
        let json = r#"[{"Plan":{"Node Type":"Seq Scan","Parallel Aware":true,"Relation Name":"orders","Alias":"orders","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4}}]"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(plan.root.operation, "Parallel Seq Scan");
        assert!(
            plan.root.properties.is_empty(),
            "Parallel Aware is consumed, not left as a bare property"
        );

        let json = r#"[{"Plan":{"Node Type":"Seq Scan","Parallel Aware":false,"Relation Name":"orders","Alias":"orders","Startup Cost":0.0,"Total Cost":1.0,"Plan Rows":1,"Plan Width":4}}]"#;
        assert_eq!(parse_plan(json).unwrap().root.operation, "Seq Scan");
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
    fn a_utility_statement_is_recognised_by_its_bare_string_element() {
        assert!(is_utility_statement(r#"["Utility Statement"]"#));
        assert!(!is_utility_statement(SAMPLE_PLAN_JSON));
        assert!(!is_utility_statement("[]"));
        assert!(!is_utility_statement("not json"));
    }

    #[test]
    fn malformed_output_is_an_error_rather_than_a_panic() {
        for json in [
            "",
            "not json",
            "[]",
            "[{}]",
            r#"[{"Plan":{"Startup Cost":1}}]"#,
        ] {
            let error = parse_plan(json).unwrap_err();
            assert!(
                error
                    .message
                    .starts_with("Could not read the execution plan"),
                "{json:?} gave {}",
                error.message
            );
        }
    }
}
