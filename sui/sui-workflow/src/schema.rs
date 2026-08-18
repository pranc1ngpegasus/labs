use serde_json::{Value, json};

use crate::{WorkflowError, WorkflowMeta};

/// Builds an MCP-style tool descriptor from workflow metadata.
#[must_use]
pub fn tool_descriptor(metadata: &WorkflowMeta) -> Value {
    let input_schema = metadata
        .args_schema
        .clone()
        .unwrap_or_else(|| json!({ "type": "object" }));
    json!({
        "name": metadata.name,
        "description": metadata.description,
        "inputSchema": input_schema,
    })
}

/// Validates workflow `args` against a minimal JSON-Schema subset.
///
/// # Errors
///
/// Returns [`WorkflowError::InvalidConfig`] when the value violates the schema.
pub fn validate_args(
    schema: &Value,
    args: Option<&Value>,
) -> Result<(), WorkflowError> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    let value = args.unwrap_or(&Value::Null);

    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !type_matches(expected, value)
    {
        return Err(WorkflowError::InvalidConfig(format!(
            "args do not match schema: expected type `{expected}`, got `{}`",
            json_type_name(value)
        )));
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let object = value.as_object();
        for entry in required {
            let Some(field) = entry.as_str() else {
                continue;
            };
            if !object.is_some_and(|map| map.contains_key(field)) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "args do not match schema: missing required field `{field}`"
                )));
            }
        }
    }

    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false)
        && let Some(object) = value.as_object()
    {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(field) = object
            .keys()
            .find(|field| !properties.is_some_and(|declared| declared.contains_key(*field)))
        {
            return Err(WorkflowError::InvalidConfig(format!(
                "args do not match schema: unexpected field `{field}`"
            )));
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && let Some(object) = value.as_object()
    {
        for (field, property_schema) in properties {
            let Some(field_value) = object.get(field) else {
                continue;
            };
            validate_property(field, property_schema, field_value)?;
        }
    }

    Ok(())
}

fn validate_property(
    field: &str,
    schema: &Value,
    value: &Value,
) -> Result<(), WorkflowError> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !type_matches(expected, value)
    {
        return Err(WorkflowError::InvalidConfig(format!(
            "args do not match schema: field `{field}` expected type `{expected}`, got `{}`",
            json_type_name(value)
        )));
    }
    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64)
        && let Some(text) = value.as_str()
        && u64::try_from(text.chars().count()).unwrap_or(u64::MAX) < min_length
    {
        return Err(WorkflowError::InvalidConfig(format!(
            "args do not match schema: field `{field}` is shorter than minLength {min_length}"
        )));
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && let Some(number) = value.as_f64()
        && number < minimum
    {
        return Err(WorkflowError::InvalidConfig(format!(
            "args do not match schema: field `{field}` is below minimum {minimum}"
        )));
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
        && let Some(number) = value.as_f64()
        && number > maximum
    {
        return Err(WorkflowError::InvalidConfig(format!(
            "args do not match schema: field `{field}` is above maximum {maximum}"
        )));
    }
    Ok(())
}

fn type_matches(
    expected: &str,
    value: &Value,
) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value
            .as_f64()
            .is_some_and(|number| number.fract() == 0.0 && !number.is_nan()),
        _ => false,
    }
}

const fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
