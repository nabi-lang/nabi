//! Lexer: source text to `Vec<Token>`.

pub mod escape;
pub mod lexer;
pub mod token;
pub mod unicode;

pub use lexer::Lexer;
pub use token::{DocLine, Origin, Token, TokenKind};
