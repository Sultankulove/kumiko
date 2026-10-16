//! The Java grammar (tree-sitter-java, MIT) as a native library for the Syntax service, which
//! loads it with libloading (spec, section 18). The symbol has its own name: the grammar's C
//! constructor is not exported from a Rust `cdylib` on every platform.

#![allow(unsafe_code)]

/// The grammar's `TSLanguage`, for `tree_sitter::Language::from_raw`.
#[unsafe(no_mangle)]
pub extern "C" fn kumiko_grammar_language() -> *const core::ffi::c_void {
    // SAFETY: the grammar's own constructor takes no arguments and returns a static table.
    unsafe { (tree_sitter_java::LANGUAGE.into_raw())() }.cast()
}
