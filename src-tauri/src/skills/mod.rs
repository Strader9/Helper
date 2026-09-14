//! V20 Skills 技能系统
//!
//! 可扩展的技能包机制，用户/开发者可以编写自定义技能（类似插件），
//! AI 能发现并调用技能。
//!
//! 架构：
//! - SkillDefinition: 技能元数据定义
//! - SkillLoader: 从本地目录加载技能
//! - SkillExecutor: 执行技能脚本（PowerShell/Python/命令行）
//! - SkillTool: 将技能包装为 AgentTool，动态注册到 ToolRegistry
//! - 3 个内置示例技能：天气查询、翻译、文件整理

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::time::{timeout, Duration};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 数据模型
// ============================================================

/// 技能类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillType {
    /// 内置技能（硬编码，不需要外部文件）
    BuiltIn,
    /// 本地技能（从 %APPDATA%/pc-guardian/skills/ 加载）
    Local,
    /// 远程技能（从 URL 加载，预留）
    Remote,
}

impl SkillType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillType::BuiltIn => "builtin",
            SkillType::Local => "local",
            SkillType::Remote => "remote",
        }
    }
}

/// 技能参数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(rename = "type", default = "default_param_type")]
    pub param_type: String,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

fn default_param_type() -> String {
    "string".to_string()
}

/// 技能定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(rename = "type", default = "default_skill_type")]
    pub skill_type: SkillType,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<SkillParameter>,
    /// 入口脚本路径（相对于技能目录），内置技能为空
    #[serde(default)]
    pub entry_point: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub installed_at: String,
    /// 技能目录路径（本地技能），内置技能为空
    #[serde(skip)]
    pub skill_dir: PathBuf,
}

fn default_version() -> String { "1.0.0".to_string() }
fn default_skill_type() -> SkillType { SkillType::Local }
fn default_true() -> bool { true }

// ============================================================
// 技能加载器
// ============================================================

/// 技能加载器
pub struct SkillLoader;

impl SkillLoader {
    /// 获取技能根目录
    pub fn skills_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pc-guardian")
            .join("skills")
    }

    /// 确保技能目录存在
    pub fn ensure_dir() -> AppResult<PathBuf> {
        let dir = Self::skills_root();
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(format!("创建技能目录失败: {}", e)))?;
        Ok(dir)
    }

    /// 从本地目录加载所有技能
    pub fn load_all() -> Vec<SkillDefinition> {
        let root = match Self::ensure_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[Skills] 技能目录初始化失败: {}", e);
                return Vec::new();
            }
        };

        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut skills = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                match Self::load_from_dir(&path) {
                    Ok(skill) => skills.push(skill),
                    Err(e) => eprintln!("[Skills] 加载技能 {:?} 失败: {}", path, e),
                }
            }
        }
        skills
    }

    /// 从单个目录加载技能
    pub fn load_from_dir(dir: &Path) -> AppResult<SkillDefinition> {
        let skill_json = dir.join("skill.json");
        if !skill_json.exists() {
            return Err(AppError::InvalidArgument(format!(
                "技能目录缺少 skill.json: {:?}", dir
            )));
        }

        let content = std::fs::read_to_string(&skill_json)
            .map_err(|e| AppError::Internal(format!("读取 skill.json 失败: {}", e)))?;

        let mut skill: SkillDefinition = serde_json::from_str(&content)
            .map_err(|e| AppError::Internal(format!("解析 skill.json 失败: {}", e)))?;

        // 验证必填字段
        if skill.id.is_empty() {
            return Err(AppError::InvalidArgument("技能 id 不能为空".to_string()));
        }
        if skill.name.is_empty() {
            return Err(AppError::InvalidArgument("技能 name 不能为空".to_string()));
        }

        skill.skill_dir = dir.to_path_buf();
        skill.skill_type = SkillType::Local;
        if skill.installed_at.is_empty() {
            skill.installed_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        }

        Ok(skill)
    }

    /// 安装技能（从目录或 zip）
    pub fn install_from_dir(source: &Path) -> AppResult<SkillDefinition> {
        let skill = Self::load_from_dir(source)?;
        let root = Self::ensure_dir()?;
        let target = root.join(&skill.id);

        if target.exists() {
            return Err(AppError::InvalidArgument(format!(
                "技能 {} 已存在", skill.id
            )));
        }

        // 复制目录
        copy_dir_all(source, &target)?;

        let mut installed = Self::load_from_dir(&target)?;
        installed.installed_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        Ok(installed)
    }

    /// 卸载技能
    pub fn uninstall(skill_id: &str) -> AppResult<()> {
        let root = Self::skills_root();
        let target = root.join(skill_id);
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| AppError::Internal(format!("删除技能目录失败: {}", e)))?;
        }
        Ok(())
    }
}

/// 递归复制目录
fn copy_dir_all(src: &Path, dst: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

// ============================================================
// 技能执行引擎
// ============================================================

/// 技能执行结果
#[derive(Debug, Serialize)]
pub struct SkillExecutionResult {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// 技能执行器
pub struct SkillExecutor;

impl SkillExecutor {
    /// 执行本地技能脚本
    pub async fn execute_local(
        skill: &SkillDefinition,
        args: &serde_json::Value,
    ) -> AppResult<SkillExecutionResult> {
        // 参数校验
        if let Err(e) = Self::validate_params(skill, args) {
            return Ok(SkillExecutionResult {
                success: false,
                output: String::new(),
                error: Some(e),
                duration_ms: 0,
            });
        }

        let entry = &skill.entry_point;
        if entry.is_empty() {
            return Ok(SkillExecutionResult {
                success: false,
                output: String::new(),
                error: Some("技能未指定入口脚本".to_string()),
                duration_ms: 0,
            });
        }

        let script_path = skill.skill_dir.join(entry);
        if !script_path.exists() {
            return Ok(SkillExecutionResult {
                success: false,
                output: String::new(),
                error: Some(format!("入口脚本不存在: {:?}", script_path)),
                duration_ms: 0,
            });
        }

        // 构建参数列表
        let args_vec = build_script_args(args);

        let start = std::time::Instant::now();

        // 根据扩展名选择执行方式
        let result = timeout(Duration::from_secs(30), async move {
            let ext = script_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let output = match ext {
                "ps1" => Command::new("powershell")
                    .arg("-NoProfile").arg("-NonInteractive").arg("-WindowStyle").arg("Hidden")
                    .arg("-ExecutionPolicy").arg("Bypass")
                    .arg("-File").arg(&script_path)
                    .args(&args_vec)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output(),
                "py" => Command::new("python")
                    .arg(&script_path)
                    .args(&args_vec)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output(),
                "bat" | "cmd" => Command::new(&script_path)
                    .args(&args_vec)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output(),
                _ => Command::new(&script_path)
                    .args(&args_vec)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output(),
            };

            match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                    if o.status.success() {
                        Ok(stdout)
                    } else {
                        Err(format!("脚本执行失败: {}", if stderr.is_empty() { stdout } else { stderr }))
                    }
                }
                Err(e) => Err(format!("启动脚本失败: {}", e)),
            }
        }).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(output)) => Ok(SkillExecutionResult {
                success: true,
                output,
                error: None,
                duration_ms,
            }),
            Ok(Err(e)) => Ok(SkillExecutionResult {
                success: false,
                output: String::new(),
                error: Some(e),
                duration_ms,
            }),
            Err(_) => Ok(SkillExecutionResult {
                success: false,
                output: String::new(),
                error: Some("技能执行超时（30秒）".to_string()),
                duration_ms,
            }),
        }
    }

    /// 校验参数
    fn validate_params(skill: &SkillDefinition, args: &serde_json::Value) -> Result<(), String> {
        for param in &skill.parameters {
            if param.required && !args.get(&param.name).is_some() {
                if param.default.is_none() {
                    return Err(format!("缺少必填参数: {}", param.name));
                }
            }
        }
        Ok(())
    }
}

/// 将 JSON 参数转为命令行参数列表
fn build_script_args(args: &serde_json::Value) -> Vec<String> {
    let mut vec = Vec::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            vec.push(format!("--{}", k));
            vec.push(match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
    }
    vec
}

// ============================================================
// 内置技能实现
// ============================================================

/// 内置技能执行函数类型
type BuiltInFn = fn(&serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<serde_json::Value>> + Send>>;

/// 内置技能注册表
struct BuiltInSkill {
    definition: SkillDefinition,
    handler: BuiltInFn,
}

/// 获取所有内置技能
fn builtin_skills() -> Vec<BuiltInSkill> {
    vec![
        BuiltInSkill {
            definition: SkillDefinition {
                id: "weather".to_string(),
                name: "天气查询".to_string(),
                description: "查询指定城市的当前天气信息，包括温度、湿度、风速和天气状况。使用 Open-Meteo 免费 API，无需 API Key。".to_string(),
                version: "1.0.0".to_string(),
                author: "PC Guardian".to_string(),
                skill_type: SkillType::BuiltIn,
                triggers: vec!["天气".to_string(), "weather".to_string(), "气温".to_string()],
                parameters: vec![
                    SkillParameter {
                        name: "city".to_string(),
                        description: "城市名称（中文或英文，如 北京、Shanghai、Tokyo）".to_string(),
                        required: true,
                        param_type: "string".to_string(),
                        default: None,
                    },
                ],
                entry_point: String::new(),
                permissions: vec!["network".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: PathBuf::new(),
            },
            handler: weather_handler,
        },
        BuiltInSkill {
            definition: SkillDefinition {
                id: "translate".to_string(),
                name: "文本翻译".to_string(),
                description: "将文本翻译为目标语言。使用 MyMemory 免费翻译 API，支持中英日韩等多种语言。".to_string(),
                version: "1.0.0".to_string(),
                author: "PC Guardian".to_string(),
                skill_type: SkillType::BuiltIn,
                triggers: vec!["翻译".to_string(), "translate".to_string(), "英文".to_string()],
                parameters: vec![
                    SkillParameter {
                        name: "text".to_string(),
                        description: "要翻译的文本".to_string(),
                        required: true,
                        param_type: "string".to_string(),
                        default: None,
                    },
                    SkillParameter {
                        name: "target_lang".to_string(),
                        description: "目标语言代码（en=英语, ja=日语, ko=韩语, zh=中文, fr=法语, de=德语）".to_string(),
                        required: false,
                        param_type: "string".to_string(),
                        default: Some(serde_json::Value::String("en".to_string())),
                    },
                ],
                entry_point: String::new(),
                permissions: vec!["network".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: PathBuf::new(),
            },
            handler: translate_handler,
        },
        BuiltInSkill {
            definition: SkillDefinition {
                id: "file_organizer".to_string(),
                name: "文件整理".to_string(),
                description: "整理指定目录中的文件，按文件类型分类移动到子文件夹（图片、文档、视频、音频、压缩包、代码、其他）。".to_string(),
                version: "1.0.0".to_string(),
                author: "PC Guardian".to_string(),
                skill_type: SkillType::BuiltIn,
                triggers: vec!["整理文件".to_string(), "文件整理".to_string(), "organize".to_string()],
                parameters: vec![
                    SkillParameter {
                        name: "directory".to_string(),
                        description: "要整理的目录路径".to_string(),
                        required: true,
                        param_type: "string".to_string(),
                        default: None,
                    },
                ],
                entry_point: String::new(),
                permissions: vec!["filesystem:write".to_string()],
                enabled: true,
                installed_at: String::new(),
                skill_dir: PathBuf::new(),
            },
            handler: file_organizer_handler,
        },
    ]
}

// --- 天气查询 ---
fn weather_handler(args: &serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<serde_json::Value>> + Send>> {
    let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Box::pin(async move {
        if city.is_empty() {
            return Err(AppError::InvalidArgument("city 参数必填".to_string()));
        }

        // 使用 Open-Meteo 地理编码 API 获取坐标
        let geo_url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=zh&format=json",
            urlencoding::encode(&city)
        );
        let client = reqwest::Client::new();
        let geo_resp = client.get(&geo_url).send().await
            .map_err(|e| AppError::Network(e))?;
        let geo_json: serde_json::Value = geo_resp.json().await
            .map_err(|e| AppError::Internal(format!("地理编码解析失败: {}", e)))?;

        let results = geo_json.get("results").and_then(|v| v.as_array());
        let (lat, lon, city_name) = match results.and_then(|r| r.first()) {
            Some(r) => (
                r.get("latitude").and_then(|v| v.as_f64()).unwrap_or(39.9),
                r.get("longitude").and_then(|v| v.as_f64()).unwrap_or(116.4),
                r.get("name").and_then(|v| v.as_str()).unwrap_or(&city).to_string(),
            ),
            None => (39.9, 116.4, city.clone()),
        };

        // 获取天气数据
        let weather_url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,relative_humidity_2m,wind_speed_10m,weather_code&timezone=auto",
            lat, lon
        );
        let weather_resp = client.get(&weather_url).send().await
            .map_err(|e| AppError::Network(e))?;
        let weather_json: serde_json::Value = weather_resp.json().await
            .map_err(|e| AppError::Internal(format!("天气数据解析失败: {}", e)))?;

        let current = weather_json.get("current").ok_or_else(|| AppError::Internal("天气数据为空".to_string()))?;
        let temp = current.get("temperature_2m").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let humidity = current.get("relative_humidity_2m").and_then(|v| v.as_i64()).unwrap_or(0);
        let wind = current.get("wind_speed_10m").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let code = current.get("weather_code").and_then(|v| v.as_i64()).unwrap_or(0);

        let weather_desc = weather_code_to_text(code);

        Ok(serde_json::json!({
            "city": city_name,
            "temperature_c": temp,
            "humidity_percent": humidity,
            "wind_speed_kmh": wind,
            "weather": weather_desc,
            "weather_code": code
        }))
    })
}

fn weather_code_to_text(code: i64) -> &'static str {
    match code {
        0 => "晴",
        1..=3 => "多云",
        45 | 48 => "雾",
        51..=57 => "毛毛雨",
        61..=67 => "雨",
        71..=77 => "雪",
        80..=82 => "阵雨",
        85..=86 => "阵雪",
        95 => "雷暴",
        96..=99 => "雷暴伴冰雹",
        _ => "未知",
    }
}

// --- 翻译 ---
fn translate_handler(args: &serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<serde_json::Value>> + Send>> {
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let target = args.get("target_lang").and_then(|v| v.as_str()).unwrap_or("en").to_string();
    Box::pin(async move {
        if text.is_empty() {
            return Err(AppError::InvalidArgument("text 参数必填".to_string()));
        }

        // MyMemory 免费翻译 API
        // 源语言自动检测，目标语言由参数指定
        let langpair = format!("auto|{}", target);
        let url = format!(
            "https://api.mymemory.translated.net/get?q={}&langpair={}",
            urlencoding::encode(&text),
            urlencoding::encode(&langpair)
        );

        let client = reqwest::Client::new();
        let resp = client.get(&url).send().await
            .map_err(|e| AppError::Network(e))?;
        let json: serde_json::Value = resp.json().await
            .map_err(|e| AppError::Internal(format!("翻译响应解析失败: {}", e)))?;

        let translated = json
            .get("responseData")
            .and_then(|r| r.get("translatedText"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if translated.is_empty() {
            return Err(AppError::Internal("翻译结果为空".to_string()));
        }

        Ok(serde_json::json!({
            "original": text,
            "translated": translated,
            "target_language": target
        }))
    })
}

// --- 文件整理 ---
fn file_organizer_handler(args: &serde_json::Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<serde_json::Value>> + Send>> {
    let directory = args.get("directory").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Box::pin(async move {
        if directory.is_empty() {
            return Err(AppError::InvalidArgument("directory 参数必填".to_string()));
        }

        let dir = Path::new(&directory);
        if !dir.exists() || !dir.is_dir() {
            return Err(AppError::InvalidArgument(format!("目录不存在: {}", directory)));
        }

        // 分类映射
        let categories: HashMap<&str, &[&str]> = [
            ("图片", &["jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "ico", "tiff"] as &[&str]),
            ("文档", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "odt", "csv"] as &[&str]),
            ("视频", &["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm", "m4v"] as &[&str]),
            ("音频", &["mp3", "wav", "flac", "aac", "ogg", "wma", "m4a"] as &[&str]),
            ("压缩包", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"] as &[&str]),
            ("代码", &["rs", "py", "js", "ts", "java", "c", "cpp", "h", "go", "rb", "php", "html", "css", "json", "xml", "yaml", "yml", "sh", "bat", "ps1"] as &[&str]),
            ("可执行", &["exe", "msi", "app", "dmg", "deb", "rpm"] as &[&str]),
        ].into_iter().collect();

        let mut moved: HashMap<String, Vec<String>> = HashMap::new();
        let mut count = 0u32;

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();

                // 找到分类
                let category = categories.iter()
                    .find(|(_, exts)| exts.contains(&ext.as_str()))
                    .map(|(cat, _)| *cat)
                    .unwrap_or("其他");

                let target_dir = dir.join(category);
                std::fs::create_dir_all(&target_dir)?;

                let file_name = path.file_name().unwrap();
                let target_path = target_dir.join(file_name);

                // 处理重名
                let final_target = if target_path.exists() {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
                    let new_name = format!("{}_copy.{}", stem, ext);
                    target_dir.join(new_name)
                } else {
                    target_path
                };

                match std::fs::rename(&path, &final_target) {
                    Ok(_) => {
                        moved.entry(category.to_string())
                            .or_default()
                            .push(file_name.to_string_lossy().to_string());
                        count += 1;
                    }
                    Err(e) => eprintln!("[FileOrganizer] 移动 {:?} 失败: {}", path, e),
                }
            }
        }

        Ok(serde_json::json!({
            "directory": directory,
            "files_moved": count,
            "categories": moved,
            "message": format!("已整理 {} 个文件", count)
        }))
    })
}

// ============================================================
// SkillTool — 将技能包装为 AgentTool
// ============================================================

/// 技能工具包装器
///
/// 将 SkillDefinition 包装为实现 AgentTool trait 的工具，
/// 使技能可以动态注册到 ToolRegistry。
pub struct SkillTool {
    definition: SkillDefinition,
    handler: Option<BuiltInFn>,
}

impl SkillTool {
    /// 从技能定义创建工具（本地技能）
    pub fn from_definition(def: SkillDefinition) -> Self {
        Self { definition: def, handler: None }
    }

    /// 从内置技能创建工具
    fn from_builtin(skill: BuiltInSkill) -> Self {
        Self { definition: skill.definition, handler: Some(skill.handler) }
    }

    pub fn definition(&self) -> &SkillDefinition {
        &self.definition
    }
}

#[async_trait]
impl AgentTool for SkillTool {
    fn name(&self) -> &'static str {
        // 技能工具名称格式：skill_<id>
        // 由于 AgentTool::name 返回 &'static str，我们用 Box::leak 泄漏
        // 这在应用生命周期内是安全的
        Box::leak(format!("skill_{}", self.definition.id).into_boxed_str())
    }

    fn description(&self) -> &'static str {
        Box::leak(self.definition.description.clone().into_boxed_str())
    }

    fn parameters(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();

        for param in &self.definition.parameters {
            let prop = serde_json::json!({
                "type": param.param_type,
                "description": param.description,
            });
            props.insert(param.name.clone(), prop);
            if param.required {
                required.push(param.name.clone());
            }
        }

        serde_json::json!({
            "type": "object",
            "properties": props,
            "required": required
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 根据权限判断风险等级
        let perms = &self.definition.permissions;
        if perms.iter().any(|p| p.contains("write") || p.contains("execute") || p.contains("system")) {
            RiskLevel::Medium
        } else if perms.iter().any(|p| p.contains("network") || p.contains("read")) {
            RiskLevel::Low
        } else {
            RiskLevel::Safe
        }
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        if let Some(handler) = self.handler {
            // 内置技能
            match handler(&args).await {
                Ok(data) => Ok(ToolResult::ok(data)),
                Err(e) => Ok(ToolResult::err("SKILL_ERROR", &e.to_string())),
            }
        } else {
            // 本地技能
            let result = SkillExecutor::execute_local(&self.definition, &args).await?;
            if result.success {
                Ok(ToolResult::ok(serde_json::json!({
                    "output": result.output,
                    "duration_ms": result.duration_ms
                })))
            } else {
                Ok(ToolResult::err("SKILL_EXECUTION_FAILED", &result.error.unwrap_or_else(|| "未知错误".to_string())))
            }
        }
    }
}

// ============================================================
// 技能管理器（全局）
// ============================================================

/// 技能管理器
pub struct SkillManager {
    skills: Mutex<Vec<SkillDefinition>>,
}

use tokio::sync::Mutex;

impl SkillManager {
    /// 创建技能管理器并加载所有技能
    pub async fn new() -> Self {
        let mut skills = Vec::new();

        // 加载内置技能
        for bs in builtin_skills() {
            skills.push(bs.definition);
        }

        // 加载本地技能
        let local = SkillLoader::load_all();
        skills.extend(local);

        Self {
            skills: Mutex::new(skills),
        }
    }

    /// 获取所有技能
    pub async fn list_skills(&self) -> Vec<SkillDefinition> {
        self.skills.lock().await.clone()
    }

    /// 获取技能详情
    pub async fn get_skill(&self, id: &str) -> Option<SkillDefinition> {
        self.skills.lock().await.iter().find(|s| s.id == id).cloned()
    }

    /// 启用/禁用技能
    pub async fn toggle_skill(&self, id: &str, enabled: bool) -> AppResult<()> {
        let mut skills = self.skills.lock().await;
        if let Some(skill) = skills.iter_mut().find(|s| s.id == id) {
            skill.enabled = enabled;
            Ok(())
        } else {
            Err(AppError::ToolNotFound(format!("技能不存在: {}", id)))
        }
    }

    /// 安装技能
    pub async fn install_skill(&self, source_dir: &str) -> AppResult<SkillDefinition> {
        let skill = SkillLoader::install_from_dir(Path::new(source_dir))?;
        let mut skills = self.skills.lock().await;
        skills.push(skill.clone());
        Ok(skill)
    }

    /// 卸载技能
    pub async fn uninstall_skill(&self, id: &str) -> AppResult<()> {
        // 内置技能不可卸载
        {
            let skills = self.skills.lock().await;
            if let Some(s) = skills.iter().find(|s| s.id == id) {
                if s.skill_type == SkillType::BuiltIn {
                    return Err(AppError::InvalidArgument("内置技能不可卸载".to_string()));
                }
            }
        }
        SkillLoader::uninstall(id)?;
        let mut skills = self.skills.lock().await;
        skills.retain(|s| s.id != id);
        Ok(())
    }

    /// 将所有启用的技能注册为工具
    pub fn register_tools(&self, registry: &mut crate::tools::ToolRegistry) {
        // 注册内置技能
        for bs in builtin_skills() {
            if bs.definition.enabled {
                registry.register(std::sync::Arc::new(SkillTool::from_builtin(bs)));
            }
        }

        // 注册本地技能
        let local_skills = SkillLoader::load_all();
        for skill in local_skills {
            if skill.enabled {
                registry.register(std::sync::Arc::new(SkillTool::from_definition(skill)));
            }
        }
    }
}

// ============================================================
// 全局单例
// ============================================================

static SKILL_MANAGER: OnceLock<SkillManager> = OnceLock::new();

/// 注册所有启用的技能为工具（在 ToolRegistry 初始化时调用）
pub fn register_skill_tools(registry: &mut crate::tools::ToolRegistry) {
    // 注册内置技能
    for bs in builtin_skills() {
        if bs.definition.enabled {
            registry.register(Arc::new(SkillTool::from_builtin(bs)));
        }
    }

    // 注册本地技能
    let local_skills = SkillLoader::load_all();
    for skill in local_skills {
        if skill.enabled {
            registry.register(Arc::new(SkillTool::from_definition(skill)));
        }
    }
}

/// 初始化全局技能管理器（在应用启动时调用）
pub async fn init_global() -> &'static SkillManager {
    if SKILL_MANAGER.get().is_none() {
        let mgr = SkillManager::new().await;
        let _ = SKILL_MANAGER.set(mgr);
    }
    SKILL_MANAGER.get().unwrap()
}

/// 获取全局技能管理器
pub fn global() -> Option<&'static SkillManager> {
    SKILL_MANAGER.get()
}
