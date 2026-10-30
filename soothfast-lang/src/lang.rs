use std::fmt;
use std::path::Path;

use tree_sitter::{Parser, Tree};

/// A source language with a tree-sitter grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Java (`.java`).
    Java,
    /// Kotlin (`.kt`, `.kts`).
    Kotlin,
    /// Python (`.py`, `.pyi`).
    Python,
    /// JavaScript (`.js`, `.mjs`, `.cjs`, `.jsx`).
    JavaScript,
    /// TypeScript (`.ts`, `.mts`, `.cts`).
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
}

impl Lang {
    /// The language of a file, chosen by extension.
    pub fn for_path(path: &Path) -> Option<Lang> {
        match path.extension()?.to_str()? {
            "java" => Some(Lang::Java),
            "kt" | "kts" => Some(Lang::Kotlin),
            "py" | "pyi" => Some(Lang::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::JavaScript),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            _ => None,
        }
    }

    /// Lowercase language name.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Java => "java",
            Lang::Kotlin => "kotlin",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
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
