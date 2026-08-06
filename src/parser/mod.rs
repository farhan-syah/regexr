//! Regex parser module.
//!
//! Implements a recursive descent parser for regular expressions.

pub mod ast;
pub mod lexer;

mod alternation;
mod atom;
mod class;
mod group;
mod state;

#[cfg(test)]
mod tests;

pub use ast::*;
pub use lexer::{EscapeKind, Lexer, Token, TokenKind};
pub use state::{parse, Parser};
