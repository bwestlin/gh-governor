use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct RepoSettings {
    #[serde(default)]
    pub pull_requests: Option<PullRequestSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PullRequestSettings {
    pub allow_merge_commit: Option<bool>,
    pub allow_squash_merge: Option<bool>,
    pub allow_rebase_merge: Option<bool>,
    pub allow_auto_merge: Option<bool>,
    pub delete_branch_on_merge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_commit_message_option: Option<MergeCommitMessageOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squash_merge_option: Option<SquashMergeOption>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SquashMergeCommitMessage {
    PrBody,
    CommitMessages,
    Blank,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SquashMergeCommitTitle {
    PrTitle,
    CommitOrPrTitle,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MergeCommitMessage {
    PrTitle,
    PrBody,
    Blank,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MergeCommitTitle {
    PrTitle,
    MergeMessage,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SquashMergeOption {
    DefaultMessage,
    PullRequestTitle,
    PullRequestTitleAndCommitDetails,
    PullRequestTitleAndDescription,
}

pub fn map_squash_option(
    opt: &SquashMergeOption,
) -> (
    Option<SquashMergeCommitTitle>,
    Option<SquashMergeCommitMessage>,
) {
    match opt {
        SquashMergeOption::DefaultMessage => (
            Some(SquashMergeCommitTitle::CommitOrPrTitle),
            Some(SquashMergeCommitMessage::CommitMessages),
        ),
        SquashMergeOption::PullRequestTitle => (
            Some(SquashMergeCommitTitle::PrTitle),
            Some(SquashMergeCommitMessage::Blank),
        ),
        SquashMergeOption::PullRequestTitleAndCommitDetails => (
            Some(SquashMergeCommitTitle::PrTitle),
            Some(SquashMergeCommitMessage::CommitMessages),
        ),
        SquashMergeOption::PullRequestTitleAndDescription => (
            Some(SquashMergeCommitTitle::PrTitle),
            Some(SquashMergeCommitMessage::PrBody),
        ),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeCommitMessageOption {
    DefaultMessage,
    PullRequestTitle,
    PullRequestTitleAndDescription,
}

pub fn map_merge_message_option(
    opt: &MergeCommitMessageOption,
) -> (Option<MergeCommitTitle>, Option<MergeCommitMessage>) {
    match opt {
        MergeCommitMessageOption::DefaultMessage => (
            Some(MergeCommitTitle::MergeMessage),
            Some(MergeCommitMessage::PrTitle),
        ),
        MergeCommitMessageOption::PullRequestTitle => (
            Some(MergeCommitTitle::PrTitle),
            Some(MergeCommitMessage::PrTitle),
        ),
        MergeCommitMessageOption::PullRequestTitleAndDescription => (
            Some(MergeCommitTitle::PrTitle),
            Some(MergeCommitMessage::PrBody),
        ),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct BranchProtectionConfig {
    #[serde(default)]
    pub rules: Vec<BranchProtectionRule>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BranchProtectionRule {
    pub pattern: String,
    #[serde(default)]
    pub required_status_checks: Option<RequiredStatusChecks>,
    #[serde(default)]
    pub required_pull_request_reviews: Option<RequiredPullRequestReviews>,
    #[serde(default)]
    pub enforce_admins: Option<bool>,
    #[serde(default)]
    pub restrictions: Option<BranchRestrictions>,
    #[serde(default)]
    pub allow_force_pushes: Option<bool>,
    #[serde(default)]
    pub allow_deletions: Option<bool>,
    #[serde(default)]
    pub block_creations: Option<bool>,
    #[serde(default)]
    pub require_linear_history: Option<bool>,
    #[serde(default)]
    pub required_conversation_resolution: Option<bool>,
    #[serde(default)]
    pub required_signatures: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RequiredStatusChecks {
    pub strict: Option<bool>,
    #[serde(default)]
    pub contexts: Option<Vec<String>>,
    #[serde(default)]
    pub checks: Option<Vec<StatusCheck>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct StatusCheck {
    pub context: String,
    #[serde(default)]
    pub app_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RequiredPullRequestReviews {
    #[serde(default)]
    pub dismiss_stale_reviews: Option<bool>,
    #[serde(default)]
    pub require_code_owner_reviews: Option<bool>,
    #[serde(default)]
    pub required_approving_review_count: Option<u8>,
    #[serde(default)]
    pub require_last_push_approval: Option<bool>,
    #[serde(default)]
    pub dismissal_restrictions: Option<ReviewDismissalRestrictions>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReviewDismissalRestrictions {
    #[serde(default)]
    pub users: Option<Vec<String>>,
    #[serde(default)]
    pub teams: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BranchRestrictions {
    #[serde(default)]
    pub users: Option<Vec<String>>,
    #[serde(default)]
    pub teams: Option<Vec<String>>,
    #[serde(default)]
    pub apps: Option<Vec<String>>,
}
