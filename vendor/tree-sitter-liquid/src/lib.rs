use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_liquid() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for Liquid templates.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_liquid) };
