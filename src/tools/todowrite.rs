use super::{ToolRegistry, ToolSpec};
use crate::paths::MiyuPaths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

pub type TodoList = Arc<Mutex<Vec<Todo>>>;

// 存储绑定(任务#13):按会话落盘 state_dir/todos/<session>.json。
// 旧实现是注册时的单例 Arc<Mutex<Vec>>,daemon 按 config 缓存复用
// registry 后所有会话共享同一份(串味实锤);现在每次调用按当前回合的
// 会话加载/回存,纯函数 todo_write/todo_update 与其测试原样保留。

fn todos_path(paths: &MiyuPaths, session: &str) -> PathBuf {
    paths.state_dir.join("todos").join(format!("{session}.json"))
}

/// 某个会话当前的待办清单。
///
/// WebUI 的常驻面板要在刷新之后还能显示当前状态，而工具事件只在工具跑的
/// 那一刻发生一次。读取收口在这里，调用方不自己拼 `todos/{session}.json`
/// ——路径和损坏容错的规则只该有一份。
pub(crate) fn session_todos(paths: &MiyuPaths, session: &str) -> Vec<Todo> {
    load_todos(paths, session)
}

/// 清掉某个会话的待办。
///
/// 待办按会话存在库外面（`todos/{session}.json`），所以「重置对话」那条路上
/// 一串清理动作全走 `StateStore`，唯独漏了它——对话重来了，上一轮的待办还挂
/// 在侧边面板上，模型下一次读 todo 也还是旧的。
pub(crate) fn clear_session_todos(paths: &MiyuPaths, session: &str) -> Result<()> {
    match std::fs::remove_file(todos_path(paths, session)) {
        Ok(()) => Ok(()),
        // 没建过清单是常态，不是错误。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn load_todos(paths: &MiyuPaths, session: &str) -> Vec<Todo> {
    let Ok(raw) = std::fs::read_to_string(todos_path(paths, session)) else {
        return Vec::new();
    };
    match serde_json::from_str(&raw) {
        Ok(todos) => todos,
        Err(error) => {
            // 容错语义保留(损坏即重新开始),但必须留痕:任务清单无声消失
            // 会让模型以为自己从没建过清单。
            tracing::warn!(
                session,
                error = %error,
                "todo list file is corrupt; starting with an empty list"
            );
            Vec::new()
        }
    }
}

fn save_todos(paths: &MiyuPaths, session: &str, todos: &[Todo]) -> Result<()> {
    let path = todos_path(paths, session);
    let Some(parent) = path.parent() else {
        anyhow::bail!("todo path has no parent directory");
    };
    std::fs::create_dir_all(parent)?;
    // 原子写:崩溃在写回中途不能把清单留成截断的半个 JSON。
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(temp.path(), serde_json::to_string_pretty(todos)?)?;
    temp.persist(&path)?;
    Ok(())
}

fn session_for_call() -> Result<String> {
    super::workspace::try_session()
        .map(|session| session.to_string())
        .ok_or_else(|| anyhow::anyhow!("todo tools require a session turn"))
}

fn run_scoped(
    paths: &MiyuPaths,
    args: Value,
    apply: fn(Value, TodoList) -> Result<String>,
) -> Result<String> {
    let session = session_for_call()?;
    let todos: TodoList = Arc::new(Mutex::new(load_todos(paths, &session)));
    let output = apply(args, Arc::clone(&todos))?;
    let list = todos.lock().expect("todo state lock").clone();
    save_todos(paths, &session, &list)?;
    Ok(output)
}

/// todowrite + todoupdate 合并(08-17):同一份清单的整表替换与增量修改。
/// 给了 updates 就走增量,给了 todos 就整表替换。
pub fn register(registry: &mut ToolRegistry, paths: MiyuPaths) {
    registry.register(ToolSpec::new(
        "todowrite",
        "Maintain the structured task list for the current session. Pass todos to create or replace the whole list; pass updates to apply small atomic changes (add, update, remove, clear) without resending everything. Exactly one of the two.",
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The full todo list. This replaces the entire list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Brief description of the task." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Current status of the task."
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Priority level of the task."
                            }
                        },
                        "required": ["content", "status", "priority"]
                    }
                },
                "updates": {
                    "type": "array",
                    "description": "Sequential todo mutations to apply atomically.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["add", "update", "remove", "clear"],
                                "description": "Mutation type."
                            },
                            "index": {
                                "type": "integer",
                                "description": "1-based target item index. For add, inserts at this position; omitted means append."
                            },
                            "match_content": {
                                "type": "string",
                                "description": "Exact content used to find the target when index is omitted."
                            },
                            "content": { "type": "string", "description": "New task content for add or update." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Updated task status."
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Updated task priority."
                            }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths.clone();
            async move {
                if args.get("updates").is_some() {
                    run_scoped(&paths, args, todo_update)
                } else {
                    run_scoped(&paths, args, todo_write)
                }
            }
        },
    ).writes());
}

fn todo_write(args: Value, todos: TodoList) -> Result<String> {
    let items = args
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("todos array is required"))?;

    let mut list = Vec::with_capacity(items.len());
    for item in items {
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if content.is_empty() {
            anyhow::bail!("todo content must not be empty");
        }
        // 与 todoupdate 同一套校验:enum 外的值静默入库会让 pending 计数
        // 和状态展示错乱。
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        validate_status(status)?;
        let priority = item
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("medium");
        validate_priority(priority)?;
        list.push(Todo {
            content,
            status: status.to_string(),
            priority: priority.to_string(),
        });
    }

    let pending_count = list
        .iter()
        .filter(|t| t.status != "completed" && t.status != "cancelled")
        .count();

    let mut state = todos.lock().expect("todo state lock");
    *state = list.clone();
    drop(state);

    let display: Vec<Value> = list
        .iter()
        .map(|t| {
            json!({
                "content": t.content,
                "status": t.status,
                "priority": t.priority,
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "operation": "write",
        "pending_count": pending_count,
        "total_count": list.len(),
        "todos": display,
    }))?)
}

fn todo_update(args: Value, todos: TodoList) -> Result<String> {
    let updates = args
        .get("updates")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("updates array is required"))?;
    if updates.is_empty() {
        anyhow::bail!("updates must not be empty");
    }

    let mut state = todos.lock().expect("todo state lock");
    let mut list = state.clone();
    for update in updates {
        let action = update
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match action {
            "add" => {
                let todo = todo_from_update(update)?;
                let insert_at = match update.get("index").and_then(Value::as_u64) {
                    Some(index) if index == 0 => anyhow::bail!("index must be 1-based"),
                    Some(index) => (index as usize - 1).min(list.len()),
                    None => list.len(),
                };
                list.insert(insert_at, todo);
            }
            "update" => {
                let idx = target_index(update, &list)?;
                if let Some(content) = update.get("content").and_then(Value::as_str) {
                    let content = content.trim();
                    if content.is_empty() {
                        anyhow::bail!("todo content must not be empty");
                    }
                    list[idx].content = content.to_string();
                }
                if let Some(status) = update.get("status").and_then(Value::as_str) {
                    validate_status(status)?;
                    list[idx].status = status.to_string();
                }
                if let Some(priority) = update.get("priority").and_then(Value::as_str) {
                    validate_priority(priority)?;
                    list[idx].priority = priority.to_string();
                }
            }
            "remove" => {
                let idx = target_index(update, &list)?;
                list.remove(idx);
            }
            "clear" => list.clear(),
            _ => anyhow::bail!("action must be add, update, remove, or clear"),
        }
    }
    *state = list.clone();
    drop(state);
    todo_output("update", &list)
}

fn todo_from_update(update: &Value) -> Result<Todo> {
    let content = update
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        anyhow::bail!("content is required for add");
    }
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    validate_status(status)?;
    let priority = update
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("medium");
    validate_priority(priority)?;
    Ok(Todo {
        content,
        status: status.to_string(),
        priority: priority.to_string(),
    })
}

fn target_index(update: &Value, list: &[Todo]) -> Result<usize> {
    if let Some(index) = update.get("index").and_then(Value::as_u64) {
        if index == 0 || index as usize > list.len() {
            anyhow::bail!("index out of range");
        }
        return Ok(index as usize - 1);
    }
    let Some(content) = update.get("match_content").and_then(Value::as_str) else {
        anyhow::bail!("index or match_content is required");
    };
    let matches = list
        .iter()
        .enumerate()
        .filter(|(_, todo)| todo.content == content)
        .map(|(idx, _)| idx)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [idx] => Ok(*idx),
        [] => anyhow::bail!("no todo matches match_content"),
        _ => anyhow::bail!("match_content matches multiple todos; use index instead"),
    }
}

fn validate_status(status: &str) -> Result<()> {
    if matches!(
        status,
        "pending" | "in_progress" | "completed" | "cancelled"
    ) {
        Ok(())
    } else {
        anyhow::bail!("status must be pending, in_progress, completed, or cancelled")
    }
}

fn validate_priority(priority: &str) -> Result<()> {
    if matches!(priority, "high" | "medium" | "low") {
        Ok(())
    } else {
        anyhow::bail!("priority must be high, medium, or low")
    }
}

fn todo_output(operation: &str, list: &[Todo]) -> Result<String> {
    let pending_count = list
        .iter()
        .filter(|t| t.status != "completed" && t.status != "cancelled")
        .count();
    let display: Vec<Value> = list
        .iter()
        .map(|t| {
            json!({
                "content": t.content,
                "status": t.status,
                "priority": t.priority,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "operation": operation,
        "pending_count": pending_count,
        "total_count": list.len(),
        "todos": display,
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_full_list() {
        let todos: TodoList = Arc::new(Mutex::new(Vec::new()));
        let result = todo_write(
            json!({
                "todos": [
                    {"content": "task A", "status": "completed", "priority": "high"},
                    {"content": "task B", "status": "in_progress", "priority": "medium"},
                    {"content": "task C", "status": "pending", "priority": "low"},
                ]
            }),
            Arc::clone(&todos),
        )
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["ok"], true);
        assert_eq!(data["total_count"], 3);
        assert_eq!(data["pending_count"], 2);
        assert_eq!(todos.lock().unwrap().len(), 3);
    }

    #[test]
    fn empty_list_clears_all() {
        let todos: TodoList = Arc::new(Mutex::new(vec![Todo {
            content: "old".into(),
            status: "pending".into(),
            priority: "low".into(),
        }]));
        let result = todo_write(json!({"todos": []}), Arc::clone(&todos)).unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["total_count"], 0);
        assert_eq!(data["pending_count"], 0);
        assert!(todos.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_content_rejected() {
        let todos: TodoList = Arc::new(Mutex::new(Vec::new()));
        let result = todo_write(
            json!({"todos": [{"content": "  ", "status": "pending", "priority": "low"}]}),
            Arc::clone(&todos),
        );
        assert!(result.is_err());
    }

    #[test]
    fn update_status_by_index_is_atomic() {
        let todos: TodoList = Arc::new(Mutex::new(vec![
            Todo {
                content: "task A".into(),
                status: "pending".into(),
                priority: "high".into(),
            },
            Todo {
                content: "task B".into(),
                status: "in_progress".into(),
                priority: "medium".into(),
            },
        ]));
        let result = todo_update(
            json!({"updates": [{"action": "update", "index": 1, "status": "completed"}]}),
            Arc::clone(&todos),
        )
        .unwrap();
        let data: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(data["operation"], "update");
        let list = todos.lock().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].content, "task A");
        assert_eq!(list[0].status, "completed");
        assert_eq!(list[1].content, "task B");
        assert_eq!(list[1].status, "in_progress");
    }

    #[test]
    fn update_adds_and_removes_without_resending_full_list() {
        let todos: TodoList = Arc::new(Mutex::new(vec![Todo {
            content: "keep".into(),
            status: "pending".into(),
            priority: "low".into(),
        }]));
        todo_update(
            json!({"updates": [
                {"action": "add", "content": "new task", "status": "in_progress", "priority": "high"},
                {"action": "remove", "match_content": "keep"}
            ]}),
            Arc::clone(&todos),
        )
        .unwrap();
        let list = todos.lock().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content, "new task");
        assert_eq!(list[0].status, "in_progress");
        assert_eq!(list[0].priority, "high");
    }

    #[test]
    fn update_rejects_ambiguous_content_match() {
        let todos: TodoList = Arc::new(Mutex::new(vec![
            Todo {
                content: "same".into(),
                status: "pending".into(),
                priority: "low".into(),
            },
            Todo {
                content: "same".into(),
                status: "pending".into(),
                priority: "low".into(),
            },
        ]));
        let result = todo_update(
            json!({"updates": [{"action": "update", "match_content": "same", "status": "completed"}]}),
            Arc::clone(&todos),
        );
        assert!(result.is_err());
    }
}
