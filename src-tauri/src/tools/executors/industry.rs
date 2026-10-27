//! 行业场景模板（industry 域）
//!
//! 借鉴早期原型的 business_line（电商/金融等垂直行业）设计，用 Rust 重写。
//! 每个场景 = 行业 + 场景名 + 领域提示词 + 推荐工具集，让助手在识别到
//! 「帮我写商品描述」「帮我分析财报」这类请求时，能加载对应领域的专业上下文，
//! 而不是用通用口径硬答。全部为静态数据，无外部依赖。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 一个行业场景模板。
pub struct IndustryScenario {
    pub id: &'static str,
    pub industry: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub prompt: &'static str,
    pub tools: &'static [&'static str],
}

/// 全量行业场景表。
pub fn scenarios() -> &'static [IndustryScenario] {
    &[
        IndustryScenario {
            id: "ecommerce_product",
            industry: "电商",
            name: "商品描述",
            description: "为商品写卖点突出、结构清晰的详情页/标题描述",
            prompt: "你是资深电商文案。写商品描述要抓卖点、讲人话、分点清晰，避免空洞堆形容词；标题含核心关键词。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "ecommerce_review",
            industry: "电商",
            name: "评价分析",
            description: "分析用户评价，提炼好评点与差评痛点",
            prompt: "你是电商运营。分析评价要归纳高频关键词、区分好评动因与差评痛点，给出可执行的改进建议。",
            tools: &["read_file", "write_file"],
        },
        IndustryScenario {
            id: "finance_report",
            industry: "金融",
            name: "财报解读",
            description: "解读财报/研报，提炼关键指标与风险",
            prompt: "你是财务分析。解读财报要关注营收、利润、现金流、负债四类核心指标，用数字说话，指出趋势与风险，不预测股价。",
            tools: &["read_file"],
        },
        IndustryScenario {
            id: "finance_budget",
            industry: "金融",
            name: "个人理财",
            description: "帮用户做收支规划与储蓄建议",
            prompt: "你是理财顾问。给建议要先问清收入、支出、目标，再做保守估计；不推荐具体金融产品，不承诺收益。",
            tools: &["remember", "recall"],
        },
        IndustryScenario {
            id: "legal_contract",
            industry: "法律",
            name: "合同要点",
            description: "审阅合同，标注关键条款与风险点",
            prompt: "你是法律助手。审合同要标出违约责任、付款条款、知识产权、争议解决、期限等关键点，并提醒风险；明确说明这不构成正式法律意见。",
            tools: &["read_file"],
        },
        IndustryScenario {
            id: "medical_health",
            industry: "医疗",
            name: "健康建议",
            description: "解答健康疑问，守住不诊断、不处方边界",
            prompt: "你是健康助手。只做科普与就医建议，不下诊断、不开处方；遇到症状描述主动提醒「请以医生诊断为准」，急重症建议立即就医。",
            tools: &[],
        },
        IndustryScenario {
            id: "education_tutoring",
            industry: "教育",
            name: "教学辅导",
            description: "讲解知识点、解题思路，引导学生自己得出答案",
            prompt: "你是耐心教师。讲解要循序渐进、举例具体，先给思路再给答案；引导学生自己思考，不直接甩结论。",
            tools: &[],
        },
        IndustryScenario {
            id: "marketing_copy",
            industry: "营销",
            name: "营销文案",
            description: "写有转化力的广告语、推广文案",
            prompt: "你是营销文案。文案要一句话抓眼球、卖点具象、有行动号召；针对目标人群说话，避免自嗨式空洞口号。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "seo_content",
            industry: "营销",
            name: "SEO 内容",
            description: "写利于搜索排名、关键词自然植入的内容",
            prompt: "你是 SEO 内容作者。围绕核心关键词写标题与小标题，关键词自然植入不堆砌，结构清晰便于阅读与抓取。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "software_review",
            industry: "技术",
            name: "代码评审",
            description: "评审代码，指出问题与改进点",
            prompt: "你是资深工程师。评审代码要聚焦正确性、可读性、边界条件、安全问题，给出具体位置与改法；区分「必须改」和「建议改」。",
            tools: &["read_file", "search_content", "search_files"],
        },
        IndustryScenario {
            id: "data_analysis",
            industry: "技术",
            name: "数据分析",
            description: "分析数据，给出有据可查的结论",
            prompt: "你是数据分析师。给结论要配数字与表格，说清数据来源与口径；发现异常先质疑数据再下结论，不编造。",
            tools: &["read_file", "read_xlsx"],
        },
        IndustryScenario {
            id: "translation",
            industry: "语言",
            name: "翻译润色",
            description: "翻译并润色，保留原意与语气",
            prompt: "你是专业译者。翻译要忠实原文、符合目标语言习惯；专业术语准确，语气与正式程度贴合场景。",
            tools: &[],
        },
        IndustryScenario {
            id: "resume_cover",
            industry: "求职",
            name: "简历求职",
            description: "优化简历与求职信，突出匹配度",
            prompt: "你是求职顾问。改简历要针对目标岗位提炼关键词、量化成果、删冗余；用 STAR 结构描述经历。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "real_estate",
            industry: "房产",
            name: "房产咨询",
            description: "分析房源、区位、交易要点",
            prompt: "你是房产顾问。分析房源要看区位、交通、配套、户型、价格、产权六个维度；客观陈述优劣，不替用户做决定。",
            tools: &["browser_open"],
        },
        IndustryScenario {
            id: "travel_plan",
            industry: "生活",
            name: "旅行规划",
            description: "做行程安排、交通住宿建议",
            prompt: "你是旅行规划师。做行程要结合天数、预算、季节、兴趣，安排合理不赶路；标注交通衔接与注意事项。",
            tools: &["browser_open", "create_calendar_event"],
        },
        IndustryScenario {
            id: "hr_recruiting",
            industry: "人力资源",
            name: "招聘面试",
            description: "写 JD、设计面试问题",
            prompt: "你是 HR 专家。写 JD 要岗位职责与任职要求分开、清晰可衡量；设计面试问题围绕岗位核心能力与真实场景。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "customer_service",
            industry: "客服",
            name: "客服话术",
            description: "写有温度、能解决问题的客服回复",
            prompt: "你是客服专家。回复要共情在前、解决在后，语气温和专业；能一次性说清就不让用户问第二次。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "social_media",
            industry: "营销",
            name: "社媒运营",
            description: "写各平台风格的帖子、互动文案",
            prompt: "你是社媒运营。写帖子要适配平台调性、抓开头三秒、有互动钩子；配话题标签，控制篇幅。",
            tools: &["write_clipboard"],
        },
        IndustryScenario {
            id: "research_writing",
            industry: "学术",
            name: "学术写作",
            description: "辅助论文结构、摘要、润色",
            prompt: "你是学术写作助手。辅助写作要逻辑严谨、引用规范、表述客观；帮整理结构而非代写，杜绝编造文献与数据。",
            tools: &["read_file", "write_file"],
        },
        IndustryScenario {
            id: "game_design",
            industry: "创作",
            name: "游戏设计",
            description: "设计玩法、关卡、数值框架",
            prompt: "你是游戏设计师。设计要明确核心玩法循环、目标体验与差异化；数值与关卡给具体方案而非空谈。",
            tools: &["write_file"],
        },
    ]
}

/// 按关键词检索场景：匹配 id / 行业 / 名称 / 描述里的任意词。
fn search_scenarios(query: &str) -> Vec<&'static IndustryScenario> {
    let lower = query.to_lowercase();
    let keywords: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == '，' || c == ',' || c == '、')
        .filter(|s| !s.is_empty())
        .collect();

    scenarios()
        .iter()
        .filter(|s| {
            let haystack =
                format!("{} {} {} {}", s.id, s.industry, s.name, s.description).to_lowercase();
            keywords.is_empty() || keywords.iter().any(|k| haystack.contains(k))
        })
        .collect()
}

// ============================================================
// industry_scenarios
// ============================================================

pub struct IndustryScenariosTool;

#[async_trait]
impl AgentTool for IndustryScenariosTool {
    fn name(&self) -> &'static str {
        "industry_scenarios"
    }

    fn description(&self) -> &'static str {
        "列出助手内置的行业场景模板（电商/金融/法律/医疗/教育/营销/技术等）。回答「你能帮我做什么专业的事」「有哪些行业场景」时用。要看具体某个场景的提示词与推荐工具，用 industry_guide。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let items: Vec<Value> = scenarios()
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "industry": s.industry,
                    "name": s.name,
                    "description": s.description,
                })
            })
            .collect();

        Ok(ToolResult::ok(json!({
            "count": items.len(),
            "scenarios": items,
        })))
    }
}

// ============================================================
// industry_guide
// ============================================================

pub struct IndustryGuideTool;

#[async_trait]
impl AgentTool for IndustryGuideTool {
    fn name(&self) -> &'static str {
        "industry_guide"
    }

    fn description(&self) -> &'static str {
        "按行业或场景关键词检索对应的领域提示词与推荐工具。用户提出「写商品描述」「分析财报」「审合同」这类专业请求时，先调这个拿到领域上下文，再用它指导回答。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "行业或场景关键词，如「电商」「财报」「合同」「文案」" }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let query = args["query"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("query is required".to_string()))?;

        let matches = search_scenarios(query);
        if matches.is_empty() {
            return Ok(ToolResult::ok(json!({
                "found": false,
                "query": query,
                "hint": "没有匹配的行业场景，可用 industry_scenarios 查看全部",
            })));
        }

        let items: Vec<Value> = matches
            .iter()
            .take(3)
            .map(|s| {
                json!({
                    "id": s.id,
                    "industry": s.industry,
                    "name": s.name,
                    "prompt": s.prompt,
                    "recommended_tools": s.tools,
                })
            })
            .collect();

        Ok(ToolResult::ok(json!({
            "found": true,
            "query": query,
            "matches": items,
            "count": items.len(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_are_non_empty_and_unique() {
        let all = scenarios();
        assert!(all.len() >= 20, "应有足够多的场景，实际 {}", all.len());
        let mut ids: Vec<&str> = all.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "场景 id 不应重复");
    }

    #[test]
    fn search_matches_by_industry_and_name() {
        let hits = search_scenarios("电商");
        assert!(hits.iter().any(|s| s.id == "ecommerce_product"));
        let hits2 = search_scenarios("财报");
        assert!(hits2.iter().any(|s| s.id == "finance_report"));
    }

    #[test]
    fn search_returns_empty_for_unknown() {
        assert!(search_scenarios("不存在场景xyz").is_empty());
    }
}
