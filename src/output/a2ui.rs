//! A2UI 1.0 surface store: envelopes in, a component tree plus data model out.
//!
//! Catalog widgets, JSON Pointer bindings, and template lists are resolved
//! here so the view only walks a finished tree.

use std::collections::HashMap;

use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct Surface {
    pub id: String,
    components: HashMap<String, Map<String, Value>>,
    data: Value,
}

impl Surface {
    fn new(id: String, data: Value) -> Self {
        Self {
            id,
            components: HashMap::new(),
            data: if data.is_null() {
                Value::Object(Map::new())
            } else {
                data
            },
        }
    }

    pub fn component(&self, id: &str) -> Option<&Map<String, Value>> {
        self.components.get(id)
    }

    pub fn data(&self) -> &Value {
        &self.data
    }

    fn upsert_components(&mut self, components: &[Value]) {
        for component in components {
            let Some(object) = component.as_object() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            self.components.insert(id.to_string(), object.clone());
        }
    }
}

#[derive(Debug, Default)]
pub struct Store {
    order: Vec<String>,
    surfaces: HashMap<String, Surface>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_all(&mut self, messages: &[Value]) {
        for message in messages {
            self.apply(message);
        }
    }

    pub fn surfaces(&self) -> impl Iterator<Item = &Surface> {
        self.order.iter().filter_map(|id| self.surfaces.get(id))
    }

    fn apply(&mut self, message: &Value) {
        let Some(object) = message.as_object() else {
            return;
        };
        if let Some(create) = object.get("createSurface").and_then(Value::as_object) {
            self.create_surface(create);
        } else if let Some(update) = object.get("updateComponents").and_then(Value::as_object) {
            self.update_components(update);
        } else if let Some(update) = object.get("updateDataModel").and_then(Value::as_object) {
            self.update_data_model(update);
        } else if let Some(delete) = object.get("deleteSurface").and_then(Value::as_object) {
            if let Some(id) = delete.get("surfaceId").and_then(Value::as_str) {
                self.surfaces.remove(id);
                self.order.retain(|existing| existing != id);
            }
        }
    }

    fn create_surface(&mut self, create: &Map<String, Value>) {
        let Some(id) = create.get("surfaceId").and_then(Value::as_str) else {
            return;
        };
        if self.surfaces.contains_key(id) {
            self.surfaces.remove(id);
            self.order.retain(|existing| existing != id);
        }
        let data = create
            .get("dataModel")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        let mut surface = Surface::new(id.to_string(), data);
        if let Some(Value::Array(components)) = create.get("components") {
            surface.upsert_components(components);
        }
        self.order.push(id.to_string());
        self.surfaces.insert(id.to_string(), surface);
    }

    fn ensure_surface(&mut self, id: &str) -> &mut Surface {
        if !self.surfaces.contains_key(id) {
            self.order.push(id.to_string());
            self.surfaces.insert(
                id.to_string(),
                Surface::new(id.to_string(), Value::Object(Map::new())),
            );
        }
        self.surfaces.get_mut(id).unwrap()
    }

    fn update_components(&mut self, update: &Map<String, Value>) {
        let Some(id) = update.get("surfaceId").and_then(Value::as_str) else {
            return;
        };
        let id = id.to_string();
        let Some(Value::Array(components)) = update.get("components") else {
            return;
        };
        self.ensure_surface(&id).upsert_components(components);
    }

    fn update_data_model(&mut self, update: &Map<String, Value>) {
        let Some(id) = update.get("surfaceId").and_then(Value::as_str) else {
            return;
        };
        let id = id.to_string();
        let path = update.get("path").and_then(Value::as_str).unwrap_or("/");
        let value = update.get("value").cloned().unwrap_or(Value::Null);
        set_pointer(&mut self.ensure_surface(&id).data, path, value);
    }
}

#[derive(Clone, Copy)]
pub(super) struct Scope<'a> {
    root: &'a Value,
    item: Option<&'a Value>,
}

impl<'a> Scope<'a> {
    pub(super) fn root(root: &'a Value) -> Self {
        Self { root, item: None }
    }

    pub(super) fn with_item(self, item: &'a Value) -> Self {
        Self {
            root: self.root,
            item: Some(item),
        }
    }
}

pub(super) fn resolve_string(value: &Value, scope: Scope<'_>) -> String {
    stringify_value(&resolve_dynamic(value, scope))
}

pub(super) fn resolve_bool(value: &Value, scope: Scope<'_>) -> bool {
    match resolve_dynamic(value, scope) {
        Value::Bool(flag) => flag,
        Value::String(text) => text == "true" || text == "1",
        Value::Number(number) => number.as_f64().unwrap_or(0.0) != 0.0,
        _ => false,
    }
}

pub(super) fn resolve_strings(value: &Value, scope: Scope<'_>) -> Vec<String> {
    match resolve_dynamic(value, scope) {
        Value::Array(items) => items.iter().map(stringify_value).collect(),
        other => {
            let text = stringify_value(&other);
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            }
        }
    }
}

pub(super) fn resolve_dynamic(value: &Value, scope: Scope<'_>) -> Value {
    match value {
        Value::String(text) => Value::String(text.clone()),
        Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
        Value::Object(object) => {
            if let Some(path) = object.get("path").and_then(Value::as_str) {
                return pointer(scope, path).cloned().unwrap_or(Value::Null);
            }
            if let Some(literal) = object.get("literal") {
                return literal.clone();
            }
            if let Some(call) = object.get("call").and_then(Value::as_str) {
                return invoke(call, object.get("args"), scope);
            }
            value.clone()
        }
        Value::Array(_) => value.clone(),
    }
}

fn invoke(name: &str, args: Option<&Value>, scope: Scope<'_>) -> Value {
    let args = args.and_then(Value::as_object);
    match name {
        "formatString" => {
            let template = args
                .and_then(|map| map.get("value"))
                .map(|value| resolve_string(value, scope))
                .unwrap_or_default();
            Value::String(interpolate(&template, scope))
        }
        "formatNumber" | "formatCurrency" | "formatDate" | "pluralize" => args
            .and_then(|map| map.get("value"))
            .map(|value| resolve_dynamic(value, scope))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn interpolate(template: &str, scope: Scope<'_>) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else {
            out.push_str("${");
            out.push_str(rest);
            return out;
        };
        let expr = rest[..end].trim();
        let value = pointer(scope, expr).cloned().unwrap_or(Value::Null);
        out.push_str(&stringify_value(&value));
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn pointer<'a>(scope: Scope<'a>, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "/" {
        return Some(scope.root);
    }
    if let Some(stripped) = path.strip_prefix('/') {
        return walk(scope.root, stripped);
    }
    walk(scope.item.unwrap_or(scope.root), path)
}

fn walk<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = root;
    if pointer.is_empty() {
        return Some(current);
    }
    for raw in pointer.split('/') {
        let key = raw.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&key)?,
            Value::Array(items) => {
                let index: usize = key.parse().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn set_pointer(root: &mut Value, path: &str, value: Value) {
    if path.is_empty() || path == "/" {
        *root = if value.is_null() {
            Value::Object(Map::new())
        } else {
            value
        };
        return;
    }
    let pointer = path.strip_prefix('/').unwrap_or(path);
    let tokens: Vec<String> = pointer
        .split('/')
        .map(|raw| raw.replace("~1", "/").replace("~0", "~"))
        .collect();
    if tokens.is_empty() {
        *root = value;
        return;
    }
    if !root.is_object() && !root.is_array() {
        *root = Value::Object(Map::new());
    }
    assign(root, &tokens, value);
}

fn assign(node: &mut Value, tokens: &[String], value: Value) {
    let Some((head, rest)) = tokens.split_first() else {
        return;
    };
    if rest.is_empty() {
        if value.is_null() {
            match node {
                Value::Object(map) => {
                    map.remove(head);
                }
                Value::Array(items) => {
                    if let Ok(index) = head.parse::<usize>()
                        && index < items.len()
                    {
                        items.remove(index);
                    }
                }
                _ => {}
            }
            return;
        }
        match node {
            Value::Object(map) => {
                map.insert(head.clone(), value);
            }
            Value::Array(items) => {
                if let Ok(index) = head.parse::<usize>() {
                    if index < items.len() {
                        items[index] = value;
                    } else if index == items.len() {
                        items.push(value);
                    }
                }
            }
            _ => {}
        }
        return;
    }
    let next_is_index = rest[0].parse::<usize>().is_ok();
    match node {
        Value::Object(map) => {
            let entry = map.entry(head.clone()).or_insert_with(|| {
                if next_is_index {
                    Value::Array(Vec::new())
                } else {
                    Value::Object(Map::new())
                }
            });
            assign(entry, rest, value);
        }
        Value::Array(items) => {
            let Ok(index) = head.parse::<usize>() else {
                return;
            };
            if index >= items.len() {
                items.resize(index + 1, Value::Null);
            }
            if items[index].is_null() {
                items[index] = if next_is_index {
                    Value::Array(Vec::new())
                } else {
                    Value::Object(Map::new())
                };
            }
            assign(&mut items[index], rest, value);
        }
        _ => {}
    }
}

pub(super) fn stringify_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn child_ids(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(id)) => vec![id.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn template_binding(value: Option<&Value>) -> Option<(String, String)> {
    let object = value.and_then(Value::as_object)?;
    let path = object.get("path").and_then(Value::as_str)?;
    let component_id = object
        .get("componentId")
        .and_then(Value::as_str)
        .or_else(|| object.get("component").and_then(Value::as_str))?;
    Some((path.to_string(), component_id.to_string()))
}

pub(super) fn array_at<'a>(scope: Scope<'a>, path: &str) -> &'a [Value] {
    pointer(scope, path)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_and_bind_text() {
        let mut store = Store::new();
        store.apply(&json!({
            "version": "v1.0",
            "createSurface": {
                "surfaceId": "card",
                "components": [
                    {"id": "root", "component": "Text", "text": {"path": "/name"}}
                ],
                "dataModel": {"name": "Ada"}
            }
        }));
        let surface = store.surfaces().next().unwrap();
        let text = surface.component("root").unwrap().get("text").unwrap();
        assert_eq!(resolve_string(text, Scope::root(surface.data())), "Ada");
    }

    #[test]
    fn pointer_update_replaces_nested_field() {
        let mut data = json!({"user": {"name": "Ada"}});
        set_pointer(&mut data, "/user/name", json!("Grace"));
        assert_eq!(data["user"]["name"], "Grace");
    }

    #[test]
    fn format_string_interpolates_paths() {
        let data = json!({"name": "Ada"});
        let value = json!({
            "call": "formatString",
            "args": {"value": "Hello ${/name}"}
        });
        assert_eq!(resolve_string(&value, Scope::root(&data)), "Hello Ada");
    }
}
