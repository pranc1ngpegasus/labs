use rhai::{Array, Dynamic, FLOAT, INT, ImmutableString, Map};
use serde_json::{Map as JsonMap, Number, Value};

use crate::WorkflowError;

pub fn json_to_dynamic(value: &Value) -> Result<Dynamic, WorkflowError> {
    match value {
        Value::Null => Ok(Dynamic::UNIT),
        Value::Bool(value) => Ok(Dynamic::from_bool(*value)),
        Value::Number(value) => number_to_dynamic(value),
        Value::String(value) => Ok(Dynamic::from(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(json_to_dynamic)
            .collect::<Result<Array, _>>()
            .map(Dynamic::from_array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone().into(), json_to_dynamic(value)?)))
            .collect::<Result<Map, WorkflowError>>()
            .map(Dynamic::from_map),
    }
}

fn number_to_dynamic(number: &Number) -> Result<Dynamic, WorkflowError> {
    if let Some(value) = number.as_i64() {
        return Ok(Dynamic::from_int(value));
    }
    if let Some(value) = number.as_u64() {
        let value = INT::try_from(value).map_err(|_| {
            WorkflowError::Value(format!(
                "unsigned integer {number} exceeds Rhai's integer range"
            ))
        })?;
        return Ok(Dynamic::from_int(value));
    }
    number.as_f64().map_or_else(
        || {
            Err(WorkflowError::Value(format!(
                "invalid JSON number {number}"
            )))
        },
        |value| Ok(Dynamic::from_float(value)),
    )
}

pub fn dynamic_to_json(value: &Dynamic) -> Result<Value, WorkflowError> {
    if value.is_unit() {
        return Ok(Value::Null);
    }
    if value.is::<bool>() {
        return Ok(Value::Bool(value.clone_cast::<bool>()));
    }
    if value.is::<INT>() {
        return Ok(Value::Number(Number::from(value.clone_cast::<INT>())));
    }
    if value.is::<FLOAT>() {
        let value = value.clone_cast::<FLOAT>();
        return Number::from_f64(value).map_or_else(
            || {
                Err(WorkflowError::Value(
                    "non-finite floating-point value".into(),
                ))
            },
            |number| Ok(Value::Number(number)),
        );
    }
    if value.is::<ImmutableString>() {
        return Ok(Value::String(
            value.clone_cast::<ImmutableString>().to_string(),
        ));
    }
    if value.is::<Array>() {
        return value
            .clone_cast::<Array>()
            .iter()
            .map(dynamic_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if value.is::<Map>() {
        return value
            .clone_cast::<Map>()
            .iter()
            .map(|(key, value)| Ok((key.to_string(), dynamic_to_json(value)?)))
            .collect::<Result<JsonMap<_, _>, WorkflowError>>()
            .map(Value::Object);
    }

    Err(WorkflowError::Value(format!(
        "Rhai type `{}` is not JSON-compatible",
        value.type_name()
    )))
}
