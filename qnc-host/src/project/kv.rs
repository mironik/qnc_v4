use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

/// Flat key-value rows in SQLite (replaces JSON blob columns for settings/config).
pub fn replace_object(
    conn: &Connection,
    table: &str,
    entity_col: &str,
    entity_id: &str,
    value: &Value,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!("DELETE FROM {table} WHERE {entity_col} = ?1"),
        params![entity_id],
    )?;
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    if obj.is_empty() {
        return Ok(());
    }
    for (k, v) in flatten_object("", obj) {
        conn.execute(
            &format!(
                "INSERT INTO {table} ({entity_col}, setting_key, setting_value) VALUES (?1, ?2, ?3)"
            ),
            params![entity_id, k, v],
        )?;
    }
    Ok(())
}

pub fn load_object(
    conn: &Connection,
    table: &str,
    entity_col: &str,
    entity_id: &str,
) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(&format!(
        "SELECT setting_key, setting_value FROM {table} WHERE {entity_col} = ?1 ORDER BY setting_key"
    ))?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![entity_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(unflatten_rows(&rows))
}

pub fn kv_count(
    conn: &Connection,
    table: &str,
    entity_col: &str,
    entity_id: &str,
) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(1) FROM {table} WHERE {entity_col} = ?1"),
        params![entity_id],
        |r| r.get(0),
    )
}

pub fn migrate_json_column_to_kv(
    conn: &Connection,
    source_table: &str,
    entity_col: &str,
    json_col: &str,
    kv_table: &str,
) -> rusqlite::Result<()> {
    let sql = format!("SELECT {entity_col}, {json_col} FROM {source_table} WHERE {json_col} != '{{}}' AND {json_col} != '[]'");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (entity_id, raw) in rows {
        if kv_count(conn, kv_table, entity_col, &entity_id)? > 0 {
            continue;
        }
        let parsed = serde_json::from_str::<Value>(&raw).unwrap_or(json!({}));
        replace_object(conn, kv_table, entity_col, &entity_id, &parsed)?;
        conn.execute(
            &format!("UPDATE {source_table} SET {json_col} = '{{}}' WHERE {entity_col} = ?1"),
            params![entity_id],
        )?;
    }
    Ok(())
}

pub fn replace_string_list(
    conn: &Connection,
    table: &str,
    entity_col: &str,
    entity_id: &str,
    list_col: &str,
    values: &[String],
) -> rusqlite::Result<()> {
    conn.execute(
        &format!("DELETE FROM {table} WHERE {entity_col} = ?1"),
        params![entity_id],
    )?;
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        conn.execute(
            &format!("INSERT INTO {table} ({entity_col}, {list_col}) VALUES (?1, ?2)"),
            params![entity_id, trimmed],
        )?;
    }
    Ok(())
}

pub fn load_string_list(
    conn: &Connection,
    table: &str,
    entity_col: &str,
    entity_id: &str,
    list_col: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {list_col} FROM {table} WHERE {entity_col} = ?1 ORDER BY {list_col}"
    ))?;
    let rows: Vec<String> = stmt
        .query_map(params![entity_id], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

fn flatten_object(prefix: &str, obj: &Map<String, Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in obj {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Object(nested) => out.extend(flatten_object(&key, nested)),
            Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    let item_key = format!("{key}[{i}]");
                    match item {
                        Value::Object(nested) => out.extend(flatten_object(&item_key, nested)),
                        _ => out.push((item_key, scalar_to_string(item))),
                    }
                }
            }
            Value::Null => {}
            _ => out.push((key, scalar_to_string(v))),
        }
    }
    out
}

fn scalar_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn unflatten_rows(rows: &[(String, String)]) -> Value {
    let mut root = Map::new();
    for (key, raw) in rows {
        insert_path(&mut root, key, parse_scalar(raw));
    }
    Value::Object(root)
}

fn parse_scalar(raw: &str) -> Value {
    if raw == "true" {
        return json!(true);
    }
    if raw == "false" {
        return json!(false);
    }
    if let Ok(n) = raw.parse::<i64>() {
        return json!(n);
    }
    if let Ok(n) = raw.parse::<f64>() {
        return json!(n);
    }
    json!(raw)
}

fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    insert_parts(root, &parts, value);
}

fn insert_parts(node: &mut Map<String, Value>, parts: &[&str], value: Value) {
    if parts.is_empty() {
        return;
    }
    let head = parts[0];
    if parts.len() == 1 {
        if let Some((name, idx)) = split_index(head) {
            let arr = node
                .entry(name.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("array slot");
            while arr.len() <= idx {
                arr.push(Value::Null);
            }
            arr[idx] = value;
            return;
        }
        node.insert(head.to_string(), value);
        return;
    }
    if let Some((name, idx)) = split_index(head) {
        let arr = node
            .entry(name.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("array slot");
        while arr.len() <= idx {
            arr.push(Value::Null);
        }
        if arr[idx].is_null() {
            arr[idx] = json!({});
        }
        let obj = arr[idx].as_object_mut().expect("object slot");
        insert_parts(obj, &parts[1..], value);
        return;
    }
    let entry = node.entry(head.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    let obj = entry.as_object_mut().expect("object slot");
    insert_parts(obj, &parts[1..], value);
}

fn split_index(token: &str) -> Option<(&str, usize)> {
    let open = token.find('[')?;
    let close = token.find(']')?;
    if close <= open + 1 {
        return None;
    }
    let name = &token[..open];
    let idx: usize = token[open + 1..close].parse().ok()?;
    Some((name, idx))
}
