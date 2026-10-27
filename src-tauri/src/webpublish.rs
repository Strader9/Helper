//! 把生成好的单文件网页发布到公网，拿到一个可以打开的链接。
//!
//! 两条通道，谁配了令牌就走谁：
//!
//! - **GitHub Pages**（默认，免费）：只要一个 Personal Access Token。首次发布自动建仓库、
//!   开 Pages，之后每次都覆盖同一个仓库里的 `index.html`，所以链接是**固定**的。
//!   缺点是首次开启 Pages 后 GitHub 要构建几十秒，期间链接还打不开。
//! - **Netlify**：同样一个 Personal Access Token，秒级生效。走官方 Direct Deploy 的
//!   **文件摘要**方式：先 POST 一份"路径 → SHA-1"清单，Netlify 回哪些文件它手上没有，
//!   再逐个 PUT 上传。既不用把站点打包成 zip，重复发布同一份产物时一个字节都不用传。

use base64::Engine as _;
use serde::Serialize;
use sha1::{Digest, Sha1};

const NETLIFY_API: &str = "https://api.netlify.com/api/v1";

/// 发布结果。
#[derive(Debug, Clone, Serialize)]
pub struct PublishOutcome {
    /// 可以直接打开的链接。
    pub url: String,
    /// 平台显示名。
    pub provider: String,
    /// 站点 id。回填到设置里，下次发布复用同一个站点，链接就不会变。
    pub site: String,
}

/// 发布一份网页。
///
/// `site_id` 为空时自动建站；有值时直接往这个站点发布。
pub async fn publish_netlify(
    client: &reqwest::Client,
    token: &str,
    site_id: Option<&str>,
    file_name: &str,
    html: &str,
) -> Result<PublishOutcome, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("还没有填写 Netlify 访问令牌，请到「设置 → 网页发布」里填一个。".to_string());
    }

    let (site, url) = match site_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(existing) => (
            existing.to_string(),
            site_url(client, token, existing).await?,
        ),
        None => create_site(client, token, file_name).await?,
    };

    deploy_index(client, token, &site, html).await?;

    Ok(PublishOutcome {
        url,
        provider: "Netlify".to_string(),
        site,
    })
}

/// 查站点当前的访问地址（复用已有站点时用）。
async fn site_url(client: &reqwest::Client, token: &str, site_id: &str) -> Result<String, String> {
    let response = client
        .get(format!("{NETLIFY_API}/sites/{site_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| net_error("连接 Netlify 失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(explain(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Netlify 返回的站点信息无法解析：{error}"))?;
    Ok(clean_url(
        body["ssl_url"]
            .as_str()
            .or_else(|| body["url"].as_str())
            .unwrap_or_default(),
    ))
}

/// 建一个新站点。Netlify 要求站点名全局唯一，撞名就退回让它自动分配。
async fn create_site(
    client: &reqwest::Client,
    token: &str,
    file_name: &str,
) -> Result<(String, String), String> {
    let mut attempt_with_name = true;
    loop {
        let mut payload = serde_json::Map::new();
        if attempt_with_name {
            if let Some(name) = site_name(file_name) {
                payload.insert("name".to_string(), serde_json::Value::String(name));
            } else {
                attempt_with_name = false;
                continue;
            }
        }
        let response = client
            .post(format!("{NETLIFY_API}/sites"))
            .bearer_auth(token)
            .json(&serde_json::Value::Object(payload))
            .send()
            .await
            .map_err(|error| net_error("连接 Netlify 失败：", error))?
            .error_for_status();

        match response {
            Ok(response) => {
                let body: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|error| format!("Netlify 返回的站点信息无法解析：{error}"))?;
                let id = body["id"]
                    .as_str()
                    .ok_or("Netlify 没有返回站点 id。")?
                    .to_string();
                let url = clean_url(
                    body["ssl_url"]
                        .as_str()
                        .or_else(|| body["url"].as_str())
                        .unwrap_or_default(),
                );
                if url.is_empty() {
                    return Err("Netlify 没有返回站点地址。".to_string());
                }
                return Ok((id, url));
            }
            Err(error) => {
                // 站点名被占用了：去掉名字再试一次，让 Netlify 自己起一个。
                let status = error.status().map(|code| code.as_u16()).unwrap_or(0);
                if attempt_with_name && (status == 422 || status == 409) {
                    attempt_with_name = false;
                    continue;
                }
                let detail = error
                    .status()
                    .map(|code| explain(code.as_u16(), String::new()))
                    .unwrap_or_else(|| error.to_string());
                return Err(detail);
            }
        }
    }
}

/// 发布 index.html 到指定站点。
async fn deploy_index(
    client: &reqwest::Client,
    token: &str,
    site_id: &str,
    html: &str,
) -> Result<(), String> {
    let digest = format!("{:x}", Sha1::digest(html.as_bytes()));

    let response = client
        .post(format!("{NETLIFY_API}/sites/{site_id}/deploys"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "files": { "/index.html": digest } }))
        .send()
        .await
        .map_err(|error| net_error("连接 Netlify 失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(explain(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    let deploy: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Netlify 返回的部署信息无法解析：{error}"))?;

    let deploy_id = deploy["id"]
        .as_str()
        .ok_or("Netlify 没有返回部署 id。")?
        .to_string();

    // `required` 里没有我们这份摘要，说明 Netlify 手上已经有同样的文件了，不用再传。
    let needed = deploy["required"]
        .as_array()
        .map(|list| {
            list.iter()
                .any(|item| item.as_str() == Some(digest.as_str()))
        })
        // 老版本响应没有 required 字段时，保守地传一次。
        .unwrap_or(true);
    if !needed {
        return Ok(());
    }

    let response = client
        .put(format!(
            "{NETLIFY_API}/deploys/{deploy_id}/files/index.html"
        ))
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(html.to_string())
        .send()
        .await
        .map_err(|error| net_error("上传网页失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(explain(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    Ok(())
}

/// 把 reqwest 的错误连同底层原因一起说清楚。
///
/// `Display` 只给一句 "error sending request for url (...)"，真正的原因（DNS 解析、
/// TLS 握手、连接被拒、代理不可用）藏在 source 链里。这一句还会被界面截断，
/// 所以必须自己把链子撸出来，不然拿着"连接失败"四个字根本没法定位。
fn net_error(context: &str, error: reqwest::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !detail.contains(&text) {
            detail.push_str(" ← ");
            detail.push_str(&text);
        }
        source = cause.source();
    }

    let lowered = detail.to_ascii_lowercase();
    let hint =
        if lowered.contains("dns") || lowered.contains("lookup") || lowered.contains("resolve") {
            "（域名解析失败，检查网络或 DNS）"
        } else if lowered.contains("certificate")
            || lowered.contains("tls")
            || lowered.contains("handshake")
        {
            "（加密握手失败，通常是代理软件在做证书拦截）"
        } else if lowered.contains("proxy") {
            "（代理不可用；开着代理软件的话，确认系统代理已开启，或在设置里填写代理地址）"
        } else if lowered.contains("timed out") || lowered.contains("timeout") {
            "（连接超时）"
        } else if lowered.contains("refused") {
            "（连接被拒绝）"
        } else {
            ""
        };
    format!("{context}{detail}{hint}")
}

/// 用文件名推一个好记的站点名；推不出（比如纯中文名）就返回 None，交给 Netlify 自动分配。
fn site_name(file_name: &str) -> Option<String> {
    let stem = file_name
        .rsplit_once('.')
        .map(|(head, _)| head)
        .unwrap_or(file_name);
    let mut slug: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    // Netlify 要求 2–63 个字符。太短或没有可用的 ASCII 字母数字就不用它。
    if slug.len() < 4 || slug.len() > 40 || !slug.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    Some(format!("{slug}-{}", suffix()))
}

/// 4 位随机后缀，降低撞名的概率。
fn suffix() -> String {
    let value = uuid::Uuid::new_v4();
    value.simple().to_string()[..6].to_string()
}

fn clean_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// 把 HTTP 状态码翻译成人话。Netlify 的错误响应体一般是 `{"message": "..."}`。
fn explain(status: u16, body: String) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value["message"]
                .as_str()
                .or_else(|| value["error"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().chars().take(200).collect());

    let hint = match status {
        401 | 403 => "访问令牌无效或已过期，请到 Netlify 重新生成一个 Personal Access Token。",
        404 => "找不到这个站点，可能它已被删除；把设置里的站点 id 清空再发布一次即可自动重建。",
        422 => "Netlify 拒绝了这次请求，请检查令牌权限与站点设置。",
        429 => "发布太频繁了 —— Netlify 限制每分钟最多 3 次、每天 100 次。",
        _ => "Netlify 返回了错误。",
    };
    if detail.is_empty() {
        format!("{hint}（HTTP {status}）")
    } else {
        format!("{hint}（HTTP {status}：{detail}）")
    }
}

// ================================ GitHub Pages ================================

const GITHUB_API: &str = "https://api.github.com";

/// 没填仓库名时用的仓库。固定一个仓库，链接就永远是同一个。
pub const DEFAULT_GITHUB_REPO: &str = "e-assisant-web";

/// 发布到 GitHub Pages。
///
/// 全流程：查用户名 → 确保仓库存在 → 取回仓库里旧 `index.html` 的 sha（有就要带上，否则
/// GitHub 会拒绝覆盖）→ PUT 新内容 → 确保 Pages 已开启。链接形如
/// `https://{用户名}.github.io/{仓库名}`，只要仓库名不变，链接就不会变。
pub async fn publish_github_pages(
    client: &reqwest::Client,
    token: &str,
    repo: Option<&str>,
    html: &str,
) -> Result<PublishOutcome, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(
            "还没有填写 GitHub 访问令牌（Personal Access Token），请到「设置 → 网页发布」里填一个。"
                .to_string(),
        );
    }
    let repo = github_repo_name(repo);

    let login = github_login(client, token).await?;
    ensure_repo(client, token, &login, &repo).await?;
    let sha = content_sha(client, token, &login, &repo).await?;
    put_index(client, token, &login, &repo, html, sha.as_deref()).await?;
    ensure_pages(client, token, &login, &repo).await?;

    Ok(PublishOutcome {
        url: format!("https://{login}.github.io/{repo}"),
        provider: "GitHub Pages".to_string(),
        site: format!("{login}/{repo}"),
    })
}

/// 仓库名：用户填了就照用，只做小写化和非法字符清洗；没填就用默认仓库。
fn github_repo_name(configured: Option<&str>) -> String {
    let raw = configured.map(str::trim).filter(|name| !name.is_empty());
    let Some(raw) = raw else {
        return DEFAULT_GITHUB_REPO.to_string();
    };
    let slug: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        DEFAULT_GITHUB_REPO.to_string()
    } else {
        slug
    }
}

/// GitHub 要求带 User-Agent，缺了会直接 403；`Accept` 锁定官方 JSON 媒体类型。
fn github_request(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .header("User-Agent", "e-assisant")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(token)
}

/// 取当前令牌对应的用户名 —— 它就是 Pages 地址里的那一段。
async fn github_login(client: &reqwest::Client, token: &str) -> Result<String, String> {
    let response = github_request(client.get(format!("{GITHUB_API}/user")), token)
        .send()
        .await
        .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(github_error(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("GitHub 返回的用户信息无法解析：{error}"))?;
    body["login"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "GitHub 没有返回用户名。".to_string())
}

/// 仓库不存在就建一个公开仓库。已存在直接返回。
async fn ensure_repo(
    client: &reqwest::Client,
    token: &str,
    login: &str,
    repo: &str,
) -> Result<(), String> {
    let response = github_request(
        client.get(format!("{GITHUB_API}/repos/{login}/{repo}")),
        token,
    )
    .send()
    .await
    .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status.as_u16() != 404 {
        return Err(github_error(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }

    let response = github_request(client.post(format!("{GITHUB_API}/user/repos")), token)
        .json(&serde_json::json!({
            "name": repo,
            "private": false,
            "description": "由 E-Assisant 网页工坊发布的网页",
            "has_issues": false,
            "has_wiki": false,
            "has_projects": false,
        }))
        .send()
        .await
        .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(github_error(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    Ok(())
}

/// 仓库里已有 `index.html` 时，必须带上它的 sha 才能覆盖（GitHub 的乐观锁）。
async fn content_sha(
    client: &reqwest::Client,
    token: &str,
    login: &str,
    repo: &str,
) -> Result<Option<String>, String> {
    let response = github_request(
        client.get(format!(
            "{GITHUB_API}/repos/{login}/{repo}/contents/index.html"
        )),
        token,
    )
    .send()
    .await
    .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(github_error(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("GitHub 返回的文件信息无法解析：{error}"))?;
    Ok(body["sha"].as_str().map(str::to_string))
}

/// 把网页写进仓库的 `index.html`。空仓库上第一次写会自动创建 main 分支。
async fn put_index(
    client: &reqwest::Client,
    token: &str,
    login: &str,
    repo: &str,
    html: &str,
    sha: Option<&str>,
) -> Result<(), String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
    let mut payload = serde_json::json!({
        "message": "publish: 更新网页工坊产物",
        "content": encoded,
        "branch": "main",
    });
    if let Some(sha) = sha {
        payload["sha"] = serde_json::Value::String(sha.to_string());
    }

    let response = github_request(
        client.put(format!(
            "{GITHUB_API}/repos/{login}/{repo}/contents/index.html"
        )),
        token,
    )
    .json(&payload)
    .send()
    .await
    .map_err(|error| net_error("上传网页到 GitHub 失败：", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(github_error(
            status.as_u16(),
            response.text().await.unwrap_or_default(),
        ));
    }
    Ok(())
}

/// 确保仓库开了 Pages（source = main 分支根目录）。已经开过就跳过。
async fn ensure_pages(
    client: &reqwest::Client,
    token: &str,
    login: &str,
    repo: &str,
) -> Result<(), String> {
    let response = github_request(
        client.get(format!("{GITHUB_API}/repos/{login}/{repo}/pages")),
        token,
    )
    .send()
    .await
    .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    if response.status().is_success() {
        return Ok(());
    }

    let response = github_request(
        client.post(format!("{GITHUB_API}/repos/{login}/{repo}/pages")),
        token,
    )
    .json(&serde_json::json!({ "source": { "branch": "main", "path": "/" } }))
    .send()
    .await
    .map_err(|error| net_error("连接 GitHub 失败：", error))?;
    let status = response.status();
    // 409：别人（或上一次发布）刚好已经开了，不算错。
    if status.is_success() || status.as_u16() == 409 {
        return Ok(());
    }
    Err(github_error(
        status.as_u16(),
        response.text().await.unwrap_or_default(),
    ))
}

/// 把 GitHub 的错误响应翻译成人话。
fn github_error(status: u16, body: String) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().chars().take(200).collect());

    let hint = match status {
        401 => "GitHub 访问令牌无效或已过期，请到「设置 → 网页发布」重新填一个。",
        403 => "令牌权限不足，或者触发了 GitHub 的频率限制。新建令牌时请勾选 repo（细粒度令牌需要 Contents 和 Pages 的读写权限）。",
        404 => "找不到这个仓库，可能已被删除或改名。",
        422 => "GitHub 拒绝了这次请求，通常是仓库名已被占用，换一个仓库名再试。",
        _ => "GitHub 返回了错误。",
    };
    if detail.is_empty() {
        format!("{hint}（HTTP {status}）")
    } else {
        format!("{hint}（HTTP {status}：{detail}）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_readable_site_name_from_an_ascii_file_name() {
        let name = site_name("name-alchemy.html").unwrap();
        assert!(name.starts_with("name-alchemy-"), "得到 {name}");
        assert!(name.len() <= 63);
    }

    #[test]
    fn falls_back_to_an_anonymous_name_for_a_chinese_file_name() {
        assert!(site_name("人生选择题.html").is_none());
    }

    #[test]
    fn strips_the_trailing_slash_off_a_site_url() {
        assert_eq!(
            clean_url("https://sparkly-panda.netlify.app/"),
            "https://sparkly-panda.netlify.app"
        );
    }

    #[test]
    fn explains_an_expired_token() {
        let message = explain(401, String::new());
        assert!(message.contains("Personal Access Token"), "得到 {message}");
    }

    #[test]
    fn defaults_to_a_fixed_github_repo_so_the_link_never_changes() {
        assert_eq!(github_repo_name(None), DEFAULT_GITHUB_REPO);
        assert_eq!(github_repo_name(Some("   ")), DEFAULT_GITHUB_REPO);
    }

    #[test]
    fn sanitizes_a_hand_written_github_repo_name() {
        assert_eq!(github_repo_name(Some("My Web Page")), "my-web-page");
        assert_eq!(github_repo_name(Some("人生选择题")), DEFAULT_GITHUB_REPO);
        assert_eq!(github_repo_name(Some("--demo--")), "demo");
    }

    #[test]
    fn reuse_of_the_same_repo_keeps_the_pages_url_stable() {
        let first = github_repo_name(Some("e-assisant-web"));
        let second = github_repo_name(Some("e-assisant-web"));
        assert_eq!(first, second);
    }

    #[test]
    fn tells_the_user_which_scope_to_tick_for_a_github_token() {
        let message = github_error(403, String::new());
        assert!(message.contains("repo"), "得到 {message}");
    }
}
