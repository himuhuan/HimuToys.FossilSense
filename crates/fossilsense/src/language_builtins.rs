#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LanguageBuiltinCategory {
    Keyword,
    BuiltinType,
    BuiltinConstant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LanguageBuiltin {
    pub label: &'static str,
    pub category: LanguageBuiltinCategory,
}

const LANGUAGE_BUILTINS: &[LanguageBuiltin] = &[
    builtin("auto", LanguageBuiltinCategory::Keyword),
    builtin("break", LanguageBuiltinCategory::Keyword),
    builtin("case", LanguageBuiltinCategory::Keyword),
    builtin("catch", LanguageBuiltinCategory::Keyword),
    builtin("class", LanguageBuiltinCategory::Keyword),
    builtin("const", LanguageBuiltinCategory::Keyword),
    builtin("continue", LanguageBuiltinCategory::Keyword),
    builtin("default", LanguageBuiltinCategory::Keyword),
    builtin("defined", LanguageBuiltinCategory::Keyword),
    builtin("delete", LanguageBuiltinCategory::Keyword),
    builtin("do", LanguageBuiltinCategory::Keyword),
    builtin("dynamic_cast", LanguageBuiltinCategory::Keyword),
    builtin("else", LanguageBuiltinCategory::Keyword),
    builtin("enum", LanguageBuiltinCategory::Keyword),
    builtin("explicit", LanguageBuiltinCategory::Keyword),
    builtin("extern", LanguageBuiltinCategory::Keyword),
    builtin("for", LanguageBuiltinCategory::Keyword),
    builtin("friend", LanguageBuiltinCategory::Keyword),
    builtin("goto", LanguageBuiltinCategory::Keyword),
    builtin("if", LanguageBuiltinCategory::Keyword),
    builtin("inline", LanguageBuiltinCategory::Keyword),
    builtin("mutable", LanguageBuiltinCategory::Keyword),
    builtin("namespace", LanguageBuiltinCategory::Keyword),
    builtin("new", LanguageBuiltinCategory::Keyword),
    builtin("operator", LanguageBuiltinCategory::Keyword),
    builtin("private", LanguageBuiltinCategory::Keyword),
    builtin("protected", LanguageBuiltinCategory::Keyword),
    builtin("public", LanguageBuiltinCategory::Keyword),
    builtin("register", LanguageBuiltinCategory::Keyword),
    builtin("reinterpret_cast", LanguageBuiltinCategory::Keyword),
    builtin("restrict", LanguageBuiltinCategory::Keyword),
    builtin("return", LanguageBuiltinCategory::Keyword),
    builtin("sizeof", LanguageBuiltinCategory::Keyword),
    builtin("static", LanguageBuiltinCategory::Keyword),
    builtin("static_assert", LanguageBuiltinCategory::Keyword),
    builtin("static_cast", LanguageBuiltinCategory::Keyword),
    builtin("struct", LanguageBuiltinCategory::Keyword),
    builtin("switch", LanguageBuiltinCategory::Keyword),
    builtin("template", LanguageBuiltinCategory::Keyword),
    builtin("this", LanguageBuiltinCategory::Keyword),
    builtin("throw", LanguageBuiltinCategory::Keyword),
    builtin("try", LanguageBuiltinCategory::Keyword),
    builtin("typedef", LanguageBuiltinCategory::Keyword),
    builtin("typeid", LanguageBuiltinCategory::Keyword),
    builtin("typename", LanguageBuiltinCategory::Keyword),
    builtin("union", LanguageBuiltinCategory::Keyword),
    builtin("using", LanguageBuiltinCategory::Keyword),
    builtin("virtual", LanguageBuiltinCategory::Keyword),
    builtin("volatile", LanguageBuiltinCategory::Keyword),
    builtin("while", LanguageBuiltinCategory::Keyword),
    builtin("alignas", LanguageBuiltinCategory::Keyword),
    builtin("alignof", LanguageBuiltinCategory::Keyword),
    builtin("and", LanguageBuiltinCategory::Keyword),
    builtin("and_eq", LanguageBuiltinCategory::Keyword),
    builtin("asm", LanguageBuiltinCategory::Keyword),
    builtin("atomic_cancel", LanguageBuiltinCategory::Keyword),
    builtin("atomic_commit", LanguageBuiltinCategory::Keyword),
    builtin("atomic_noexcept", LanguageBuiltinCategory::Keyword),
    builtin("bitand", LanguageBuiltinCategory::Keyword),
    builtin("bitor", LanguageBuiltinCategory::Keyword),
    builtin("compl", LanguageBuiltinCategory::Keyword),
    builtin("concept", LanguageBuiltinCategory::Keyword),
    builtin("const_cast", LanguageBuiltinCategory::Keyword),
    builtin("constexpr", LanguageBuiltinCategory::Keyword),
    builtin("decltype", LanguageBuiltinCategory::Keyword),
    builtin("not", LanguageBuiltinCategory::Keyword),
    builtin("not_eq", LanguageBuiltinCategory::Keyword),
    builtin("or", LanguageBuiltinCategory::Keyword),
    builtin("or_eq", LanguageBuiltinCategory::Keyword),
    builtin("override", LanguageBuiltinCategory::Keyword),
    builtin("final", LanguageBuiltinCategory::Keyword),
    builtin("thread_local", LanguageBuiltinCategory::Keyword),
    builtin("xor", LanguageBuiltinCategory::Keyword),
    builtin("xor_eq", LanguageBuiltinCategory::Keyword),
    builtin("bool", LanguageBuiltinCategory::BuiltinType),
    builtin("char", LanguageBuiltinCategory::BuiltinType),
    builtin("char16_t", LanguageBuiltinCategory::BuiltinType),
    builtin("char32_t", LanguageBuiltinCategory::BuiltinType),
    builtin("double", LanguageBuiltinCategory::BuiltinType),
    builtin("float", LanguageBuiltinCategory::BuiltinType),
    builtin("int", LanguageBuiltinCategory::BuiltinType),
    builtin("long", LanguageBuiltinCategory::BuiltinType),
    builtin("short", LanguageBuiltinCategory::BuiltinType),
    builtin("signed", LanguageBuiltinCategory::BuiltinType),
    builtin("unsigned", LanguageBuiltinCategory::BuiltinType),
    builtin("void", LanguageBuiltinCategory::BuiltinType),
    builtin("wchar_t", LanguageBuiltinCategory::BuiltinType),
    builtin("size_t", LanguageBuiltinCategory::BuiltinType),
    builtin("ptrdiff_t", LanguageBuiltinCategory::BuiltinType),
    builtin("intptr_t", LanguageBuiltinCategory::BuiltinType),
    builtin("uintptr_t", LanguageBuiltinCategory::BuiltinType),
    builtin("int8_t", LanguageBuiltinCategory::BuiltinType),
    builtin("int16_t", LanguageBuiltinCategory::BuiltinType),
    builtin("int32_t", LanguageBuiltinCategory::BuiltinType),
    builtin("int64_t", LanguageBuiltinCategory::BuiltinType),
    builtin("uint8_t", LanguageBuiltinCategory::BuiltinType),
    builtin("uint16_t", LanguageBuiltinCategory::BuiltinType),
    builtin("uint32_t", LanguageBuiltinCategory::BuiltinType),
    builtin("uint64_t", LanguageBuiltinCategory::BuiltinType),
    builtin("wint_t", LanguageBuiltinCategory::BuiltinType),
    builtin("NULL", LanguageBuiltinCategory::BuiltinConstant),
    builtin("false", LanguageBuiltinCategory::BuiltinConstant),
    builtin("true", LanguageBuiltinCategory::BuiltinConstant),
    builtin("nullptr", LanguageBuiltinCategory::BuiltinConstant),
];

const fn builtin(label: &'static str, category: LanguageBuiltinCategory) -> LanguageBuiltin {
    LanguageBuiltin { label, category }
}

pub(crate) fn language_builtins() -> &'static [LanguageBuiltin] {
    LANGUAGE_BUILTINS
}

pub(crate) fn reserved_word_labels() -> impl Iterator<Item = &'static str> {
    LANGUAGE_BUILTINS.iter().map(|builtin| builtin.label)
}

pub(crate) fn is_language_keyword(label: &str) -> bool {
    LANGUAGE_BUILTINS.iter().any(|builtin| {
        builtin.category == LanguageBuiltinCategory::Keyword && builtin.label == label
    })
}

#[cfg(test)]
mod tests {
    use super::is_language_keyword;

    #[test]
    fn keyword_gate_does_not_reject_builtin_like_typedef_names() {
        assert!(is_language_keyword("const"));
        assert!(is_language_keyword("class"));
        assert!(!is_language_keyword("size_t"));
        assert!(!is_language_keyword("AVTextWriter"));
    }
}
