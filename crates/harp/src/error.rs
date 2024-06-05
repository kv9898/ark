//
// error.rs
//
// Copyright (C) 2022 Posit Software, PBC. All rights reserved.
//
//

use std::backtrace::Backtrace;
use std::fmt;
use std::str::Utf8Error;

use crate::utils::r_type2char;

pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    HelpTopicNotFoundError {
        topic: String,
        package: Option<String>,
    },
    ParseError {
        code: String,
        message: String,
    },
    EvaluationError {
        code: Option<String>,
        message: String,
        class: Option<Vec<String>>,
        r_trace: String,
        rust_trace: Option<Backtrace>,
    },
    UnsafeEvaluationError(String),
    UnexpectedLength(usize, usize),
    UnexpectedType(u32, Vec<u32>),
    ValueOutOfRange {
        value: i64,
        min: i64,
        max: i64,
    },
    InvalidUtf8(Utf8Error),
    TryEvalError {
        message: String,
    },
    TopLevelExecError {
        message: String,
        backtrace: Backtrace,
        span_trace: tracing_error::SpanTrace,
    },
    ParseSyntaxError {
        message: String,
        line: i32,
    },
    MissingValueError,
    MissingBindingError {
        name: String,
    },
    InspectError {
        path: Vec<String>,
    },
    StackUsageError {
        message: String,
        backtrace: Backtrace,
        span_trace: tracing_error::SpanTrace,
    },
    Anyhow(anyhow::Error),
}

// empty implementation required for 'anyhow'
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::InvalidUtf8(source) => Some(source),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::HelpTopicNotFoundError { topic, package } => match package {
                Some(package) => write!(
                    f,
                    "Help topic '{}' not available in package '{}'",
                    topic, package
                ),
                None => write!(f, "Help topic '{}' not available", topic),
            },

            Error::ParseError { code, message } => {
                write!(f, "Error parsing {}: {}", code, message)
            },

            Error::EvaluationError {
                code,
                message,
                r_trace,
                rust_trace,
                ..
            } => {
                let mut message = if let Some(code) = code {
                    format!("Error evaluating {code}: {message}")
                } else {
                    message.clone()
                };

                if !r_trace.is_empty() {
                    message = format!("{message}\n\nR backtrace:\n{r_trace}");
                }

                if let Some(rust_trace) = rust_trace {
                    message = format!("{message}\n\nR thread backtrace:\n{rust_trace}");
                }

                write!(f, "{message}")
            },

            Error::UnsafeEvaluationError(code) => {
                write!(
                    f,
                    "Evaluation of function calls not supported in this context: {}",
                    code
                )
            },

            Error::UnexpectedLength(actual, expected) => {
                write!(
                    f,
                    "Unexpected vector length (expected {}; got {})",
                    expected, actual
                )
            },

            Error::UnexpectedType(actual, expected) => {
                let actual = r_type2char(*actual);
                let expected = expected
                    .iter()
                    .map(|value| r_type2char(*value))
                    .collect::<Vec<_>>()
                    .join(" | ");
                write!(
                    f,
                    "Unexpected vector type (expected {}; got {})",
                    expected, actual
                )
            },

            Error::ValueOutOfRange { value, min, max } => {
                write!(
                    f,
                    "Value is out of range: value: {} min: {} max: {}",
                    value, min, max
                )
            },

            Error::InvalidUtf8(error) => {
                write!(f, "Invalid UTF-8 in string: {}", error)
            },

            Error::TryEvalError { message } => {
                write!(f, "R-level error: {}", message)
            },

            Error::TopLevelExecError {
                message,
                backtrace: _backtrace,
                span_trace,
            } => {
                writeln!(f, "{message}")?;

                writeln!(f)?;
                writeln!(f, "In spans:")?;
                span_trace.fmt(f)?;
                writeln!(f)?;
                writeln!(f)?;

                Ok(())
            },

            Error::ParseSyntaxError { message, line } => {
                write!(f, "Syntax error on line {} when parsing: {}", line, message)
            },

            Error::MissingValueError => {
                write!(f, "Missing value")
            },

            Error::InspectError { path } => {
                write!(f, "Error inspecting path {}", path.join(" / "))
            },

            Error::StackUsageError { .. } => {
                write!(f, "C stack usage too close to the limit")
            },

            Error::Anyhow(err) => {
                write!(f, "{err:?}")
            },

            Error::MissingBindingError { name } => {
                write!(f, "Can't find binding {name} in environment")
            },
        }
    }
}

#[macro_export]
macro_rules! anyhow {
    ($($rest: expr),*) => {{
        let message = format!($($rest, )*);
        crate::error::Error::Anyhow {
            message,
        }
    }}
}

// TODO: Macro variants of `check_` helpers that record function name, see
// `function_name` in https://docs.rs/stdext/latest/src/stdext/macros.rs.html

fn check(x: impl Into<libr::SEXP>, expected: libr::SEXPTYPE) -> crate::Result<()> {
    let x = x.into();
    let typ = crate::r_typeof(x);

    if typ != expected {
        let err = Error::UnexpectedType(typ, vec![expected]);
        return Err(err);
    }

    Ok(())
}

pub fn check_env(x: impl Into<libr::SEXP>) -> crate::Result<()> {
    check(x, libr::ENVSXP)
}

// NOTE: Debug is the same as Display but with backtrace printing.
// This matches anyhow error formatters and we can still retrieve the
// struct-style format with `{:#?}`.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)?;

        match self {
            Error::TopLevelExecError {
                message: _,
                backtrace,
                span_trace: _,
            } => {
                // If you change this header, make sure to update the panic handler in main.rs
                writeln!(f)?;
                writeln!(f, "R thread backtrace:")?;
                fmt::Display::fmt(backtrace, f)
            },
            _ => Ok(()),
        }
    }
}

impl From<Utf8Error> for Error {
    fn from(error: Utf8Error) -> Self {
        Self::InvalidUtf8(error)
    }
}
