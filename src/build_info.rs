//! 构建来源（正式发版 / 本地构建）与提交号。
//!
//! 开发期常用本地构建直接替换生产二进制，版本号仍是同一个 `0.3.92`。
//! 没有这层标记时，只能靠文件时间或路径猜「现在跑的是哪一版」，
//! 排查问题时很容易对错二进制。

/// `channel` 为 `release` 时表示正式发版；本地部署脚本会写成 `local`。
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct BuildInfo {
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub commit: String,
}

impl Default for BuildInfo {
    fn default() -> Self {
        Self {
            channel: "release".to_string(),
            commit: String::new(),
        }
    }
}

impl BuildInfo {
    /// 用于界面的短标签，例如 `本地构建 ef3570e`；正式发版返回空串。
    pub(crate) fn local_label(&self) -> String {
        if self.channel == "release" {
            return String::new();
        }
        let channel = if self.channel.trim().is_empty() {
            "local"
        } else {
            self.channel.as_str()
        };
        if self.commit.trim().is_empty() {
            format!("本地构建 {channel}")
        } else {
            format!("本地构建 {} {}", channel, self.commit)
        }
    }
}

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/embedded-build.json"));

pub(crate) fn current() -> BuildInfo {
    serde_json::from_str(EMBEDDED).unwrap_or_default()
}
