//! Conversion of Core Foundation property lists into JSON.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::CFError;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::number::CFNumberIsFloatType;
use serde_json::{Map, Value};

/// Converts a borrowed CF object. Types without a JSON equivalent become their description.
pub fn to_json(r: CFTypeRef) -> Value {
    if r.is_null() {
        return Value::Null;
    }
    let t = unsafe { CFType::wrap_under_get_rule(r) };
    convert(&t)
}

fn convert(t: &CFType) -> Value {
    if let Some(s) = t.downcast::<CFString>() {
        Value::String(s.to_string())
    } else if let Some(b) = t.downcast::<CFBoolean>() {
        Value::Bool(b.into())
    } else if let Some(n) = t.downcast::<CFNumber>() {
        if unsafe { CFNumberIsFloatType(n.as_concrete_TypeRef()) } != 0 {
            n.to_f64().map_or(Value::Null, Value::from)
        } else {
            n.to_i64().map_or(Value::Null, Value::from)
        }
    } else if let Some(a) = t.downcast::<CFArray>() {
        Value::Array(a.iter().map(|v| to_json(*v)).collect())
    } else if let Some(d) = t.downcast::<CFDictionary>() {
        let (keys, values) = d.get_keys_and_values();
        let map: Map<String, Value> = keys
            .into_iter()
            .zip(values)
            .map(|(k, v)| {
                let k = unsafe { CFType::wrap_under_get_rule(k) };
                let key = match k.downcast::<CFString>() {
                    Some(s) => s.to_string(),
                    None => format!("{k:?}"),
                };
                (key, to_json(v))
            })
            .collect();
        Value::Object(map)
    } else if let Some(u) = t.downcast::<CFURL>() {
        Value::String(u.get_string().to_string())
    } else if let Some(e) = t.downcast::<CFError>() {
        serde_json::json!({
            "domain": e.domain().to_string(),
            "code": e.code(),
            "description": e.description().to_string(),
        })
    } else {
        Value::String(format!("{t:?}"))
    }
}
