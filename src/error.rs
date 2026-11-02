//! Machine-readable errors and stable exit codes.
//!
//! Agents need to tell "no such symbol" from "file not indexed" from "the
//! command ran but produced nothing" without parsing English on stderr. Every
//! fatal path goes through [`FilerError`], which maps to one of the exit codes
//! below. With `--json` the same information is printed as an envelope:
//!
//! ```json
//! {"error":{"code":"symbol_not_found","message":"...","hint":"..."}}
//! ```

use serde_json::{json, Value};

/// A bug, an I/O failure or another unrecoverable error.
pub const EXIT_INTERNAL: i32 = 1;
/// Something the caller named does not exist (symbol, file, branch, state).
pub const EXIT_NOT_FOUND: i32 = 2;
/// The file exists but the index does not know it (binary, skipped dir, ...).
pub const EXIT_NOT_INDEXED: i32 = 3;
/// The command succeeded but produced no rows/units.
pub const EXIT_EMPTY: i32 = 4;

/// Stable machine-readable error codes. Several codes share an exit code; the
/// code, not the exit status, is what callers should branch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Internal,
    SymbolNotFound,
    UnitNotFound,
    FileNotFound,
    BranchNotFound,
    StateNotFound,
    NotIndexed,
    EmptyResult,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Internal => "internal",
            Code::SymbolNotFound => "symbol_not_found",
            Code::UnitNotFound => "unit_not_found",
            Code::FileNotFound => "file_not_found",
            Code::BranchNotFound => "branch_not_found",
            Code::StateNotFound => "state_not_found",
            Code::NotIndexed => "not_indexed",
            Code::EmptyResult => "empty_result",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Code::Internal => EXIT_INTERNAL,
            Code::SymbolNotFound
            | Code::UnitNotFound
            | Code::FileNotFound
            | Code::BranchNotFound
            | Code::StateNotFound => EXIT_NOT_FOUND,
            Code::NotIndexed => EXIT_NOT_INDEXED,
            Code::EmptyResult => EXIT_EMPTY,
        }
    }
}

/// A fatal error: a stable code, a one-line message and an optional hint that
/// tells the caller what to try next.
#[derive(Debug)]
pub struct FilerError {
    pub code: Code,
    pub message: String,
    pub hint: Option<String>,
}

impl FilerError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Code::Internal, message)
    }

    pub fn symbol_not_found(file: &str, symbol: &str) -> Self {
        Self::new(
            Code::SymbolNotFound,
            format!("{file} has no symbol {symbol}"),
        )
        .with_hint("pass a path alone to bundle the whole file")
    }

    pub fn unit_not_found(message: impl Into<String>) -> Self {
        Self::new(Code::UnitNotFound, message)
    }

    pub fn file_not_found(message: impl Into<String>) -> Self {
        Self::new(Code::FileNotFound, message)
    }

    pub fn not_indexed(message: impl Into<String>) -> Self {
        Self::new(Code::NotIndexed, message)
    }

    pub fn empty_result(message: impl Into<String>) -> Self {
        Self::new(Code::EmptyResult, message)
    }

    /// The JSON envelope written by `--json`.
    pub fn envelope(&self) -> Value {
        let mut error = json!({
            "code": self.code.as_str(),
            "message": self.message,
        });
        if let Some(hint) = &self.hint {
            error["hint"] = json!(hint);
        }
        json!({ "error": error })
    }

    /// Prints the error to stderr, as JSON with `--json`, as prose otherwise.
    /// Hard errors are never silenced by `--quiet`.
    pub fn emit(&self, json: bool) {
        if json {
            eprintln!("{}", self.envelope());
        } else {
            eprintln!("filer: {}", self.message);
            if let Some(hint) = &self.hint {
                eprintln!("filer: hint: {hint}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(Code::Internal.exit_code(), 1);
        assert_eq!(Code::SymbolNotFound.exit_code(), 2);
        assert_eq!(Code::UnitNotFound.exit_code(), 2);
        assert_eq!(Code::FileNotFound.exit_code(), 2);
        assert_eq!(Code::BranchNotFound.exit_code(), 2);
        assert_eq!(Code::StateNotFound.exit_code(), 2);
        assert_eq!(Code::NotIndexed.exit_code(), 3);
        assert_eq!(Code::EmptyResult.exit_code(), 4);
    }

    #[test]
    fn envelope_shape() {
        let error = FilerError::symbol_not_found("app/Service.java", "Nope")
            .with_hint("try the class name");
        assert_eq!(
            error.envelope(),
            serde_json::json!({
                "error": {
                    "code": "symbol_not_found",
                    "message": "app/Service.java has no symbol Nope",
                    "hint": "try the class name",
                }
            })
        );
    }

    #[test]
    fn envelope_omits_a_missing_hint() {
        let error = FilerError::internal("disk on fire");
        assert_eq!(
            error.envelope(),
            serde_json::json!({
                "error": {"code": "internal", "message": "disk on fire"}
            })
        );
    }
}
