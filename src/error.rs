use std::fmt::{Display, Formatter};

pub const EXIT_ARGS: i32 = 2;
pub const EXIT_CONFIG: i32 = 3;
pub const EXIT_AUTH: i32 = 4;
pub const EXIT_NETWORK: i32 = 5;
pub const EXIT_RATE_LIMIT: i32 = 6;
pub const EXIT_PROVIDER: i32 = 7;
pub const EXIT_MODEL: i32 = 8;
pub const EXIT_SESSION: i32 = 10;

#[derive(Debug)]
pub struct AppError {
    pub code: i32,
    pub message: String,
}

impl AppError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;

pub trait ResultCodeExt<T> {
    fn code(self, code: i32, context: impl Into<String>) -> AppResult<T>;
}

impl<T, E> ResultCodeExt<T> for Result<T, E>
where
    E: std::error::Error,
{
    fn code(self, code: i32, context: impl Into<String>) -> AppResult<T> {
        self.map_err(|err| AppError::new(code, format!("{}: {}", context.into(), err)))
    }
}
