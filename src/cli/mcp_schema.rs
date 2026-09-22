//! MCP 桥对外吐的工具 schema:先做一层与方言无关的净化,再按上游模型方言整形。
//!
//! 净化(所有方言都过):`enum` 里的空串和类型不符的项剔掉,剔空了连键一起删,
//! `default` 不在 enum 里就剔掉。Miyu 的工具 schema 是给 OpenAI/Anthropic 线写的,
//! 那两家来者不拒,所以脏项一直埋着;Google 侧对空 enum 是硬 400,而且不是废掉
//! 这一个工具,是当轮全部工具一起被毙(`properties[site].enum[4]: cannot be empty`,
//! 09-03 antigravity 中转首跑撞上)。原来只在 gemini 方言里滤,claude-code 与 codex
//! 两条线走无方言路径,空串照旧在它们的出站工具表里(issue #45 / PR #46 发现,
//! 09-18 修)。**递归只走 schema 位置**(`properties` 的各个值、`items`、`anyOf` /
//! `oneOf` / `allOf`):`properties` 底下可以有名字就叫 `enum` / `default` 的属性,
//! 无差别递归会把同名属性静默删掉(PR #46 就踩了这个)。
//!
//! Gemini 方言整形:`type` 不能是数组,`additionalProperties`/`pattern`/`default`
//! 这些键不认。整形只发生在桥上、只在拉起方点名 `MIYU_MCP_SCHEMA_DIALECT=gemini`
//! 时——工具自己的 schema 一个字不改,别的供应商照旧。

use serde_json::{json, Map, Value};

/// Gemini 认识的 schema 键;其余一律剔除。
const GEMINI_KEYS: &[&str] = &[
    "type",
    "description",
    "enum",
    "items",
    "properties",
    "required",
    "nullable",
    "format",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
];

pub(in crate::cli) fn shape_for_dialect(schema: Value, dialect: &str) -> Value {
    let schema = sanitize(schema);
    match dialect {
        "gemini" => gemini_compatible(schema),
        _ => schema,
    }
}

/// 与方言无关的净化。只在 schema 位置递归:这一层的 `enum`/`default` 是关键字,
/// `properties` 下面的同名键是**属性名**,不碰。
pub(in crate::cli) fn sanitize(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return schema;
    };
    let declared_type = map.get("type").cloned();
    if let Some(Value::Array(values)) = map.remove("enum") {
        let kept: Vec<Value> = values
            .into_iter()
            .filter(|value| enum_value_fits(value, declared_type.as_ref()))
            .collect();
        if !kept.is_empty() {
            map.insert("enum".into(), Value::Array(kept));
        }
    }
    if let (Some(default), Some(Value::Array(allowed))) = (map.get("default"), map.get("enum")) {
        if !allowed.contains(default) {
            map.remove("default");
        }
    }
    if let Some(Value::Object(props)) = map.remove("properties") {
        let cleaned: Map<String, Value> = props
            .into_iter()
            .map(|(name, sub)| (name, sanitize(sub)))
            .collect();
        map.insert("properties".into(), Value::Object(cleaned));
    }
    if let Some(items) = map.remove("items") {
        map.insert("items".into(), sanitize(items));
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = map.remove(key) {
            map.insert(
                key.into(),
                Value::Array(variants.into_iter().map(sanitize).collect()),
            );
        }
    }
    Value::Object(map)
}

/// enum 项要非空、且与声明的类型对得上(没声明类型就只挡空串)。
fn enum_value_fits(value: &Value, declared_type: Option<&Value>) -> bool {
    if value.as_str().is_some_and(str::is_empty) || value.is_null() {
        return false;
    }
    let types: Vec<&str> = match declared_type {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => return true,
    };
    if types.is_empty() {
        return true;
    }
    types.iter().any(|kind| match *kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    })
}

fn gemini_compatible(schema: Value) -> Value {
    let Value::Object(map) = schema else {
        return schema;
    };
    let mut out = Map::new();
    let mut nullable = map
        .get("nullable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for (key, value) in map {
        if !GEMINI_KEYS.contains(&key.as_str()) {
            continue;
        }
        match key.as_str() {
            "type" => {
                // 联合类型:有 array 就取 array(保住 items,数组形状仍可达),否则
                // 取第一个非 null 的;null 折成 nullable。
                let picked = match &value {
                    Value::Array(types) => {
                        if types.iter().any(|t| t.as_str() == Some("null")) {
                            nullable = true;
                        }
                        types
                            .iter()
                            .find(|t| t.as_str() == Some("array"))
                            .or_else(|| types.iter().find(|t| t.as_str() != Some("null")))
                            .cloned()
                            .unwrap_or(Value::String("string".into()))
                    }
                    other => other.clone(),
                };
                out.insert(key, picked);
            }
            "enum" => {
                if let Value::Array(values) = value {
                    let kept: Vec<Value> = values
                        .into_iter()
                        .filter(|v| v.as_str().is_none_or(|s| !s.is_empty()))
                        .collect();
                    if !kept.is_empty() {
                        out.insert(key, Value::Array(kept));
                    }
                }
            }
            "properties" => {
                if let Value::Object(props) = value {
                    let shaped: Map<String, Value> = props
                        .into_iter()
                        .map(|(name, sub)| (name, gemini_compatible(sub)))
                        .collect();
                    // 空 properties 的 OBJECT 会被拒("should be non-empty");
                    // 干脆不带这个键,让上游按无参处理。
                    if !shaped.is_empty() {
                        out.insert(key, Value::Object(shaped));
                    }
                }
            }
            "items" => {
                out.insert(key, gemini_compatible(value));
            }
            "required" => {
                if let Value::Array(names) = &value {
                    if !names.is_empty() {
                        out.insert(key, value);
                    }
                }
            }
            "nullable" => {}
            _ => {
                out.insert(key, value);
            }
        }
    }
    // items 只属于 array:联合类型折成别的类型后不能留一个孤儿 items。
    if out.get("type").and_then(Value::as_str) != Some("array") {
        out.remove("items");
    }
    // required 里引用的属性要真的存在(属性可能刚被剔掉)。
    if let Some(Value::Array(required)) = out.get("required").cloned() {
        let present = out
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| {
                required
                    .into_iter()
                    .filter(|name| name.as_str().is_some_and(|n| props.contains_key(n)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if present.is_empty() {
            out.remove("required");
        } else {
            out.insert("required".into(), Value::Array(present));
        }
    }
    if nullable {
        out.insert("nullable".into(), json!(true));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_enum_entries_and_unknown_keys_are_dropped() {
        let shaped = shape_for_dialect(
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "site": { "type": "string", "enum": ["a", "", "b"], "default": "a", "pattern": "^a" },
                    "seat": { "type": ["string", "array"], "items": { "type": "string" } },
                    "flag": { "type": ["null", "boolean"] },
                    "orphan": { "type": ["string", "integer"], "items": { "type": "string" } }
                },
                "required": ["site", "gone"]
            }),
            "gemini",
        );
        assert!(shaped.get("additionalProperties").is_none());
        assert_eq!(shaped["properties"]["site"]["enum"], json!(["a", "b"]));
        assert!(shaped["properties"]["site"].get("default").is_none());
        assert!(shaped["properties"]["site"].get("pattern").is_none());
        assert_eq!(shaped["properties"]["seat"]["type"], "array");
        assert!(shaped["properties"]["seat"].get("items").is_some());
        assert_eq!(shaped["properties"]["flag"]["type"], "boolean");
        assert_eq!(shaped["properties"]["flag"]["nullable"], true);
        assert_eq!(shaped["required"], json!(["site"]));
        assert_eq!(shaped["properties"]["orphan"]["type"], "string");
        assert!(shaped["properties"]["orphan"].get("items").is_none());
    }

    #[test]
    fn empty_object_properties_are_omitted_and_other_dialects_untouched() {
        let raw = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        let shaped = shape_for_dialect(raw.clone(), "gemini");
        assert_eq!(shaped, json!({ "type": "object" }));
        assert_eq!(shape_for_dialect(raw.clone(), ""), raw);
    }

    /// 净化对所有方言生效:空串/类型不符的 enum 项剔掉,剔空连键删,default 不在
    /// enum 里剔掉;items 与 anyOf/oneOf/allOf 里的也一样。
    #[test]
    fn sanitizing_drops_bad_enum_items_on_every_dialect() {
        let raw = json!({
            "type": "object",
            "properties": {
                "site": { "type": "string", "enum": ["zh", "cn", "uk", "ja", ""], "default": "cn" },
                "count": { "type": "integer", "enum": [1, "two", 3, null], "default": "two" },
                "gone": { "type": "string", "enum": ["", ""] },
                "list": { "type": "array", "items": { "type": "string", "enum": ["a", ""] } },
                "either": { "anyOf": [ { "type": "string", "enum": ["", "x"] }, { "type": "integer" } ] },
                "loose": { "enum": ["k", "", 2] }
            }
        });
        for dialect in ["", "gemini"] {
            let shaped = shape_for_dialect(raw.clone(), dialect);
            let props = &shaped["properties"];
            assert_eq!(
                props["site"]["enum"],
                json!(["zh", "cn", "uk", "ja"]),
                "{dialect}"
            );
            assert_eq!(props["count"]["enum"], json!([1, 3]));
            assert!(
                props["count"].get("default").is_none(),
                "default 不在 enum 里要剔掉"
            );
            assert!(props["gone"].get("enum").is_none(), "剔空了连键一起删");
            assert_eq!(props["list"]["items"]["enum"], json!(["a"]));
            assert_eq!(
                props["loose"]["enum"],
                json!(["k", 2]),
                "没声明类型只挡空串"
            );
        }
        // 无方言路径除净化外一个字不改:default 还在、anyOf 还在(gemini 方言本来
        // 就不认 anyOf,整个键被剔,那是整形不是净化)。
        let plain = shape_for_dialect(raw.clone(), "");
        assert_eq!(plain["properties"]["site"]["default"], "cn");
        assert_eq!(
            plain["properties"]["either"]["anyOf"][0]["enum"],
            json!(["x"])
        );
    }

    /// 递归只走 schema 位置:`properties` 底下名字就叫 enum / default / items 的
    /// **属性**原样保留(PR #46 的无差别递归会把它们删掉)。
    #[test]
    fn properties_named_like_keywords_survive() {
        let raw = json!({
            "type": "object",
            "properties": {
                "enum": { "type": "string", "description": "a field that happens to be called enum" },
                "default": { "type": "integer" },
                "items": { "type": "array", "items": { "type": "string", "enum": ["", "q"] } },
                "nested": {
                    "type": "object",
                    "properties": { "enum": { "type": "string", "enum": ["", "z"] } }
                }
            }
        });
        let shaped = shape_for_dialect(raw, "");
        let props = &shaped["properties"];
        assert_eq!(props["enum"]["type"], "string");
        assert_eq!(props["default"]["type"], "integer");
        assert_eq!(props["items"]["items"]["enum"], json!(["q"]));
        assert_eq!(props["nested"]["properties"]["enum"]["enum"], json!(["z"]));
    }
}
