//! 工作流引擎
//!
//! 三类"自动化"在本项目里的分工：
//!
//! | 机制 | 谁决定做什么 | 适合 |
//! |------|--------------|------|
//! | `agent_tasks` | 模型边做边拆 | 一次对话里的临时计划 |
//! | `scheduled_jobs` | 到点让**模型**自己想办法 | 目标明确但步骤不定的活 |
//! | 本模块 | **用户写死**步骤与依赖 | 每次都一样的固定流程 |
//!
//! 为什么需要第三种：备份、发布、日报这类流程的正确顺序是业务知识，
//! 不是模型每次现场推理出来的。让模型每次重新规划，只是引入了不确定性
//! ——今天多做一步、明天漏一步。把顺序固化下来，模型只负责"什么时候跑"
//! 和"跑完怎么说"。
//!
//! 执行纪律：每一步都走 `ToolExecutor`（权限判定 → 回执 → 审计），
//! 不另开旁路。否则工作流就成了绕过三层闸门的后门。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

/// 单次运行最多多少步。上限的作用不是防大流程，是防"模型生成一万步"。
pub const MAX_STEPS: usize = 100;
/// 单步等待上限（毫秒）
pub const MAX_DELAY_MS: u64 = 60 * 60 * 1000;

// ============================================================
// 数据结构
// ============================================================

/// 一个步骤。
///
/// `kind` 只有四种，刻意不做成插件式：每多一种 kind 就要多一处权限判定、
/// 多一条契约、多一个"失败算什么"的语义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// 步骤标识，供 `depends_on` 与变量引用使用
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// tool / delay / notify / set_var
    #[serde(default = "default_kind")]
    pub kind: String,
    /// kind=tool 时要调的工具名
    #[serde(default)]
    pub tool: Option<String>,
    /// 工具参数；支持 {{vars.x}} / {{steps.id.data.field}} 占位
    #[serde(default)]
    pub arguments: Value,
    /// 依赖的步骤 id（全部完成后才执行本步）
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 失败后是否继续跑后续步骤（默认 false：失败即中止）
    #[serde(default)]
    pub continue_on_error: bool,
    /// 单步超时（秒），覆盖默认值
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    /// kind=delay 时等待的毫秒数
    #[serde(default)]
    pub delay_ms: Option<u64>,
    /// kind=notify 时的标题/正文
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// kind=set_var 时的变量名与值
    #[serde(default)]
    pub var_name: Option<String>,
    #[serde(default)]
    pub var_value: Option<Value>,
}

fn default_kind() -> String {
    "tool".to_string()
}

/// 工作流定义（不变式：steps 构成 DAG）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
    pub variables: Value,
    pub enabled: bool,
    pub run_count: i64,
}

/// 校验问题
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub step: String,
    pub message: String,
}

/// 解析 steps JSON 数组。
pub fn parse_steps(raw: &Value) -> AppResult<Vec<Step>> {
    let arr = raw
        .as_array()
        .ok_or_else(|| AppError::InvalidArgument("steps 必须是数组".to_string()))?;
    if arr.is_empty() {
        return Err(AppError::InvalidArgument("steps 不能为空".to_string()));
    }
    if arr.len() > MAX_STEPS {
        return Err(AppError::InvalidArgument(format!(
            "步骤数上限 {}，实际 {}",
            MAX_STEPS,
            arr.len()
        )));
    }
    let mut steps = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let step: Step = serde_json::from_value(item.clone()).map_err(|e| {
            AppError::InvalidArgument(format!("第 {} 个步骤结构不合法: {}", i + 1, e))
        })?;
        steps.push(step);
    }
    // 没写 id 的自动补：模型经常只给 name
    for (i, step) in steps.iter_mut().enumerate() {
        if step.id.trim().is_empty() {
            step.id = format!("step{}", i + 1);
        }
        if step.name.trim().is_empty() {
            step.name = step
                .tool
                .clone()
                .unwrap_or_else(|| format!("步骤 {}", i + 1));
        }
    }
    Ok(steps)
}

/// 检查 DAG 是否合法，返回全部问题（不是遇到第一个就返回）。
///
/// 一次报全：模型改一处再试一次要跑一整轮对话，把问题一次性列出来
/// 能省掉大部分往返。
pub fn validate(steps: &[Step]) -> Vec<Problem> {
    let mut problems: Vec<Problem> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for step in steps {
        // id 必须是"能当变量名"的形式：它会被拼进 {{steps.<id>.data.x}}，
        // 带空格或点号会让占位符解析歧义。
        if step.id.is_empty() {
            problems.push(Problem {
                step: step.id.clone(),
                message: "步骤 id 不能为空".to_string(),
            });
        } else if !step
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            problems.push(Problem {
                step: step.id.clone(),
                message: format!("步骤 id「{}」只能包含字母、数字、下划线、连字符", step.id),
            });
        }
        if !seen.insert(step.id.as_str()) {
            problems.push(Problem {
                step: step.id.clone(),
                message: format!("步骤 id「{}」重复", step.id),
            });
        }

        match step.kind.as_str() {
            "tool" => {
                let tool = step.tool.as_deref().unwrap_or("").trim();
                if tool.is_empty() {
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: "kind=tool 的步骤必须指定 tool".to_string(),
                    });
                } else if tool == "workflow_run" {
                    // 直接挡掉自递归：工作流里再起一个工作流，
                    // 一旦形成环就是无限进程 + 无限数据库写入。
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: "工作流步骤不能调用 workflow_run（会形成自递归）".to_string(),
                    });
                }
            }
            "delay" => {
                let ms = step.delay_ms.unwrap_or(0);
                if ms == 0 {
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: "kind=delay 的步骤必须给 delay_ms".to_string(),
                    });
                } else if ms > MAX_DELAY_MS {
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: format!("delay_ms 上限 {}（1 小时），实际 {}", MAX_DELAY_MS, ms),
                    });
                }
            }
            "notify" => {
                if step
                    .message
                    .as_deref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
                {
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: "kind=notify 的步骤必须给 message".to_string(),
                    });
                }
            }
            "set_var" => {
                if step
                    .var_name
                    .as_deref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
                {
                    problems.push(Problem {
                        step: step.id.clone(),
                        message: "kind=set_var 的步骤必须给 var_name".to_string(),
                    });
                }
            }
            other => problems.push(Problem {
                step: step.id.clone(),
                message: format!(
                    "未知的 kind「{}」，只支持 tool / delay / notify / set_var",
                    other
                ),
            }),
        }
    }

    // 依赖引用必须存在
    let ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    for step in steps {
        for dep in &step.depends_on {
            if !ids.contains(dep.as_str()) {
                problems.push(Problem {
                    step: step.id.clone(),
                    message: format!("依赖的步骤「{}」不存在", dep),
                });
            }
            if dep == &step.id {
                problems.push(Problem {
                    step: step.id.clone(),
                    message: "步骤不能依赖自己".to_string(),
                });
            }
        }
    }

    // 环检测：Kahn 算法，剩下的就是环上的点
    if problems.is_empty() {
        let mut indegree: HashMap<&str, usize> = steps
            .iter()
            .map(|s| (s.id.as_str(), s.depends_on.len()))
            .collect();
        let mut queue: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0usize;
        while let Some(current) = queue.pop() {
            visited += 1;
            for step in steps {
                if step.depends_on.iter().any(|d| d == current) {
                    if let Some(d) = indegree.get_mut(step.id.as_str()) {
                        *d -= 1;
                        if *d == 0 {
                            queue.push(step.id.as_str());
                        }
                    }
                }
            }
        }
        if visited != steps.len() {
            let stuck: Vec<&str> = indegree
                .iter()
                .filter(|(_, d)| **d > 0)
                .map(|(id, _)| *id)
                .collect();
            problems.push(Problem {
                step: stuck.join(","),
                message: "步骤依赖构成环，无法确定执行顺序".to_string(),
            });
        }
    }

    problems
}

/// 拓扑排序：返回按依赖顺序排好的步骤下标。
///
/// 同一层内保持**声明顺序**（不是哈希序）：用户写 [备份, 压缩] 时
/// 期望就是先备份再压缩，即使两者没有显式依赖。
pub fn topo_order(steps: &[Step]) -> Result<Vec<usize>, String> {
    let mut indegree: Vec<usize> = steps.iter().map(|s| s.depends_on.len()).collect();
    let mut order: Vec<usize> = Vec::with_capacity(steps.len());
    let mut done = vec![false; steps.len()];

    while order.len() < steps.len() {
        let mut progressed = false;
        for i in 0..steps.len() {
            if done[i] || indegree[i] > 0 {
                continue;
            }
            order.push(i);
            done[i] = true;
            progressed = true;
            for j in 0..steps.len() {
                if steps[j].depends_on.iter().any(|d| d == &steps[i].id) {
                    indegree[j] = indegree[j].saturating_sub(1);
                }
            }
            break; // 重新从头扫，保证同层按声明顺序
        }
        if !progressed {
            return Err("步骤依赖构成环".to_string());
        }
    }
    Ok(order)
}

// ============================================================
// 变量替换
// ============================================================

/// 运行期上下文
#[derive(Debug, Default)]
pub struct RunContext {
    pub run_id: String,
    pub vars: HashMap<String, Value>,
    /// step_id → 该步工具返回的 data
    pub outputs: HashMap<String, Value>,
    /// step_id → 是否成功
    pub ok: HashMap<String, bool>,
}

impl RunContext {
    /// 解析一个占位表达式，例如 `steps.fetch.data.count`。
    fn lookup(&self, path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return None;
        }
        match parts[0] {
            "run" if parts.len() > 1 && parts[1] == "id" => {
                Some(Value::String(self.run_id.clone()))
            }
            "now" => Some(Value::String(
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            )),
            "vars" if parts.len() > 1 => self.vars.get(parts[1]).cloned(),
            "steps" if parts.len() > 2 => {
                let step_id = parts[1];
                if parts[2] == "ok" {
                    return self.ok.get(step_id).map(|b| Value::Bool(*b));
                }
                if parts[2] != "data" {
                    return None;
                }
                let mut current = self.outputs.get(step_id)?.clone();
                for key in &parts[3..] {
                    current = match current {
                        Value::Object(map) => map.get(*key)?.clone(),
                        Value::Array(arr) => {
                            let idx: usize = key.parse().ok()?;
                            arr.get(idx)?.clone()
                        }
                        _ => return None,
                    };
                }
                Some(current)
            }
            _ => None,
        }
    }
}

/// 递归替换字符串里的 `{{...}}`。
///
/// 整串只有一个占位符时保留原类型（数字仍是数字、对象仍是对象）——
/// 否则把 `{{steps.a.data.count}}` 变成字符串 "5"，后续工具拿到的是
/// 字符串而不是整数，会在参数校验上栽跟头。
pub fn resolve_value(value: &Value, ctx: &RunContext) -> Value {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with("{{")
                && trimmed.ends_with("}}")
                && trimmed.matches("{{").count() == 1
            {
                let expr = &trimmed[2..trimmed.len() - 2];
                return ctx.lookup(expr.trim()).unwrap_or(Value::Null);
            }
            let mut out = text.clone();
            let mut cursor = 0usize;
            let mut guard = 0;
            while guard < 100 {
                guard += 1;
                let Some(start_rel) = out[cursor..].find("{{") else {
                    break;
                };
                let start = cursor + start_rel;
                let Some(end_rel) = out[start..].find("}}") else {
                    break;
                };
                let end = start + end_rel;
                let expr = out[start + 2..end].trim().to_string();
                let replacement = match ctx.lookup(&expr) {
                    Some(Value::String(s)) => s,
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                out.replace_range(start..end + 2, &replacement);
                cursor = start + replacement.len();
            }
            Value::String(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|v| resolve_value(v, ctx)).collect()),
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (k, v) in map {
                result.insert(k.clone(), resolve_value(v, ctx));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

// ============================================================
// 存储
// ============================================================

fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 读定义
pub fn load_definition(workflow_id: &str) -> AppResult<Option<Definition>> {
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    let mut stmt = conn.prepare(
        "SELECT id, name, description, steps_json, variables_json, enabled, run_count
         FROM workflow_defs WHERE id = ?1",
    )?;
    let mut rows = stmt.query([workflow_id])?;
    if let Some(row) = rows.next()? {
        let steps_json: String = row.get(3)?;
        let variables_json: String = row.get(4)?;
        let steps = parse_steps(&serde_json::from_str(&steps_json).unwrap_or(json!([])))
            .unwrap_or_default();
        Ok(Some(Definition {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            steps,
            variables: serde_json::from_str(&variables_json).unwrap_or(json!({})),
            enabled: row.get::<_, i64>(5)? != 0,
            run_count: row.get(6)?,
        }))
    } else {
        Ok(None)
    }
}

/// 按名字找定义
pub fn find_by_name(name: &str) -> AppResult<Option<Definition>> {
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM workflow_defs WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .ok();
    match id {
        Some(id) => load_definition(&id),
        None => Ok(None),
    }
}

/// 写一条运行记录，返回 run_id
pub fn create_run(
    workflow_id: &str,
    workflow_name: &str,
    session_id: &str,
    trigger: &str,
    step_count: usize,
) -> AppResult<String> {
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    let run_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO workflow_runs (id, workflow_id, workflow_name, session_id, trigger_kind, status, started_at, step_count)
         VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
        rusqlite::params![run_id, workflow_id, workflow_name, session_id, trigger, now_text(), step_count as i64],
    )?;
    Ok(run_id)
}

/// 记录一步的开始/结果
pub fn record_step(
    run_id: &str,
    step: &Step,
    seq: usize,
    status: &str,
    output: Option<&Value>,
    error: Option<&str>,
) -> AppResult<()> {
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    let output_text = output.map(|v| {
        // 太大的输出不整段入库：一个 list_directory 在 C 盘根目录就能产出几 MB，
        // 塞进 SQLite 只会让运行历史变成第二个磁盘占用源。
        let s = v.to_string();
        if s.chars().count() > 8000 {
            let cut: String = s.chars().take(8000).collect();
            json!({ "truncated": true, "preview": cut })
        } else {
            v.clone()
        }
        .to_string()
    });
    conn.execute(
        "INSERT INTO workflow_steps (run_id, step_id, name, kind, seq, status, started_at, finished_at, output_json, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            run_id,
            step.id,
            step.name,
            step.kind,
            seq as i64,
            status,
            if status == "pending" || status == "skipped" { None } else { Some(now_text()) },
            if status == "pending" || status == "running" { None } else { Some(now_text()) },
            output_text,
            error,
        ],
    )?;
    Ok(())
}

/// 收尾：写运行结果、累加定义上的计数
pub fn finish_run(
    run_id: &str,
    workflow_id: &str,
    status: &str,
    ok_count: usize,
    fail_count: usize,
    summary: &str,
    error: Option<&str>,
) -> AppResult<()> {
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    conn.execute(
        "UPDATE workflow_runs SET status = ?1, finished_at = ?2, ok_count = ?3, fail_count = ?4, summary = ?5, error = ?6
         WHERE id = ?7",
        rusqlite::params![status, now_text(), ok_count as i64, fail_count as i64, summary, error, run_id],
    )?;
    conn.execute(
        "UPDATE workflow_defs SET run_count = run_count + 1, last_run_at = ?1, last_status = ?2, updated_at = ?2
         WHERE id = ?3",
        rusqlite::params![now_text(), status, workflow_id],
    )?;
    Ok(())
}

// ============================================================
// 执行
// ============================================================

/// 单步执行的原始结果
pub struct StepOutcome {
    pub ok: bool,
    pub data: Value,
    pub error: Option<String>,
}

/// 执行一个步骤的"动作"部分（不含拓扑与记录）。
async fn execute_step(step: &Step, args: Value, session_id: &str, run_id: &str) -> StepOutcome {
    match step.kind.as_str() {
        "delay" => {
            let ms = step.delay_ms.unwrap_or(1000).min(MAX_DELAY_MS);
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            StepOutcome {
                ok: true,
                data: json!({ "waited_ms": ms }),
                error: None,
            }
        }
        "notify" => {
            // 通知走系统能力，不经过工具层：它没有副作用，
            // 也不需要权限确认，走工具层只会多一次回执噪音。
            let title = step.title.clone().unwrap_or_else(|| step.name.clone());
            let body = step.message.clone().unwrap_or_default();
            match crate::tools::executors::os_extra::show_toast(&title, &body) {
                Ok((true, _)) => StepOutcome {
                    ok: true,
                    data: json!({ "title": title, "message": body }),
                    error: None,
                },
                Ok((false, stderr)) => StepOutcome {
                    ok: false,
                    data: Value::Null,
                    error: Some(format!("通知发送失败: {}", stderr.trim())),
                },
                Err(e) => StepOutcome {
                    ok: false,
                    data: Value::Null,
                    error: Some(format!("通知发送失败: {}", e)),
                },
            }
        }
        "set_var" => {
            let name = step.var_name.clone().unwrap_or_default();
            StepOutcome {
                ok: true,
                data: json!({ "var": name, "value": step.var_value.clone().unwrap_or(Value::Null) }),
                error: None,
            }
        }
        _ => {
            // kind=tool：走三层闸门
            let tool = match step.tool.as_deref() {
                Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                _ => {
                    return StepOutcome {
                        ok: false,
                        data: Value::Null,
                        error: Some("kind=tool 但没指定 tool".to_string()),
                    }
                }
            };
            let Some(executor) = crate::tools::executor::global_executor() else {
                return StepOutcome {
                    ok: false,
                    data: Value::Null,
                    error: Some("工具执行器未初始化：工作流需要在应用运行时执行".to_string()),
                };
            };

            // 记住调用方的会话，执行完还原：executor 会把会话写进全局槽，
            // 不还原的话外层工具后续拿到的 session 会变成工作流的。
            let outer_session = crate::tools::current_session();
            let request = crate::tools::ToolCallRequest {
                tool_name: tool.clone(),
                arguments: args,
                session_id: Some(session_id.to_string()),
                run_id: Some(run_id.to_string()),
            };
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(step.timeout_sec.unwrap_or(180).max(5)),
                executor.execute(request),
            )
            .await;
            crate::tools::set_current_session(&outer_session);

            match result {
                Err(_) => StepOutcome {
                    ok: false,
                    data: Value::Null,
                    error: Some(format!(
                        "步骤超时（{} 秒）",
                        step.timeout_sec.unwrap_or(180)
                    )),
                },
                Ok(Err(e)) => StepOutcome {
                    ok: false,
                    data: Value::Null,
                    error: Some(format!("调用 {} 失败: {}", tool, e)),
                },
                Ok(Ok(outcome)) => {
                    if outcome.result.success {
                        StepOutcome {
                            ok: true,
                            data: outcome.result.data.unwrap_or(Value::Null),
                            error: None,
                        }
                    } else {
                        StepOutcome {
                            ok: false,
                            data: outcome.result.data.unwrap_or(Value::Null),
                            error: outcome
                                .result
                                .error
                                .or(outcome.result.error_code)
                                .or(Some("工具返回失败".to_string())),
                        }
                    }
                }
            }
        }
    }
}

/// 运行结果摘要
pub struct RunOutcome {
    pub run_id: String,
    pub status: String,
    pub ok_count: usize,
    pub fail_count: usize,
    pub summary: String,
    pub steps: Vec<Value>,
}

/// 跑一个已加载的定义。
///
/// `session_id` 决定权限语义：传 `autonomous:` 前缀的会话时，
/// 需要确认的工具会被直接拒绝（无人值守不能弹窗）。
pub async fn run_definition(
    def: &Definition,
    session_id: &str,
    trigger: &str,
    extra_vars: Value,
) -> AppResult<RunOutcome> {
    let problems = validate(&def.steps);
    if !problems.is_empty() {
        let detail = problems
            .iter()
            .map(|p| format!("[{}] {}", p.step, p.message))
            .collect::<Vec<_>>()
            .join("；");
        return Err(AppError::InvalidArgument(format!(
            "工作流定义有问题，无法运行: {}",
            detail
        )));
    }

    let order = topo_order(&def.steps)
        .map_err(|e| AppError::InvalidArgument(format!("无法确定执行顺序: {}", e)))?;

    let run_id = create_run(&def.id, &def.name, session_id, trigger, def.steps.len())?;

    let mut ctx = RunContext {
        run_id: run_id.clone(),
        ..Default::default()
    };
    // 变量优先级：定义里的默认值 < 本次运行传入的值
    if let Value::Object(map) = &def.variables {
        for (k, v) in map {
            ctx.vars.insert(k.clone(), v.clone());
        }
    }
    if let Value::Object(map) = &extra_vars {
        for (k, v) in map {
            ctx.vars.insert(k.clone(), v.clone());
        }
    }

    let mut step_results: Vec<Value> = Vec::new();
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    let mut aborted = false;
    let mut first_error: Option<String> = None;

    for (seq, index) in order.iter().enumerate() {
        let step = &def.steps[*index];

        // 前序失败且该步没有 continue_on_error → 跳过
        if aborted {
            record_step(
                &run_id,
                step,
                seq,
                "skipped",
                None,
                Some("前序步骤失败，已中止"),
            )?;
            step_results.push(json!({
                "step": step.id,
                "name": step.name,
                "kind": step.kind,
                "status": "skipped",
            }));
            continue;
        }

        // 依赖里有失败的步骤，且本步没被标记继续 → 跳过
        let dep_failed = step
            .depends_on
            .iter()
            .any(|d| ctx.ok.get(d).map(|ok| !ok).unwrap_or(true));

        if dep_failed && !step.continue_on_error {
            fail_count += 1;
            record_step(
                &run_id,
                step,
                seq,
                "skipped",
                None,
                Some("依赖的步骤未成功"),
            )?;
            step_results.push(json!({
                "step": step.id,
                "name": step.name,
                "kind": step.kind,
                "status": "skipped",
                "reason": "依赖的步骤未成功",
            }));
            continue;
        }

        let args = resolve_value(&step.arguments, &ctx);
        let outcome = execute_step(step, args, session_id, &run_id).await;

        if outcome.ok {
            ok_count += 1;
            ctx.ok.insert(step.id.clone(), true);
            ctx.outputs.insert(step.id.clone(), outcome.data.clone());
            if step.kind == "set_var" {
                if let Some(name) = step.var_name.as_deref() {
                    ctx.vars.insert(
                        name.to_string(),
                        step.var_value.clone().unwrap_or(Value::Null),
                    );
                }
            }
            record_step(&run_id, step, seq, "ok", Some(&outcome.data), None)?;
            step_results.push(json!({
                "step": step.id,
                "name": step.name,
                "kind": step.kind,
                "status": "ok",
                "data": outcome.data,
            }));
        } else {
            fail_count += 1;
            ctx.ok.insert(step.id.clone(), false);
            let message = outcome
                .error
                .clone()
                .unwrap_or_else(|| "未知错误".to_string());
            if first_error.is_none() {
                first_error = Some(format!("{}: {}", step.id, message));
            }
            record_step(
                &run_id,
                step,
                seq,
                "failed",
                if outcome.data.is_null() {
                    None
                } else {
                    Some(&outcome.data)
                },
                Some(&message),
            )?;
            step_results.push(json!({
                "step": step.id,
                "name": step.name,
                "kind": step.kind,
                "status": "failed",
                "error": message,
            }));
            if !step.continue_on_error {
                aborted = true;
            }
        }
    }

    let status = if fail_count == 0 {
        "success"
    } else if ok_count == 0 {
        "failed"
    } else {
        "partial"
    };
    let summary = format!(
        "共 {} 步：成功 {}，失败/跳过 {}",
        def.steps.len(),
        ok_count,
        fail_count
    );
    finish_run(
        &run_id,
        &def.id,
        status,
        ok_count,
        fail_count,
        &summary,
        first_error.as_deref(),
    )?;

    Ok(RunOutcome {
        run_id,
        status: status.to_string(),
        ok_count,
        fail_count,
        summary,
        steps: step_results,
    })
}

// ============================================================
// 内置模板
// ============================================================

/// 开箱即用的几个流程模板。
///
/// 只放"参数全是占位符、用户改两个值就能跑"的：模板要是还得先读半小时文档，
/// 那就等于没给。
pub fn templates() -> Value {
    json!([
        {
            "name": "每日备份目录",
            "description": "把指定目录复制到备份位置，压缩后发通知。把 source / destination 换成真实路径即可。",
            "variables": { "source": "", "destination": "" },
            "steps": [
                { "id": "prepare", "name": "确认目标目录存在", "kind": "tool", "tool": "create_directory",
                  "arguments": { "path": "{{vars.destination}}" } },
                { "id": "copy", "name": "复制目录", "kind": "tool", "tool": "copy_path", "depends_on": ["prepare"],
                  "arguments": { "source": "{{vars.source}}", "destination": "{{vars.destination}}", "overwrite": true } },
                { "id": "done", "name": "发通知", "kind": "notify", "depends_on": ["copy"],
                  "title": "备份完成", "message": "目录 {{vars.source}} 已备份到 {{vars.destination}}" }
            ]
        },
        {
            "name": "整理下载目录",
            "description": "统计下载目录里的大文件，把清单写成文本文件并通知。",
            "variables": { "downloads": "" },
            "steps": [
                { "id": "list", "name": "列出目录", "kind": "tool", "tool": "list_directory",
                  "arguments": { "path": "{{vars.downloads}}", "recursive": false } },
                { "id": "notify", "name": "报告结果", "kind": "notify", "depends_on": ["list"],
                  "title": "下载目录清单", "message": "共 {{steps.list.data.count}} 项" }
            ]
        },
        {
            "name": "项目体检",
            "description": "查 Git 状态、看磁盘占用，最后汇总通知。适合每天开工前跑一次。",
            "variables": { "repo": "" },
            "steps": [
                { "id": "status", "name": "Git 状态", "kind": "tool", "tool": "git_status",
                  "arguments": { "repo": "{{vars.repo}}" } },
                { "id": "disk", "name": "磁盘占用", "kind": "tool", "tool": "get_disk_usage",
                  "arguments": {} },
                { "id": "report", "name": "汇总通知", "kind": "notify", "depends_on": ["status", "disk"],
                  "title": "项目体检", "message": "工作区{{steps.status.data.clean}}（true=干净），分支 {{steps.status.data.branch}}" }
            ]
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, tool: &str, deps: &[&str]) -> Step {
        Step {
            id: id.to_string(),
            name: id.to_string(),
            kind: "tool".to_string(),
            tool: Some(tool.to_string()),
            arguments: json!({}),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            continue_on_error: false,
            timeout_sec: None,
            delay_ms: None,
            title: None,
            message: None,
            var_name: None,
            var_value: None,
        }
    }

    #[test]
    fn validate_accepts_a_simple_chain() {
        let steps = vec![step("a", "read_file", &[]), step("b", "write_file", &["a"])];
        assert!(validate(&steps).is_empty(), "{:?}", validate(&steps));
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let steps = vec![step("a", "read_file", &[]), step("a", "write_file", &[])];
        let problems = validate(&steps);
        assert!(problems.iter().any(|p| p.message.contains("重复")));
    }

    #[test]
    fn validate_rejects_missing_dependency() {
        let steps = vec![step("a", "read_file", &["ghost"])];
        let problems = validate(&steps);
        assert!(problems.iter().any(|p| p.message.contains("不存在")));
    }

    #[test]
    fn validate_detects_cycles() {
        let mut a = step("a", "read_file", &["b"]);
        let mut b = step("b", "write_file", &["a"]);
        let problems = validate(&[a.clone(), b.clone()]);
        assert!(problems.iter().any(|p| p.message.contains("环")));
        // topo_order 也必须拒绝，不能返回一个看似合理的顺序
        assert!(topo_order(&[a.clone(), b.clone()]).is_err());
        a.depends_on.clear();
        b.depends_on.clear();
    }

    #[test]
    fn validate_blocks_workflow_recursion() {
        let steps = vec![step("a", "workflow_run", &[])];
        let problems = validate(&steps);
        assert!(problems.iter().any(|p| p.message.contains("自递归")));
    }

    #[test]
    fn validate_rejects_unknown_kind() {
        let mut s = step("a", "read_file", &[]);
        s.kind = "magic".to_string();
        let problems = validate(&[s]);
        assert!(problems.iter().any(|p| p.message.contains("未知的 kind")));
    }

    #[test]
    fn topo_order_is_stable_within_a_layer() {
        // 三步互不依赖：顺序必须等于声明顺序，不能是哈希序
        let steps = vec![
            step("backup", "copy_path", &[]),
            step("compress", "execute_command", &[]),
            step("notify", "send_notification", &[]),
        ];
        let order = topo_order(&steps).unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn topo_order_respects_dependencies() {
        let steps = vec![
            step("c", "send_notification", &["b"]),
            step("a", "copy_path", &[]),
            step("b", "execute_command", &["a"]),
        ];
        let order = topo_order(&steps).unwrap();
        let pos: HashMap<usize, usize> = order.iter().enumerate().map(|(i, s)| (*s, i)).collect();
        assert!(pos[&1] < pos[&2], "a 必须先于 b");
        assert!(pos[&2] < pos[&0], "b 必须先于 c");
    }

    #[test]
    fn resolve_keeps_original_type_for_whole_placeholder() {
        let mut ctx = RunContext {
            run_id: "run-1".to_string(),
            ..Default::default()
        };
        ctx.outputs
            .insert("list".to_string(), json!({ "count": 42, "ok": true }));
        let value = resolve_value(&json!("{{steps.list.data.count}}"), &ctx);
        assert_eq!(value, json!(42), "整串占位符应保留数字类型");
    }

    #[test]
    fn resolve_embeds_values_inside_longer_text() {
        let mut ctx = RunContext {
            run_id: "run-1".to_string(),
            ..Default::default()
        };
        ctx.vars.insert("name".to_string(), json!("报告"));
        let value = resolve_value(&json!("今天生成{{vars.name}}完毕"), &ctx);
        assert_eq!(value, json!("今天生成报告完毕"));
    }

    #[test]
    fn resolve_missing_reference_becomes_empty_not_literal() {
        let ctx = RunContext::default();
        // 留 `{{...}}` 原文会让下游工具拿到一串占位符当参数，
        // 变成"路径不存在"这种莫名其妙的错误
        assert_eq!(
            resolve_value(&json!("x{{steps.nope.data.a}}y"), &ctx),
            json!("xy")
        );
    }

    #[test]
    fn resolve_walks_nested_structures() {
        let mut ctx = RunContext::default();
        ctx.vars.insert("dir".to_string(), json!("D:/data"));
        let value = resolve_value(
            &json!({ "path": "{{vars.dir}}", "list": ["{{vars.dir}}", 1] }),
            &ctx,
        );
        assert_eq!(value["path"], json!("D:/data"));
        assert_eq!(value["list"][0], json!("D:/data"));
        assert_eq!(value["list"][1], json!(1));
    }

    #[test]
    fn run_context_exposes_step_ok() {
        let mut ctx = RunContext::default();
        ctx.ok.insert("a".to_string(), true);
        assert_eq!(resolve_value(&json!("{{steps.a.ok}}"), &ctx), json!(true));
    }

    #[test]
    fn templates_are_all_valid() {
        let list = templates();
        let arr = list.as_array().expect("模板应是数组");
        assert!(arr.len() >= 3);
        for tpl in arr {
            let steps = parse_steps(&tpl["steps"]).expect("模板步骤应可解析");
            let problems = validate(&steps);
            assert!(
                problems.is_empty(),
                "模板「{}」不合法: {:?}",
                tpl["name"],
                problems
            );
            // 模板里的工具名必须真实存在，否则模板一用就报"工具不存在"
            let registry = crate::tools::ToolRegistry::default();
            for s in &steps {
                if s.kind == "tool" {
                    let tool = s.tool.clone().unwrap_or_default();
                    assert!(
                        registry.get(&tool).is_some(),
                        "模板「{}」引用了不存在的工具 {}",
                        tpl["name"],
                        tool
                    );
                }
            }
        }
    }
}
