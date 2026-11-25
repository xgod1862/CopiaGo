//! Chinese source strings, English translations, and deferred UI messages.

use std::{borrow::Cow, fmt};

include!(concat!(env!("OUT_DIR"), "/english.rs"));

/// The language stored in settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LanguagePreference {
    #[default]
    System,
    ZhCn,
    En,
}

impl LanguagePreference {
    /// Reads an existing setting, falling back to the system preference.
    pub fn from_setting(value: &str) -> Self {
        match value {
            "zh-CN" => Self::ZhCn,
            "en" => Self::En,
            _ => Self::System,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }

    /// Resolves the language for the current render pass.
    pub fn resolve(self) -> Language {
        self.resolve_with(system_language_id)
    }

    fn resolve_with(self, system_language_id: impl FnOnce() -> Option<u16>) -> Language {
        match self {
            Self::System => match system_language_id() {
                // The low ten bits of LANGID hold the primary language. Chinese is 0x04.
                Some(id) if id & 0x03ff == 0x04 => Language::ZhCn,
                _ => Language::En,
            },
            Self::ZhCn => Language::ZhCn,
            Self::En => Language::En,
        }
    }
}

fn system_language_id() -> Option<u16> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

        // SAFETY: GetUserDefaultUILanguage takes no parameters and returns a LANGID by value.
        Some(unsafe { GetUserDefaultUILanguage() })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A resolved interface language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    ZhCn,
    En,
}

impl Language {
    fn template(self, source: &str) -> &str {
        if self == Self::En
            && let Ok(index) = ENGLISH.binary_search_by_key(&source, |&(key, _)| key)
        {
            return ENGLISH[index].1;
        }
        source
    }
}

/// A named argument's value, either literal text or another deferred message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Argument {
    Text(String),
    Message(Box<Message>),
}

impl<T: fmt::Display> From<T> for Argument {
    fn from(value: T) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Message> for Argument {
    fn from(message: Message) -> Self {
        Self::Message(Box::new(message))
    }
}

/// A message that can cross threads and be rendered in the UI's current language.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    source: &'static str,
    values: Vec<(&'static str, Argument)>,
}

impl Message {
    /// Creates a message without named arguments. Usable in constants.
    pub const fn new(source: &'static str) -> Self {
        Self {
            source,
            values: Vec::new(),
        }
    }

    /// Stores the arguments with the message. See [`message!`] for named syntax.
    pub fn with_values(source: &'static str, values: Vec<(&'static str, Argument)>) -> Self {
        Self { source, values }
    }

    /// A fixed message that can be exported without including runtime arguments.
    pub fn literal_text(&self) -> Option<&'static str> {
        self.values.is_empty().then_some(self.source)
    }

    pub fn render(&self, language: Language) -> Cow<'static, str> {
        tr(language, self.source, &self.values)
    }
}

/// Translates a source string and substitutes its named fields.
///
/// A missing translation falls back to the Chinese source. When arguments are supplied,
/// doubled braces produce literal braces, and unmatched fields remain visible.
pub fn tr<'a>(language: Language, source: &'a str, values: &[(&str, Argument)]) -> Cow<'a, str> {
    let template = language.template(source);
    if values.is_empty() {
        return Cow::Borrowed(template);
    }

    let mut output = String::with_capacity(template.len());
    write_template(&mut output, language, template, values);
    Cow::Owned(output)
}

fn write_template(
    output: &mut String,
    language: Language,
    mut template: &str,
    values: &[(&str, Argument)],
) {
    if values.is_empty() {
        output.push_str(template);
        return;
    }

    while let Some(index) = template.find(['{', '}']) {
        output.push_str(&template[..index]);
        template = &template[index..];
        let brace = template.as_bytes()[0];

        if template.as_bytes().get(1) == Some(&brace) {
            output.push(char::from(brace));
            template = &template[2..];
        } else if let Some((name, rest)) =
            template.strip_prefix('{').and_then(|s| s.split_once('}'))
        {
            match values.iter().find(|(key, _)| *key == name) {
                Some((_, Argument::Text(text))) => output.push_str(text),
                Some((_, Argument::Message(message))) => {
                    write_template(
                        output,
                        language,
                        language.template(message.source),
                        &message.values,
                    );
                }
                None => output.push_str(&template[..template.len() - rest.len()]),
            }
            template = rest;
        } else {
            output.push(char::from(brace));
            template = &template[1..];
        }
    }
    output.push_str(template);
}

/// Translates immediately using named arguments.
///
/// ```
/// use copiago_core::i18n::{Language, tr};
///
/// assert_eq!(tr!(Language::En, "昨天 {time}", time = "18:04"), "Yesterday 18:04");
/// ```
#[macro_export]
macro_rules! tr {
    ($language:expr, $source:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::tr(
            $language,
            $source,
            &[$((stringify!($name), $crate::i18n::Argument::from($value))),*],
        )
    };
}

/// Creates a message with named arguments to translate later.
///
/// ```
/// use copiago_core::i18n::{Language, message};
///
/// let sent = message!("已发送{kind}到 iPhone", kind = message!("文本"));
/// assert_eq!(sent.render(Language::En), "Sent text to iPhone.");
/// assert_eq!(sent.render(Language::ZhCn), "已发送文本到 iPhone");
/// ```
#[macro_export]
macro_rules! message {
    ($source:expr $(,)?) => {
        $crate::i18n::Message::new($source)
    };
    ($source:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::Message::with_values(
            $source,
            vec![$((stringify!($name), $crate::i18n::Argument::from($value))),+],
        )
    };
}

pub use crate::{message, tr};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_the_python_english_catalog() {
        // Counted from the Python 1.2.0 ENGLISH catalog in i18n.py.
        const PYTHON_ENGLISH_ENTRIES: usize = 170;
        assert!(ENGLISH.len() >= PYTHON_ENGLISH_ENTRIES);
        for &(source, english) in ENGLISH {
            assert_eq!(tr!(Language::En, source), english);
            assert_eq!(tr!(Language::ZhCn, source), source);
        }
    }

    #[test]
    fn resolves_system_language_from_injected_langids() {
        for (id, expected) in [
            (Some(0x0804), Language::ZhCn), // Simplified Chinese
            (Some(0x0404), Language::ZhCn), // Traditional Chinese
            (Some(0x0c04), Language::ZhCn), // Hong Kong Chinese
            (Some(0x0409), Language::En),   // US English
            (Some(0x040c), Language::En),   // French
            (None, Language::En),
        ] {
            let language = LanguagePreference::System.resolve_with(|| id);
            assert_eq!(language, expected);
        }
    }

    #[test]
    fn reads_preferences_and_resolves_manual_choices_without_a_system_lookup() {
        for option in [
            LanguagePreference::System,
            LanguagePreference::ZhCn,
            LanguagePreference::En,
        ] {
            assert_eq!(LanguagePreference::from_setting(option.as_str()), option);
        }
        assert_eq!(
            LanguagePreference::from_setting("invalid"),
            LanguagePreference::System,
        );
        for (preference, language) in [
            (LanguagePreference::ZhCn, Language::ZhCn),
            (LanguagePreference::En, Language::En),
        ] {
            assert_eq!(
                preference.resolve_with(|| panic!("lookup called")),
                language
            );
        }
    }

    #[test]
    fn substitutes_named_fields_in_both_languages() {
        let source = "正在下载文件 {index}/{count} · {received} / {total}";
        assert_eq!(
            tr!(
                Language::En,
                source,
                received = "1 MB",
                total = "3 MB",
                count = 4,
                index = 2
            ),
            "Downloading file 2/4 · 1 MB / 3 MB",
        );
        assert_eq!(
            tr!(
                Language::ZhCn,
                source,
                index = 2,
                count = 4,
                received = "1 MB",
                total = "3 MB"
            ),
            "正在下载文件 2/4 · 1 MB / 3 MB",
        );
    }

    #[test]
    fn keeps_literals_and_interpolated_text_intact() {
        assert!(matches!(
            tr!(Language::En, "未知文本"),
            Cow::Borrowed("未知文本")
        ));
        assert_eq!(tr!(Language::En, "昨天 {time}"), "Yesterday {time}");
        assert_eq!(
            tr!(
                Language::En,
                "{{{name}}} {name} {missing}",
                name = "🖼️{missing}"
            ),
            "{🖼️{missing}} 🖼️{missing} {missing}",
        );
    }

    #[test]
    fn renders_nested_messages_in_the_selected_language() {
        let message = message!(
            "已同步{kind} · 可直接粘贴{note}",
            kind = message!("文本"),
            note = message!(" · 项目过大，未加入历史"),
        );
        let message = std::thread::spawn(move || message).join().unwrap();
        assert_eq!(
            message.render(Language::En),
            "Synced text · Ready to paste · Item too large for history",
        );
        assert_eq!(
            message.render(Language::ZhCn),
            "已同步文本 · 可直接粘贴 · 项目过大，未加入历史",
        );
    }
}
