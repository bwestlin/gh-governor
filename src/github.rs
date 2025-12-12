use octocrab::Octocrab;
use octocrab::models::{Label, issues::Issue};
use octocrab::params;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::sets::LabelSpec;

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
