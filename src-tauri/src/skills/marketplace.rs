//! 技能市场（本地打包目录）
//!
//! 与早期原型的 Skill Hall 对齐：先搜索、再安装官方打包技能。
//! 这里只提供**随应用打包的官方技能**（bundled），不接远程市场，
//! 因此不存在「任意下载并执行远端代码」的路径——安装一个市场技能等于
//! 把内置的、经过审阅的 skill.json + 入口脚本物化到本地技能目录。

use serde::{Deserialize, Serialize};

use super::{SkillDefinition, SkillParameter, SkillType};

/// 市场目录项（给人和搜索用的元数据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceSkill {
    /// 稳定技能 id（skill_marketplace_install 用它）
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub author: String,
    pub version: String,
    /// 是否随应用打包、可直接安装
    pub installable: bool,
}

/// 打包技能的完整定义 + 入口脚本内容。
struct BundledPackage {
    definition: SkillDefinition,
    script: &'static str,
}

/// 内置官方技能目录。
///
/// 每个条目都对应一个纯计算 / 只读本地的能力，避免引入有外部副作用的
/// 代码。入口脚本是 PowerShell，与 `SkillExecutor::execute_local` 的
/// `ps1` 分支对齐。
fn packages() -> Vec<BundledPackage> {
    vec![
        BundledPackage {
            definition: SkillDefinition {
                id: "time-now".to_string(),
                name: "当前时间".to_string(),
                description: "返回当前日期、时间、时区与 Unix 时间戳。".to_string(),
                version: "1.0.0".to_string(),
                author: "E-Assisant Official".to_string(),
                skill_type: SkillType::Local,
                triggers: vec![
                    "现在几点".to_string(),
                    "当前时间".to_string(),
                    "时间".to_string(),
                ],
                parameters: vec![SkillParameter {
                    name: "format".to_string(),
                    description: "输出格式：full（完整）或 iso（ISO 8601）".to_string(),
                    required: false,
                    param_type: "string".to_string(),
                    default: Some(serde_json::Value::String("full".to_string())),
                }],
                entry_point: "run.ps1".to_string(),
                permissions: vec!["read".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: std::path::PathBuf::new(),
            },
            script: r#"param([string]$format = "full")
$now = Get-Date
$utc = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
if ($format -eq "iso") {
    $out = $now.ToString("o")
} else {
    $out = "time=$($now.ToString('yyyy-MM-dd HH:mm:ss'))|timezone=$([TimeZoneInfo]::Local.Id)|unix_seconds=$utc"
}
Write-Output $out
"#,
        },
        BundledPackage {
            definition: SkillDefinition {
                id: "random-number".to_string(),
                name: "随机数".to_string(),
                description: "在指定范围内生成一个随机整数，可生成多个。".to_string(),
                version: "1.0.0".to_string(),
                author: "E-Assisant Official".to_string(),
                skill_type: SkillType::Local,
                triggers: vec![
                    "随机数".to_string(),
                    "抽签".to_string(),
                    "random".to_string(),
                ],
                parameters: vec![
                    SkillParameter {
                        name: "min".to_string(),
                        description: "最小值（含）".to_string(),
                        required: false,
                        param_type: "integer".to_string(),
                        default: Some(serde_json::Value::from(1)),
                    },
                    SkillParameter {
                        name: "max".to_string(),
                        description: "最大值（含）".to_string(),
                        required: false,
                        param_type: "integer".to_string(),
                        default: Some(serde_json::Value::from(100)),
                    },
                    SkillParameter {
                        name: "count".to_string(),
                        description: "生成个数".to_string(),
                        required: false,
                        param_type: "integer".to_string(),
                        default: Some(serde_json::Value::from(1)),
                    },
                ],
                entry_point: "run.ps1".to_string(),
                permissions: vec!["read".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: std::path::PathBuf::new(),
            },
            script: r#"param([int]$min = 1, [int]$max = 100, [int]$count = 1)
if ($min -gt $max) { $t = $min; $min = $max; $max = $t }
if ($count -lt 1) { $count = 1 }
if ($count -gt 100) { $count = 100 }
$rng = [System.Random]::new()
$vals = @()
for ($i = 0; $i -lt $count; $i++) { $vals += $rng.Next($min, $max + 1) }
Write-Output ("values=" + ($vals -join ",") + "|min=$min|max=$max|count=$count")
"#,
        },
        BundledPackage {
            definition: SkillDefinition {
                id: "text-stats".to_string(),
                name: "文本统计".to_string(),
                description: "统计一段文本的字符数、字数（按空白切分）与行数。".to_string(),
                version: "1.0.0".to_string(),
                author: "E-Assisant Official".to_string(),
                skill_type: SkillType::Local,
                triggers: vec![
                    "字数".to_string(),
                    "统计".to_string(),
                    "text stats".to_string(),
                ],
                parameters: vec![SkillParameter {
                    name: "text".to_string(),
                    description: "要统计的文本".to_string(),
                    required: true,
                    param_type: "string".to_string(),
                    default: None,
                }],
                entry_point: "run.ps1".to_string(),
                permissions: vec!["read".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: std::path::PathBuf::new(),
            },
            script: r#"param([string]$text = "")
$chars = $text.Length
$lines = if ($text -eq "") { 0 } else { ($text -split "`n").Count }
$words = if ($text.Trim() -eq "") { 0 } else { ($text -split "\s+").Count }
Write-Output ("chars=$chars|words=$words|lines=$lines")
"#,
        },
    ]
}

/// 市场目录（元数据）。
pub fn catalog() -> Vec<MarketplaceSkill> {
    packages()
        .into_iter()
        .map(|p| MarketplaceSkill {
            id: p.definition.id.clone(),
            name: p.definition.name.clone(),
            description: p.definition.description.clone(),
            category: "utility".to_string(),
            author: p.definition.author.clone(),
            version: p.definition.version.clone(),
            installable: true,
        })
        .collect()
}

/// 按 id 查目录项。
pub fn find(id: &str) -> Option<MarketplaceSkill> {
    catalog().into_iter().find(|s| s.id == id)
}

/// 搜索目录（按名称/描述/类别做大小写不敏感子串匹配；空查询返回全部）。
pub fn search(query: &str) -> Vec<MarketplaceSkill> {
    let q = query.trim().to_lowercase();
    catalog()
        .into_iter()
        .filter(|s| {
            q.is_empty()
                || s.name.to_lowercase().contains(&q)
                || s.description.to_lowercase().contains(&q)
                || s.category.to_lowercase().contains(&q)
                || s.id.to_lowercase().contains(&q)
        })
        .collect()
}

/// 取出可安装的打包技能（定义 + 入口脚本）。
///
/// 仅随应用打包的官方技能可安装；`None` 表示该 id 不在官方目录里，
/// 不能凭空安装。
pub fn bundled_package(id: &str) -> Option<(SkillDefinition, &'static str)> {
    packages()
        .into_iter()
        .find(|p| p.definition.id == id)
        .map(|p| (p.definition, p.script))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_searchable_and_installable() {
        assert!(catalog().len() >= 3);
        assert!(search("时间").iter().any(|s| s.id == "time-now"));
        assert!(search("").len() == catalog().len());
        let (def, script) = bundled_package("random-number").expect("random-number 应可安装");
        assert_eq!(def.entry_point, "run.ps1");
        assert!(!script.contains("Write-Output") || script.contains("values="));
        assert!(bundled_package("no-such-skill").is_none());
    }

    #[test]
    fn find_returns_none_for_unknown_id() {
        assert!(find("unknown-id-xyz").is_none());
        assert!(find("time-now").is_some());
    }
}
