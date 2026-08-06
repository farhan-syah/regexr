//! Lexer/tokenizer for regex patterns.

mod escape;
mod scanner;
mod token;

#[cfg(test)]
mod tests;

pub use scanner::Lexer;
pub use token::{EscapeKind, Token, TokenKind};
