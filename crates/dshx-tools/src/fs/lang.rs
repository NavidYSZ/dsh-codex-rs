//! The read card's `lang` hint (dsh `util-code-language` `readLangHintForPath`).

/// Short language id for a path's extension, if highlighted.
pub fn lang_for_path(path: &str) -> Option<&'static str> {
    let slash = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let base = &path[slash..];
    let ext = base.rsplit_once('.')?.1.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "tsx" => "tsx",
        "jsx" => "jsx",
        "tf" => "tf",
        "tfvars" => "tfvars",
        "gradle" => "gradle",
        "ts" | "mts" | "cts" => "ts",
        "js" | "mjs" | "cjs" => "js",
        "sh" | "bash" | "zsh" => "sh",
        "fish" => "fish",
        "json" | "jsonc" | "jsonl" | "ndjson" | "ipynb" => "json",
        "csv" => "csv",
        "py" | "pyw" | "pyi" => "py",
        "rb" | "rake" | "gemspec" => "rb",
        "go" => "go",
        "rs" => "rs",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "cs",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "php" => "php",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "conf" | "cfg" | "properties" => "ini",
        "env" => "env",
        "log" => "log",
        "diff" | "patch" => "diff",
        "http" => "http",
        "md" | "markdown" => "md",
        "mdx" => "mdx",
        "rst" => "rst",
        "tex" | "sty" | "cls" => "tex",
        "bib" => "bib",
        "adoc" => "adoc",
        "html" | "htm" | "xhtml" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "sql" => "sql",
        "xml" | "xsd" | "xsl" | "xslt" | "plist" | "svg" => "xml",
        "lua" => "lua",
        "bat" | "cmd" => "bat",
        "ps1" | "psm1" | "psd1" => "ps1",
        "r" => "r",
        "jl" => "jl",
        "dart" => "dart",
        "scala" => "scala",
        "clj" | "cljs" | "edn" => "clj",
        "erl" | "hrl" => "erl",
        "ex" | "exs" => "ex",
        "hs" => "hs",
        "fs" | "fsi" | "fsx" => "fs",
        "vb" => "vb",
        "pl" | "pm" => "pl",
        "v" => "v",
        "sv" | "svh" => "sv",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "hcl" => "hcl",
        "nix" => "nix",
        "vue" => "vue",
        "svelte" => "svelte",
        "makefile" | "mk" => "make",
        "cmake" => "cmake",
        "groovy" => "groovy",
        _ => return None,
    };
    Some(lang)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hints() {
        assert_eq!(super::lang_for_path("/a/b.rs"), Some("rs"));
        assert_eq!(super::lang_for_path("/a/b.TSX"), Some("tsx"));
        assert_eq!(super::lang_for_path("/a/notes.txt"), None);
        assert_eq!(super::lang_for_path("/a.d/Makefile"), None);
    }
}
