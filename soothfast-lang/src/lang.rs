use std::fmt;
use std::path::Path;

use tree_sitter::{Parser, Tree};

/// A source language with a tree-sitter grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Python (`.py`, `.pyi`).
    Python,
}

impl Lang {
    /// The language of a file, chosen by extension.
    pub fn for_path(path: &Path) -> Option<Lang> {
        match path.extension()?.to_str()? {
            "py" | "pyi" => Some(Lang::Python),
            _ => None,
        }
    }

    /// Lowercase language name.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    pub(crate) fn parse(self, source: &str) -> Result<Tree, Error> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.grammar())
            .map_err(|e| Error::Grammar(e.to_string()))?;
        parser.parse(source, None).ok_or(Error::Parse(self))
    }
}

/// Why a file could not be analyzed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The grammar was rejected by the tree-sitter runtime.
    Grammar(String),
    /// The parser produced no tree for the file.
    Parse(Lang),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Grammar(why) => write!(f, "incompatible tree-sitter grammar: {why}"),
            Error::Parse(lang) => write!(f, "could not parse {} source", lang.name()),
        }
    }
}

impl std::error::Error for Error {}
