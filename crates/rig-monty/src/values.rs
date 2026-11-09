//! The deliberately narrow JSON boundary between Rig and Python.
//!
//! Monty's serde format represents its value graph, not JSON. Borrow values
//! through ObjectRef and reject unsupported types instead of losing information.

use std::collections::BTreeMap;

use monty_types::{MontyObject, NamedValues, ObjectRef};
use serde_json::{Map, Number, Value};

const MAX_DEPTH: usize = 32;

pub(crate) fn inputs(
    values: &BTreeMap<String, Value>,
    max_bytes: usize,
) -> Result<NamedValues, String> {
    let mut budget = Budget::new(max_bytes);
    budget.take(2)?; // enclosing JSON object
    let mut inputs = NamedValues::new();
    for (index, (name, value)) in values.iter().enumerate() {
        if name == "tool" {
            return Err("the input name 'tool' is reserved for Rig's host bridge".to_owned());
        }
        budget.take(usize::from(index != 0) + 1)?; // comma and colon
        budget.string(name)?;
        inputs.push(name, import(value, 1, &mut budget)?);
    }
    inputs.push("tool", MontyObject::function("tool", None));
    Ok(inputs)
}

pub(crate) fn from_json(value: &Value, max_bytes: usize) -> Result<MontyObject, String> {
    import(value, 0, &mut Budget::new(max_bytes))
}

pub(crate) fn to_json(value: ObjectRef<'_>, max_bytes: usize) -> Result<Value, String> {
    export(value, 0, &mut Budget::new(max_bytes))
}

fn import(value: &Value, depth: usize, budget: &mut Budget) -> Result<MontyObject, String> {
    check_depth(depth)?;
    Ok(match value {
        Value::Null => {
            budget.take(4)?;
            MontyObject::none()
        }
        Value::Bool(value) => {
            budget.take(if *value { 4 } else { 5 })?;
            MontyObject::bool(*value)
        }
        Value::Number(value) => {
            budget.take(value.to_string().len())?;
            if let Some(integer) = value.as_i64() {
                MontyObject::int(integer)
            } else if value.is_u64() {
                return Err("JSON integers must fit in a signed 64-bit value".to_owned());
            } else {
                let float = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or("JSON numbers must be finite")?;
                MontyObject::float(float)
            }
        }
        Value::String(value) => {
            budget.string(value)?;
            MontyObject::string(value.as_str())
        }
        Value::Array(values) => {
            budget.container(values.len())?;
            let items = values
                .iter()
                .map(|value| import(value, depth + 1, budget))
                .collect::<Result<Vec<_>, _>>()?;
            MontyObject::list(items)
        }
        Value::Object(values) => {
            budget.container(values.len())?;
            let mut pairs = Vec::new();
            for (key, value) in values {
                budget.string(key)?;
                budget.take(1)?;
                pairs.push((
                    MontyObject::string(key.as_str()),
                    import(value, depth + 1, budget)?,
                ));
            }
            MontyObject::dict(pairs)
        }
    })
}

fn export(value: ObjectRef<'_>, depth: usize, budget: &mut Budget) -> Result<Value, String> {
    check_depth(depth)?;
    Ok(match value.type_name() {
        "NoneType" => {
            budget.take(4)?;
            Value::Null
        }
        "bool" => {
            let value = value.as_bool().ok_or("invalid Python bool")?;
            budget.take(if value { 4 } else { 5 })?;
            Value::Bool(value)
        }
        "int" => {
            let value = value
                .as_int()
                .ok_or("Python integers must fit in a signed 64-bit JSON value")?;
            let number = Number::from(value);
            budget.take(number.to_string().len())?;
            Value::Number(number)
        }
        "float" => {
            let number = value
                .as_float()
                .and_then(Number::from_f64)
                .ok_or("Python float is not a finite JSON number")?;
            budget.take(number.to_string().len())?;
            Value::Number(number)
        }
        "str" => {
            let value = value.as_str().ok_or("invalid Python str")?;
            budget.string(value)?;
            Value::String(value.to_owned())
        }
        "list" => {
            let items = value.items().ok_or("invalid Python list")?;
            budget.container(items.len())?;
            Value::Array(
                items
                    .into_iter()
                    .map(|value| export(value, depth + 1, budget))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        "dict" => {
            let pairs = value.pairs().ok_or("invalid Python dict")?;
            budget.container(pairs.len())?;
            let mut object = Map::new();
            for (key, value) in pairs {
                let key = key
                    .as_str()
                    .ok_or("Python dictionary keys must be JSON strings")?;
                budget.string(key)?;
                budget.take(1)?;
                let value = export(value, depth + 1, budget)?;
                if object.insert(key.to_owned(), value).is_some() {
                    return Err("Python dictionary contains duplicate JSON keys".to_owned());
                }
            }
            Value::Object(object)
        }
        kind => return Err(format!("Python {kind} cannot cross the JSON boundary")),
    })
}

fn check_depth(depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("JSON value is cyclic or exceeds 32 levels of nesting".to_owned());
    }
    Ok(())
}

/// Counts the actual compact JSON encoding before copying strings or children.
struct Budget {
    remaining: usize,
}

impl Budget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn take(&mut self, bytes: usize) -> Result<(), String> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or("JSON value exceeds its host-owned byte limit")?;
        Ok(())
    }

    fn container(&mut self, children: usize) -> Result<(), String> {
        self.take(2)?;
        self.take(children.saturating_sub(1))
    }

    fn string(&mut self, text: &str) -> Result<(), String> {
        self.take(2)?;
        for character in text.chars() {
            self.take(match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
                '\u{0000}'..='\u{001f}' => 6,
                character => character.len_utf8(),
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_round_trip_preserves_types_and_refuses_loss_or_oversized_values() {
        let value = json!({"text":"é\n\u{0001}", "items":[null, true, i64::MIN, 1.25]});
        let bytes = serde_json::to_vec(&value).unwrap().len();
        let python = from_json(&value, bytes).unwrap();
        assert_eq!(to_json(python.as_ref(), bytes).unwrap(), value);
        assert!(from_json(&value, bytes - 1).is_err());
        assert!(to_json(python.as_ref(), bytes - 1).is_err());
        assert!(from_json(&json!(u64::MAX), 1024).is_err());
        assert!(to_json(MontyObject::float(f64::NAN).as_ref(), 1024).is_err());
        assert!(to_json(MontyObject::tuple([MontyObject::int(1)]).as_ref(), 1024).is_err());
        let deep = (0..34).fold(Value::Null, |value, _| Value::Array(vec![value]));
        assert!(from_json(&deep, 1024).is_err());
    }
}
