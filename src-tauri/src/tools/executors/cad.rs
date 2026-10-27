//! CAD 图纸工具（cad 域）
//!
//! 借鉴早期原型的 cad 域（读逻辑、用 Rust 重写，不逐字复制）。
//! 早期的 `cad_draw_floorplan_in_autocad` 依赖 MCP 连 AutoCAD + 视觉 OCR，
//! 对单机 MIT 项目不可行，这里**只做不依赖 AutoCAD 的部分**：DXF 生成与解析。
//! DXF 是 ASCII 文本格式（R12），可以纯函数生成，被 AutoCAD / FreeCAD /
//! LibreCAD 等直接打开——这是个人助手场景里真正有用的 CAD 能力。

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 浮点 → DXF 数值：整数不带小数点，小数保留到 4 位并去掉尾零。
fn dxf_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// 一个 DXF 图形元素。
#[derive(Debug, Clone)]
pub enum DxfShape {
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
    },
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    Text {
        x: f64,
        y: f64,
        height: f64,
        text: String,
    },
}

impl DxfShape {
    /// 从 JSON 参数解析；类型不支持返回 None。
    fn from_value(v: &Value) -> Option<Self> {
        let ty = v.get("type")?.as_str()?;
        let num = |k: &str| v.get(k).and_then(|x| x.as_f64());
        match ty {
            "line" => Some(DxfShape::Line {
                x1: num("x1")?,
                y1: num("y1")?,
                x2: num("x2")?,
                y2: num("y2")?,
            }),
            "circle" => Some(DxfShape::Circle {
                cx: num("cx")?,
                cy: num("cy")?,
                r: num("r")?,
            }),
            "rect" => Some(DxfShape::Rect {
                x: num("x")?,
                y: num("y")?,
                w: num("w")?,
                h: num("h")?,
            }),
            "text" => Some(DxfShape::Text {
                x: num("x")?,
                y: num("y")?,
                height: num("height").unwrap_or(2.5),
                text: v.get("text")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }
}

/// 生成一份最小可用的 DXF（R12）文本。
///
/// 纯函数，方便在不装任何 CAD 软件的情况下测试。
pub fn build_dxf(shapes: &[DxfShape]) -> String {
    let mut out = String::new();
    out.push_str("0\nSECTION\n2\nENTITIES\n");

    for shape in shapes {
        match shape {
            DxfShape::Line { x1, y1, x2, y2 } => {
                out.push_str(&format!(
                    "0\nLINE\n8\n0\n10\n{}\n20\n{}\n11\n{}\n21\n{}\n",
                    dxf_num(*x1),
                    dxf_num(*y1),
                    dxf_num(*x2),
                    dxf_num(*y2)
                ));
            }
            DxfShape::Circle { cx, cy, r } => {
                out.push_str(&format!(
                    "0\nCIRCLE\n8\n0\n10\n{}\n20\n{}\n40\n{}\n",
                    dxf_num(*cx),
                    dxf_num(*cy),
                    dxf_num(*r)
                ));
            }
            DxfShape::Rect { x, y, w, h } => {
                let corners = [
                    (*x, *y, *x + *w, *y),
                    (*x + *w, *y, *x + *w, *y + *h),
                    (*x + *w, *y + *h, *x, *y + *h),
                    (*x, *y + *h, *x, *y),
                ];
                for (x1, y1, x2, y2) in corners {
                    out.push_str(&format!(
                        "0\nLINE\n8\n0\n10\n{}\n20\n{}\n11\n{}\n21\n{}\n",
                        dxf_num(x1),
                        dxf_num(y1),
                        dxf_num(x2),
                        dxf_num(y2)
                    ));
                }
            }
            DxfShape::Text { x, y, height, text } => {
                // DXF 的文本组(1)是单行，换行会破坏结构
                let text = text.replace(['\r', '\n'], " ");
                out.push_str(&format!(
                    "0\nTEXT\n8\n0\n10\n{}\n20\n{}\n40\n{}\n1\n{}\n",
                    dxf_num(*x),
                    dxf_num(*y),
                    dxf_num(*height),
                    text
                ));
            }
        }
    }

    out.push_str("0\nENDSEC\n0\nEOF\n");
    out
}

/// 统计 DXF 文本里的实体数量（按类型）。
pub fn count_dxf_entities(dxf: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let structural = [
        "SECTION", "ENDSEC", "HEADER", "EOF", "ENTITIES", "TABLES", "BLOCKS",
    ];

    let lines: Vec<&str> = dxf.lines().collect();
    let mut i = 0;
    while i + 1 < lines.len() {
        // DXF 的实体名是「0 组」后跟的那一行
        if lines[i].trim() == "0" {
            let name = lines[i + 1].trim().to_uppercase();
            if !structural.contains(&name.as_str()) && !name.is_empty() {
                *counts.entry(name).or_insert(0) += 1;
            }
        }
        i += 1;
    }
    counts
}

// ============================================================
// cad_generate_dxf
// ============================================================

pub struct CadGenerateDxfTool;

#[async_trait]
impl AgentTool for CadGenerateDxfTool {
    fn name(&self) -> &'static str {
        "cad_generate_dxf"
    }

    fn description(&self) -> &'static str {
        "把一组基本图形（线段 line / 圆 circle / 矩形 rect / 文字 text）生成一个 .dxf 图纸文件，可被 AutoCAD、FreeCAD、LibreCAD 打开。适合「画个简单示意图」「生成一个带尺寸标注的矩形」这类需求。坐标单位由调用方自定。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出 .dxf 文件路径" },
                "entities": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["line", "circle", "rect", "text"] },
                            "x1": { "type": "number" }, "y1": { "type": "number" },
                            "x2": { "type": "number" }, "y2": { "type": "number" },
                            "cx": { "type": "number" }, "cy": { "type": "number" }, "r": { "type": "number" },
                            "x": { "type": "number" }, "y": { "type": "number" },
                            "w": { "type": "number" }, "h": { "type": "number" },
                            "height": { "type": "number" },
                            "text": { "type": "string" }
                        }
                    },
                    "description": "图形元素列表。line 需要 x1/y1/x2/y2；circle 需要 cx/cy/r；rect 需要 x/y/w/h；text 需要 x/y/text（可选 height）"
                }
            },
            "required": ["path", "entities"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;

        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext != "dxf" {
            return Ok(ToolResult::err(
                "INVALID_EXTENSION",
                &format!("输出扩展名应为 .dxf，实际：{}", ext),
            ));
        }

        let entities = match args["entities"].as_array() {
            Some(arr) => arr,
            None => return Ok(ToolResult::err("INVALID_PARAMS", "entities 必须是数组")),
        };

        let mut shapes = Vec::new();
        for (i, e) in entities.iter().enumerate() {
            match DxfShape::from_value(e) {
                Some(s) => shapes.push(s),
                None => {
                    return Ok(ToolResult::err(
                        "INVALID_ENTITY",
                        &format!("第 {} 个元素类型无法解析或参数缺失", i + 1),
                    ))
                }
            }
        }
        if shapes.is_empty() {
            return Ok(ToolResult::err("INVALID_PARAMS", "entities 不能为空"));
        }

        let dxf = build_dxf(&shapes);
        std::fs::write(path, &dxf)
            .map_err(|e| AppError::ToolExecution(format!("写入 DXF 失败：{}", e)))?;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(ToolResult::ok(json!({
            "created": true,
            "path": path,
            "entity_count": shapes.len(),
            "size_bytes": size,
        })))
    }
}

// ============================================================
// cad_dxf_info
// ============================================================

pub struct CadDxfInfoTool;

#[async_trait]
impl AgentTool for CadDxfInfoTool {
    fn name(&self) -> &'static str {
        "cad_dxf_info"
    }

    fn description(&self) -> &'static str {
        "读取 .dxf 图纸的基本信息：按类型统计实体数量（线段/圆/文字等）。回答「这个图纸里画了什么」时用。只读文本统计，不依赖任何 CAD 软件。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".dxf 文件路径" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                return Ok(ToolResult::err(
                    "FILE_NOT_FOUND",
                    &format!("文件不存在或不是文本：{}", path),
                ))
            }
        };

        let counts = count_dxf_entities(&content);
        let total: usize = counts.values().sum();

        Ok(ToolResult::ok(json!({
            "path": path,
            "entities": counts,
            "entity_count": total,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxf_num_drops_trailing_zeros_and_keeps_integers() {
        assert_eq!(dxf_num(100.0), "100");
        assert_eq!(dxf_num(12.5), "12.5");
        assert_eq!(dxf_num(0.0), "0");
        assert_eq!(dxf_num(3.14159), "3.1416");
    }

    #[test]
    fn build_dxf_emits_line_and_circle() {
        let dxf = build_dxf(&[
            DxfShape::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 100.0,
            },
            DxfShape::Circle {
                cx: 50.0,
                cy: 50.0,
                r: 25.0,
            },
        ]);
        assert!(dxf.contains("0\nSECTION\n2\nENTITIES"));
        assert!(dxf.contains("0\nLINE"));
        assert!(dxf.contains("0\nCIRCLE"));
        assert!(dxf.contains("40\n25"), "圆的半径应写入 40 组");
        assert!(dxf.ends_with("0\nENDSEC\n0\nEOF\n"));
    }

    #[test]
    fn build_dxf_expands_rect_to_four_lines() {
        let dxf = build_dxf(&[DxfShape::Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 20.0,
        }]);
        assert_eq!(dxf.matches("0\nLINE").count(), 4, "矩形应展开成 4 条线");
    }

    #[test]
    fn build_dxf_sanitizes_newlines_in_text() {
        let dxf = build_dxf(&[DxfShape::Text {
            x: 1.0,
            y: 2.0,
            height: 3.0,
            text: "a\nb".to_string(),
        }]);
        assert!(dxf.contains("1\na b"), "换行应替换成空格");
    }

    #[test]
    fn count_dxf_entities_tallies_by_type() {
        let dxf = build_dxf(&[
            DxfShape::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
            },
            DxfShape::Line {
                x1: 0.0,
                y1: 1.0,
                x2: 1.0,
                y2: 0.0,
            },
            DxfShape::Circle {
                cx: 0.0,
                cy: 0.0,
                r: 5.0,
            },
        ]);
        let counts = count_dxf_entities(&dxf);
        assert_eq!(counts.get("LINE"), Some(&2));
        assert_eq!(counts.get("CIRCLE"), Some(&1));
        assert!(!counts.contains_key("SECTION"), "结构段不应计入实体");
    }

    #[test]
    fn shape_from_value_rejects_unknown_type() {
        assert!(
            DxfShape::from_value(&json!({"type": "line", "x1": 0.0})).is_none(),
            "缺参数应拒绝"
        );
        assert!(
            DxfShape::from_value(&json!({"type": "arc"})).is_none(),
            "未知类型应拒绝"
        );
        assert!(
            DxfShape::from_value(&json!({"type": "circle", "cx": 1.0, "cy": 2.0, "r": 3.0}))
                .is_some()
        );
    }
}
