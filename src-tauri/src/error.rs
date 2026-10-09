use serde::{Serialize, Serializer};

/// 应用统一错误类型。所有 Tauri command 都返回 `AppResult<T>`，
/// 错误序列化为字符串后由前端展示。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("数据库错误：{0}")]
    Db(#[from] rusqlite::Error),

    #[error("凭据存取失败：{0}")]
    Keyring(#[from] keyring::Error),

    #[error("网络请求失败：{0}")]
    Http(#[from] reqwest::Error),

    #[error("邮件协议错误：{0}")]
    Mail(String),

    #[error("邮件地址解析失败：{0}")]
    Address(String),

    #[error("文件读写错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("数据格式错误：{0}")]
    Json(#[from] serde_json::Error),

    #[error("账号不存在：{0}")]
    AccountNotFound(String),

    /// 指定的 AI 模型未在本机安装。单独成一类，便于自动回退到可用模型。
    #[error("模型未安装：{0}")]
    ModelNotFound(String),

    /// Ollama 的 runner 卡在异常状态（典型表现：复读保护误报）。
    /// 单独成一类，便于「卸载模型 → 重载重试」自愈。
    #[error("模型运行器状态异常：{0}")]
    ModelBusy(String),

    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<imap::Error> for AppError {
    fn from(e: imap::Error) -> Self {
        AppError::Mail(e.to_string())
    }
}

impl From<native_tls::Error> for AppError {
    fn from(e: native_tls::Error) -> Self {
        AppError::Mail(format!("TLS 初始化失败：{e}"))
    }
}

impl From<lettre::transport::smtp::Error> for AppError {
    fn from(e: lettre::transport::smtp::Error) -> Self {
        AppError::Mail(e.to_string())
    }
}

impl From<lettre::error::Error> for AppError {
    fn from(e: lettre::error::Error) -> Self {
        AppError::Mail(e.to_string())
    }
}

impl From<lettre::address::AddressError> for AppError {
    fn from(e: lettre::address::AddressError) -> Self {
        AppError::Address(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
