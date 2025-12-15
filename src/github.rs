use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use octocrab::Octocrab;
use octocrab::models::{IssueState, Label, issues::Issue, pulls::PullRequest};
use octocrab::params;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::warn;

use crate::error::{Error, Result};
use crate::sets::LabelSpec;
use crate::settings::{
    BranchProtectionRule, BranchRestrictions, PullRequestSettings, RepoSettings,
    RequiredPullRequestReviews, RequiredStatusChecks, ReviewDismissalRestrictions, StatusCheck,
};

#[derive(Debug, Clone)]
pub struct RepoFile {
    pub sha: String,
    pub content: String,
}

#[derive(Clone)]
pub struct GithubClient {
    pub(crate) inner: Octocrab,
    pub(crate) org: String,
}

#[derive(Debug, Clone)]
pub struct LabelUsageEntry {
    pub number: u64,
    pub url: Option<String>,
    pub is_pr: bool,
}

impl GithubClient {
    pub fn new(token: &str, org: String) -> Result<Self> {
        let inner = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(Error::Octo)?;
        Ok(Self { inner, org })
    }

    pub async fn get_repo(&self, repo: &str) -> Result<octocrab::models::Repository> {
        self.inner
            .repos(&self.org, repo)
            .get()
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))
    }

    pub async fn list_repo_labels(&self, repo: &str) -> Result<Vec<Label>> {
        let first = self
            .inner
            .issues(&self.org, repo)
            .list_labels_for_repo()
            .per_page(100)
            .send()
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        let mut labels = first.items.clone();
        labels.extend(
            collect_paginated(&self.inner, first, |e| map_repo_error(&self.org, repo, e)).await?,
        );
        Ok(labels)
    }

    pub async fn create_label(&self, repo: &str, label: &LabelSpec) -> Result<()> {
        let color = normalize_color(&label.color);
        self.inner
            .issues(&self.org, repo)
            .create_label(
                label.name.clone(),
                color,
                label.description.clone().unwrap_or_default(),
            )
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(())
    }

    pub async fn update_label(&self, repo: &str, label: &LabelSpec) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/labels/{}",
            self.org,
            repo,
            encode_label_name(&label.name)
        );
        #[derive(Serialize)]
        struct Body {
            name: String,
            color: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,
        }
        let body = Body {
            name: label.name.clone(),
            color: normalize_color(&label.color),
            description: label.description.clone(),
        };
        self.inner
            ._patch(path, Some(&body))
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(())
    }

    pub async fn delete_label(&self, repo: &str, label_name: &str) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/labels/{}",
            self.org,
            repo,
            encode_label_name(label_name)
        );
        self.inner
            ._delete(path, Option::<()>::None.as_ref())
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(())
    }

    pub async fn label_usage(
        &self,
        repo: &str,
        label_name: &str,
        include_details: bool,
    ) -> Result<Option<Vec<LabelUsageEntry>>> {
        let page_limit: usize = if include_details { 10 } else { 1 };
        let mut issues_page = self
            .inner
            .issues(&self.org, repo)
            .list()
            .labels(&[label_name.to_string()])
            .state(params::State::All)
            .per_page(page_limit as u8)
            .send()
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;

        let mut entries = collect_issue_refs(&issues_page.items);
        if include_details {
            while let Some(next) = self
                .inner
                .get_page(&issues_page.next)
                .await
                .map_err(|e| map_repo_error(&self.org, repo, e))?
            {
                entries.extend(collect_issue_refs(&next.items));
                if entries.len() >= page_limit {
                    break;
                }
                issues_page = next;
            }
        } else if issues_page.next.is_some() {
            entries.push(LabelUsageEntry {
                number: 0,
                url: None,
                is_pr: false,
            });
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            Ok(Some(entries))
        }
    }

    pub async fn get_repo_settings(&self, repo: &str) -> Result<RepoSettings> {
        let repo_model = self.get_repo(repo).await?;

        Ok(RepoSettings {
            pull_requests: Some(PullRequestSettings {
                allow_merge_commit: repo_model.allow_merge_commit,
                allow_squash_merge: repo_model.allow_squash_merge,
                allow_rebase_merge: repo_model.allow_rebase_merge,
                allow_auto_merge: repo_model.allow_auto_merge,
                delete_branch_on_merge: repo_model.delete_branch_on_merge,
                merge_commit_message_option: None,
                squash_merge_option: None,
            }),
        })
    }

    pub async fn update_repo_settings(&self, repo: &str, settings: &RepoSettings) -> Result<()> {
        #[derive(Serialize)]
        struct Body {
            #[serde(skip_serializing_if = "Option::is_none")]
            allow_merge_commit: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            allow_squash_merge: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            allow_rebase_merge: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            allow_auto_merge: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            delete_branch_on_merge: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            merge_commit_message: Option<crate::settings::MergeCommitMessage>,
            #[serde(skip_serializing_if = "Option::is_none")]
            merge_commit_title: Option<crate::settings::MergeCommitTitle>,
            #[serde(skip_serializing_if = "Option::is_none")]
            squash_merge_commit_message: Option<crate::settings::SquashMergeCommitMessage>,
            #[serde(skip_serializing_if = "Option::is_none")]
            squash_merge_commit_title: Option<crate::settings::SquashMergeCommitTitle>,
        }

        let (merge_commit_title, merge_commit_message) = settings
            .pull_requests
            .as_ref()
            .and_then(|p| p.merge_commit_message_option.as_ref())
            .map(crate::settings::map_merge_message_option)
            .unwrap_or((None, None));

        let (option_title, option_message) = settings
            .pull_requests
            .as_ref()
            .and_then(|p| p.squash_merge_option.as_ref())
            .map(crate::settings::map_squash_option)
            .unwrap_or((None, None));

        let body = Body {
            allow_merge_commit: settings
                .pull_requests
                .as_ref()
                .and_then(|p| p.allow_merge_commit),
            allow_squash_merge: settings
                .pull_requests
                .as_ref()
                .and_then(|p| p.allow_squash_merge),
            allow_rebase_merge: settings
                .pull_requests
                .as_ref()
                .and_then(|p| p.allow_rebase_merge),
            allow_auto_merge: settings
                .pull_requests
                .as_ref()
                .and_then(|p| p.allow_auto_merge),
            delete_branch_on_merge: settings
                .pull_requests
                .as_ref()
                .and_then(|p| p.delete_branch_on_merge),
            merge_commit_message,
            merge_commit_title,
            squash_merge_commit_message: option_message,
            squash_merge_commit_title: option_title,
        };

        // Only send an update if at least one field is present.
        if serde_json::to_value(&body)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|m| m.is_empty())
            .unwrap_or(true)
        {
            return Ok(());
        }

        self.inner
            ._patch(format!("/repos/{}/{}", self.org, repo), Some(&body))
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;

        Ok(())
    }

    pub async fn get_file(
        &self,
        repo: &str,
        path: &str,
        branch: Option<&str>,
    ) -> Result<Option<RepoFile>> {
        #[derive(serde::Deserialize)]
        struct ContentFile {
            content: String,
            sha: String,
            encoding: String,
        }

        let route = match branch {
            Some(b) => format!("/repos/{}/{}/contents/{}?ref={}", self.org, repo, path, b),
            None => format!("/repos/{}/{}/contents/{}", self.org, repo, path),
        };
        match self.inner.get::<ContentFile, _, ()>(route, None).await {
            Ok(file) => {
                if file.encoding != "base64" {
                    return Ok(None);
                }
                let decoded = match BASE64.decode(file.content.replace('\n', "")) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        warn!(
                            "could not decode file content for {}/{}:{}: {}",
                            self.org, repo, path, e
                        );
                        return Ok(None);
                    }
                };
                let content = String::from_utf8_lossy(&decoded).to_string();
                Ok(Some(RepoFile {
                    sha: file.sha,
                    content,
                }))
            }
            Err(octocrab::Error::GitHub { ref source, .. })
                if source.status_code == reqwest::StatusCode::NOT_FOUND =>
            {
                Ok(None)
            }
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }

    pub async fn put_file(
        &self,
        repo: &str,
        path: &str,
        content: &str,
        sha: Option<String>,
        message: &str,
        branch: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            message: &'a str,
            content: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            sha: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            branch: Option<&'a str>,
        }

        let body = Body {
            message,
            content: BASE64.encode(content.as_bytes()),
            sha,
            branch,
        };
        let route = format!("/repos/{}/{}/contents/{}", self.org, repo, path);
        self.inner
            ._put(route, Some(&body))
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(())
    }

    pub async fn delete_file(
        &self,
        repo: &str,
        path: &str,
        sha: &str,
        message: &str,
        branch: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            message: &'a str,
            sha: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            branch: Option<&'a str>,
        }
        let body = Body {
            message,
            sha,
            branch,
        };
        let route = format!("/repos/{}/{}/contents/{}", self.org, repo, path);
        self.inner
            ._delete(route, Some(&body))
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(())
    }

    pub async fn list_github_files(
        &self,
        repo: &str,
        branch: &str,
        prefix: &str,
    ) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct TreeEntry {
            path: String,
            #[serde(rename = "type")]
            entry_type: String,
        }
        #[derive(serde::Deserialize)]
        struct TreeResp {
            tree: Vec<TreeEntry>,
        }

        let sha = self.get_branch_sha(repo, branch).await?;
        let path = format!("/repos/{}/{}/git/trees/{}?recursive=1", self.org, repo, sha);
        let resp: TreeResp = self
            .inner
            .get(path, None::<&()>)
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;

        Ok(resp
            .tree
            .into_iter()
            .filter(|e| e.entry_type == "blob" && e.path.starts_with(prefix))
            .map(|e| e.path)
            .collect())
    }

    pub async fn get_branch_protection(
        &self,
        repo: &str,
        pattern: &str,
    ) -> Result<Option<BranchProtectionRule>> {
        let path = format!(
            "/repos/{}/{}/branches/{}/protection",
            self.org, repo, pattern
        );
        match self
            .inner
            .get::<BranchProtectionResponse, _, ()>(path, None)
            .await
        {
            Ok(data) => Ok(Some(map_branch_protection_response(pattern, data))),
            Err(octocrab::Error::GitHub { ref source, .. })
                if source.status_code == reqwest::StatusCode::NOT_FOUND =>
            {
                Ok(None)
            }
            Err(octocrab::Error::GitHub { ref source, .. })
                if source.status_code == reqwest::StatusCode::FORBIDDEN =>
            {
                warn!(
                    "branch protection not available for {}/{}: {}",
                    self.org, repo, source.message
                );
                Ok(None)
            }
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }

    pub async fn set_branch_protection(
        &self,
        repo: &str,
        rule: &BranchProtectionRule,
    ) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/branches/{}/protection",
            self.org, repo, rule.pattern
        );
        let body = BranchProtectionRequest::from_rule(rule);
        match self.inner._put(path, Some(&body)).await {
            Ok(_) => Ok(()),
            Err(octocrab::Error::GitHub { ref source, .. })
                if source.status_code == reqwest::StatusCode::FORBIDDEN =>
            {
                warn!(
                    "branch protection not available for {}/{}: {}",
                    self.org, repo, source.message
                );
                Ok(())
            }
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }

    pub async fn get_branch_sha(&self, repo: &str, branch: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct RefObject {
            sha: String,
        }
        #[derive(serde::Deserialize)]
        struct RefResp {
            object: RefObject,
        }

        let path = format!("/repos/{}/{}/git/ref/heads/{}", self.org, repo, branch);
        let resp: RefResp = self
            .inner
            .get(path, None::<&()>)
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;
        Ok(resp.object.sha)
    }

    pub async fn create_branch_from(
        &self,
        repo: &str,
        new_branch: &str,
        base_sha: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            r#ref: &'a str,
            sha: &'a str,
        }
        let body = Body {
            r#ref: &format!("refs/heads/{new_branch}"),
            sha: base_sha,
        };
        let path = format!("/repos/{}/{}/git/refs", self.org, repo);
        match self.inner._post(path, Some(&body)).await {
            Ok(_) => Ok(()),
            Err(octocrab::Error::GitHub { ref source, .. })
                if source.status_code == reqwest::StatusCode::UNPROCESSABLE_ENTITY =>
            {
                // branch probably exists; treat as success
                Ok(())
            }
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }

    pub async fn create_pull_request(
        &self,
        repo: &str,
        title: &str,
        head: &str,
        base: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            title: &'a str,
            head: &'a str,
            base: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
            draft: bool,
        }
        let body = Body {
            title,
            head,
            base,
            body,
            draft,
        };
        match self
            .inner
            ._post(format!("/repos/{}/{}/pulls", self.org, repo), Some(&body))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }

    pub async fn find_open_pr_by_head_prefix(
        &self,
        repo: &str,
        head_prefix: &str,
        base: &str,
    ) -> Result<Option<PullRequest>> {
        let mut page = self
            .inner
            .pulls(&self.org, repo)
            .list()
            .state(octocrab::params::State::Open)
            .base(base.to_string())
            .per_page(50)
            .send()
            .await
            .map_err(|e| map_repo_error(&self.org, repo, e))?;

        loop {
            if let Some(pr) = page
                .items
                .iter()
                .find(|p| {
                    p.state == Some(IssueState::Open) && p.head.ref_field.starts_with(head_prefix)
                })
                .cloned()
            {
                return Ok(Some(pr));
            }
            match self
                .inner
                .get_page::<PullRequest>(&page.next)
                .await
                .map_err(|e| map_repo_error(&self.org, repo, e))?
            {
                Some(next) => page = next,
                None => break,
            }
        }
        Ok(None)
    }

    pub async fn update_pull_request(
        &self,
        repo: &str,
        number: u64,
        title: &str,
        body: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            title: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
        }
        let body = Body { title, body };
        match self
            .inner
            ._patch(
                format!("/repos/{}/{}/pulls/{}", self.org, repo, number),
                Some(&body),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(map_repo_error(&self.org, repo, e)),
        }
    }
}

fn normalize_color(color: &Option<String>) -> String {
    color
        .as_ref()
        .map(|c| c.trim_start_matches('#').to_lowercase())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "ededed".to_string())
}

fn encode_label_name(name: &str) -> String {
    utf8_percent_encode(name, NON_ALPHANUMERIC).to_string()
}

async fn collect_paginated<T, F>(
    octo: &Octocrab,
    mut page: octocrab::Page<T>,
    map_err: F,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
    F: Fn(octocrab::Error) -> Error,
{
    let mut items = Vec::new();
    while let Some(mut next) = octo.get_page::<T>(&page.next).await.map_err(&map_err)? {
        items.extend(std::mem::take(&mut next.items).into_iter());
        page = next;
    }
    Ok(items)
}

fn map_repo_error(org: &str, repo: &str, err: octocrab::Error) -> Error {
    if let octocrab::Error::GitHub { source, .. } = &err {
        if source.status_code == reqwest::StatusCode::NOT_FOUND {
            return Error::RepoNotFound {
                org: org.to_string(),
                repo: repo.to_string(),
            };
        }
    }
    Error::Octo(err)
}

fn collect_issue_refs(issues: &[Issue]) -> Vec<LabelUsageEntry> {
    issues
        .iter()
        .map(|issue| LabelUsageEntry {
            number: issue.number,
            url: Some(issue.html_url.to_string()),
            is_pr: issue.pull_request.is_some(),
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct BranchProtectionResponse {
    required_status_checks: Option<RequiredStatusChecksResponse>,
    enforce_admins: Option<EnforceAdmins>,
    required_pull_request_reviews: Option<RequiredPullRequestReviewsResponse>,
    restrictions: Option<BranchRestrictionsResponse>,
    allow_force_pushes: Option<EnabledFlag>,
    allow_deletions: Option<EnabledFlag>,
    block_creations: Option<EnabledFlag>,
    required_linear_history: Option<EnabledFlag>,
    required_conversation_resolution: Option<EnabledFlag>,
    required_signatures: Option<EnabledFlag>,
}

#[derive(serde::Deserialize)]
struct RequiredStatusChecksResponse {
    strict: Option<bool>,
    contexts: Option<Vec<String>>,
    checks: Option<Vec<StatusCheckResponse>>,
}

#[derive(serde::Deserialize)]
struct StatusCheckResponse {
    context: String,
    app_id: Option<u64>,
}

#[derive(serde::Deserialize)]
struct EnforceAdmins {
    enabled: Option<bool>,
}

#[derive(serde::Deserialize)]
struct RequiredPullRequestReviewsResponse {
    dismiss_stale_reviews: Option<bool>,
    require_code_owner_reviews: Option<bool>,
    required_approving_review_count: Option<u8>,
    require_last_push_approval: Option<bool>,
    dismissal_restrictions: Option<ReviewDismissalRestrictionsResponse>,
}

#[derive(serde::Deserialize)]
struct ReviewDismissalRestrictionsResponse {
    users: Option<Vec<SimpleActor>>,
    teams: Option<Vec<SimpleActor>>,
}

#[derive(serde::Deserialize)]
struct BranchRestrictionsResponse {
    users: Option<Vec<SimpleActor>>,
    teams: Option<Vec<SimpleActor>>,
    apps: Option<Vec<SimpleActor>>,
}

#[derive(serde::Deserialize)]
struct SimpleActor {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(serde::Deserialize)]
struct EnabledFlag {
    enabled: Option<bool>,
}

fn map_branch_protection_response(
    pattern: &str,
    resp: BranchProtectionResponse,
) -> BranchProtectionRule {
    BranchProtectionRule {
        pattern: pattern.to_string(),
        required_status_checks: resp.required_status_checks.map(|c| RequiredStatusChecks {
            strict: c.strict,
            contexts: c.contexts,
            checks: c.checks.map(|v| {
                v.into_iter()
                    .map(|c| StatusCheck {
                        context: c.context,
                        app_id: c.app_id,
                    })
                    .collect()
            }),
        }),
        required_pull_request_reviews: resp.required_pull_request_reviews.map(|r| {
            RequiredPullRequestReviews {
                dismiss_stale_reviews: r.dismiss_stale_reviews,
                require_code_owner_reviews: r.require_code_owner_reviews,
                required_approving_review_count: r.required_approving_review_count,
                require_last_push_approval: r.require_last_push_approval,
                dismissal_restrictions: r.dismissal_restrictions.map(|d| {
                    ReviewDismissalRestrictions {
                        users: d
                            .users
                            .map(|u| u.into_iter().filter_map(|v| v.login.or(v.slug)).collect()),
                        teams: d
                            .teams
                            .map(|t| t.into_iter().filter_map(|v| v.slug.or(v.login)).collect()),
                    }
                }),
            }
        }),
        enforce_admins: resp.enforce_admins.and_then(|e| e.enabled),
        restrictions: resp.restrictions.map(|r| BranchRestrictions {
            users: r
                .users
                .map(|u| u.into_iter().filter_map(|v| v.login.or(v.slug)).collect()),
            teams: r
                .teams
                .map(|t| t.into_iter().filter_map(|v| v.slug.or(v.login)).collect()),
            apps: r
                .apps
                .map(|a| a.into_iter().filter_map(|v| v.slug.or(v.login)).collect()),
        }),
        allow_force_pushes: resp.allow_force_pushes.and_then(|f| f.enabled),
        allow_deletions: resp.allow_deletions.and_then(|f| f.enabled),
        block_creations: resp.block_creations.and_then(|f| f.enabled),
        require_linear_history: resp.required_linear_history.and_then(|f| f.enabled),
        required_conversation_resolution: resp
            .required_conversation_resolution
            .and_then(|f| f.enabled),
        required_signatures: resp.required_signatures.and_then(|f| f.enabled),
    }
}

#[derive(serde::Serialize)]
struct BranchProtectionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    required_status_checks: Option<RequiredStatusChecksRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enforce_admins: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_pull_request_reviews: Option<RequiredPullRequestReviewsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restrictions: Option<BranchRestrictionsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_force_pushes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_deletions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_creations: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "required_linear_history"
    )]
    require_linear_history: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_conversation_resolution: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_signatures: Option<bool>,
}

#[derive(serde::Serialize)]
struct RequiredStatusChecksRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contexts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checks: Option<Vec<StatusCheckRequest>>,
}

#[derive(serde::Serialize)]
struct StatusCheckRequest {
    context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<u64>,
}

#[derive(serde::Serialize)]
struct RequiredPullRequestReviewsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    dismiss_stale_reviews: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    require_code_owner_reviews: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_approving_review_count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    require_last_push_approval: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dismissal_restrictions: Option<ReviewDismissalRestrictionsRequest>,
}

#[derive(serde::Serialize)]
struct ReviewDismissalRestrictionsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    users: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    teams: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
struct BranchRestrictionsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    users: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    teams: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apps: Option<Vec<String>>,
}

impl BranchProtectionRequest {
    fn from_rule(rule: &BranchProtectionRule) -> Self {
        BranchProtectionRequest {
            required_status_checks: rule.required_status_checks.as_ref().map(|c| {
                RequiredStatusChecksRequest {
                    strict: c.strict,
                    contexts: c.contexts.clone(),
                    checks: c.checks.as_ref().map(|v| {
                        v.iter()
                            .map(|c| StatusCheckRequest {
                                context: c.context.clone(),
                                app_id: c.app_id,
                            })
                            .collect()
                    }),
                }
            }),
            enforce_admins: rule.enforce_admins,
            required_pull_request_reviews: rule.required_pull_request_reviews.as_ref().map(|r| {
                RequiredPullRequestReviewsRequest {
                    dismiss_stale_reviews: r.dismiss_stale_reviews,
                    require_code_owner_reviews: r.require_code_owner_reviews,
                    required_approving_review_count: r.required_approving_review_count,
                    require_last_push_approval: r.require_last_push_approval,
                    dismissal_restrictions: map_review_dismissals(r),
                }
            }),
            restrictions: rule
                .restrictions
                .as_ref()
                .map(|r| BranchRestrictionsRequest {
                    users: r.users.clone(),
                    teams: r.teams.clone(),
                    apps: r.apps.clone(),
                }),
            allow_force_pushes: rule.allow_force_pushes,
            allow_deletions: rule.allow_deletions,
            block_creations: rule.block_creations,
            require_linear_history: rule.require_linear_history,
            required_conversation_resolution: rule.required_conversation_resolution,
            required_signatures: rule.required_signatures,
        }
    }
}

fn map_review_dismissals(
    reviews: &RequiredPullRequestReviews,
) -> Option<ReviewDismissalRestrictionsRequest> {
    reviews
        .dismissal_restrictions
        .as_ref()
        .map(|d| ReviewDismissalRestrictionsRequest {
            users: d.users.clone(),
            teams: d.teams.clone(),
        })
}
