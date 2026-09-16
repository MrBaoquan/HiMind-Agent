use serde_json::Value;
use std::error::Error;

use super::WorkflowCondition;

pub(crate) fn evaluate_condition(
    condition: &WorkflowCondition,
    context: &Value,
) -> Result<bool, Box<dyn Error>> {
    match condition.operator.as_str() {
        "all" => {
            for child in &condition.conditions {
                if !evaluate_condition(child, context)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "any" => {
            for child in &condition.conditions {
                if evaluate_condition(child, context)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "not" => Ok(!evaluate_condition(
            condition
                .conditions
                .first()
                .ok_or("workflow not condition is missing its child")?,
            context,
        )?),
        "equals" => Ok(resolve_path(context, &condition.path)? == condition.value),
        "not_equals" => Ok(resolve_path(context, &condition.path)? != condition.value),
        "exists" => Ok(resolve_path(context, &condition.path)
            .map(|value| !value.is_null())
            .unwrap_or(false)),
        "contains" => contains_value(&resolve_path(context, &condition.path)?, &condition.value),
        "gt" | "gte" | "lt" | "lte" => {
            let left = resolve_path(context, &condition.path)?;
            compare_numbers(condition.operator.as_str(), &left, &condition.value)
        }
        value => Err(format!("unsupported workflow condition operator: {value}").into()),
    }
}

fn resolve_path(context: &Value, path: &str) -> Result<Value, Box<dyn Error>> {
    let mut current = context;
    for segment in path.split('.').filter(|segment| !segment.trim().is_empty()) {
        if let Ok(index) = segment.parse::<usize>() {
            current = current
                .get(index)
                .ok_or_else(|| format!("workflow condition path is missing array index: {path}"))?;
        } else {
            current = current
                .get(segment)
                .ok_or_else(|| format!("workflow condition path is missing property: {path}"))?;
        }
    }
    Ok(current.clone())
}

fn contains_value(container: &Value, target: &Value) -> Result<bool, Box<dyn Error>> {
    match container {
        Value::String(value) => Ok(value.contains(target.as_str().unwrap_or_default())),
        Value::Array(values) => Ok(values.contains(target)),
        Value::Object(values) => Ok(target.as_str().is_some_and(|key| values.contains_key(key))),
        _ => Ok(false),
    }
}

fn compare_numbers(operator: &str, left: &Value, right: &Value) -> Result<bool, Box<dyn Error>> {
    let left = left
        .as_f64()
        .ok_or("workflow comparison left value is not numeric")?;
    let right = right
        .as_f64()
        .ok_or("workflow comparison right value is not numeric")?;
    Ok(match operator {
        "gt" => left > right,
        "gte" => left >= right,
        "lt" => left < right,
        "lte" => left <= right,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn condition(operator: &str, path: &str, value: Value) -> WorkflowCondition {
        WorkflowCondition {
            operator: operator.to_string(),
            path: path.to_string(),
            value,
            conditions: Vec::new(),
        }
    }

    #[test]
    fn evaluates_structured_conditions_without_scripts() {
        let context = json!({
            "input": {"mode": "develop"},
            "steps": {"TEST": {"status": "passed"}},
            "loops": {"DEV": {"iteration": 2}}
        });
        let all = WorkflowCondition {
            operator: "all".to_string(),
            path: String::new(),
            value: Value::Null,
            conditions: vec![
                condition("equals", "input.mode", json!("develop")),
                condition("equals", "steps.TEST.status", json!("passed")),
                condition("gte", "loops.DEV.iteration", json!(2)),
            ],
        };
        assert!(evaluate_condition(&all, &context).unwrap());
    }

    #[test]
    fn missing_path_fails_for_leaf_conditions() {
        let context = json!({"input": {}});
        let error = evaluate_condition(
            &condition("equals", "input.mode", json!("develop")),
            &context,
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing property"));
    }
}
