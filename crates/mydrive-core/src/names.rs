//! OneDrive and SharePoint name and path validation (ADR-0015).
//!
//! Linux accepts almost any byte string as a file name; OneDrive and SharePoint do not. This
//! module decides, before any upload is attempted, whether a local name can exist on the
//! server, and says exactly why not (ADR-0015 "Names", flow F04 step 12, journey J05 V3/V4).
//! It only reports: invalid names are never renamed or "fixed" here.
//!
//! # Sources
//!
//! Every rule cites the research note
//! `research_notes/OneDrive Linux on demand clients/sharepoint_libraries.md`, section
//! "Names, paths, types", which records Microsoft's page
//! [Restrictions and limitations in OneDrive and SharePoint][restrictions] and the
//! [rclone OneDrive docs][rclone]. Where a rule was ambiguous the stricter reading is
//! implemented and the rule's doc comment says so.
//!
//! # Rules from the research note not implemented here, and why
//!
//! - **File types not synced by Microsoft's client** (`.tmp`, `.ds_store`, `.PST` cadence,
//!   admin-blocked file types): these are behaviours of Microsoft's sync app or per-tenant
//!   policy, not server name restrictions. The server accepts such names.
//! - **Symbolic links and junction points**: a property of the local entry, not of its name.
//! - **Site names containing `:`**: concerns how Microsoft's client names the local folder for
//!   a library, not names uploaded into it.
//! - **Case-sensitive volumes**: a limitation of Microsoft's client; the server-side
//!   consequence (case-insensitive namespace) is implemented by [`find_case_collisions`].
//! - **Non-UTF-8 names**: the overview rules that the VFS refuses them at creation, so this
//!   module takes `&str`.
//! - **Unicode normalisation** (NFC versus NFD): the note says nothing about it, so names that
//!   differ only in normalisation are not reported as collisions.
//!
//! [restrictions]: https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa
//! [rclone]: https://rclone.org/onedrive/

use std::collections::HashMap;
use std::fmt;

/// Which kind of account the drive belongs to. Some rules apply only to work accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    /// A personal Microsoft account and its personal OneDrive.
    Personal,
    /// A work or school account: OneDrive for Business and SharePoint document libraries.
    Work,
}

/// Where a name is going to live on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameContext {
    /// The account kind of the drive the item belongs to.
    pub account: AccountKind,
    /// Whether the item sits directly in the root folder of its drive. A work drive, including
    /// OneDrive for Business, is a SharePoint document library, so its root is a library root.
    pub at_library_root: bool,
}

/// Characters the server rejects anywhere in a name, for every account kind.
///
/// Source: research note, "Disallowed characters: `" * : < > ? / \ |`", citing Microsoft's
/// restrictions page.
pub const FORBIDDEN_CHARACTERS: [char; 9] = ['"', '*', ':', '<', '>', '?', '/', '\\', '|'];

/// Characters rejected in work accounts only.
///
/// Source: research note, "some organisations do not yet support `#` and `%`". Whether a given
/// organisation supports them cannot be known from the name, so the stricter reading applies:
/// they are rejected in every work account. Personal accounts are not named by the source and
/// accept them.
pub const WORK_FORBIDDEN_CHARACTERS: [char; 2] = ['#', '%'];

/// Reserved device names, rejected as the whole name or as the part before the first `.`.
///
/// Source: research note, "Reserved names: ... `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
/// `LPT0`–`LPT9`". The source does not say whether a name with an extension (`CON.txt`) or a
/// different case (`con`) is also reserved; the stricter reading rejects both, as Windows does.
pub const RESERVED_DEVICE_NAMES: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Names reserved as a whole.
///
/// Source: research note, "Reserved names: `.lock`, ... `desktop.ini`". They are matched as
/// the whole name, case-insensitively (stricter reading, since the namespace is
/// case-insensitive). The source lists them among names, not extensions, so `Cargo.lock` is
/// accepted; this reading is unverified.
pub const RESERVED_WHOLE_NAMES: [&str; 2] = [".lock", "desktop.ini"];

/// Prefix the server rejects.
///
/// Source: research note, "any name starting with `~$`".
pub const FORBIDDEN_PREFIX: &str = "~$";

/// Substring the server rejects anywhere in a name.
///
/// Source: research note, "`_vti_` cannot appear anywhere in a name". Matched
/// case-insensitively (stricter reading; the source does not state the case rule). Applied to
/// personal accounts too, since the source does not restrict it to SharePoint.
pub const FORBIDDEN_SUBSTRING: &str = "_vti_";

/// Name the server rejects at a library root in work accounts.
///
/// Source: research note, "`forms` cannot be used for a file or folder at the library root".
/// Matched case-insensitively (stricter reading).
pub const LIBRARY_ROOT_RESERVED_NAME: &str = "forms";

/// The server path must be shorter than this many characters.
///
/// Source: research note, the path must be "fewer than 400 characters for OneDrive, OneDrive
/// for Business and SharePoint Online" (rclone OneDrive docs).
pub const SERVER_PATH_LIMIT: usize = 400;

/// Why a name or path cannot exist on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameProblem {
    /// The name is empty.
    Empty,
    /// The name contains a character the server rejects.
    ForbiddenCharacter(char),
    /// The name is, or starts with, a reserved name.
    ReservedName(String),
    /// The name starts with a prefix the server rejects.
    ForbiddenPrefix(&'static str),
    /// The name contains a substring the server rejects.
    ForbiddenSubstring(&'static str),
    /// The name is reserved at a library root.
    ReservedAtLibraryRoot(&'static str),
    /// The name starts with a space.
    LeadingSpace,
    /// The name ends with a space.
    TrailingSpace,
    /// The name ends with a period.
    TrailingPeriod,
    /// The server path is too long.
    PathTooLong {
        /// Length counted in UTF-16 code units.
        length: usize,
        /// The path must be shorter than this.
        limit: usize,
    },
    /// Another name in the same folder differs from this one only by case.
    CaseCollision {
        /// The other names in the folder that collide with this one.
        with: Vec<String>,
    },
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the name is empty"),
            Self::ForbiddenCharacter(c) => write!(f, "it contains the character {c:?}"),
            Self::ReservedName(r) => write!(f, "{r:?} is a reserved name"),
            Self::ForbiddenPrefix(p) => write!(f, "names may not start with {p:?}"),
            Self::ForbiddenSubstring(s) => write!(f, "names may not contain {s:?}"),
            Self::ReservedAtLibraryRoot(r) => {
                write!(f, "{r:?} is reserved at the root of a library")
            }
            Self::LeadingSpace => write!(f, "names may not start with a space"),
            Self::TrailingSpace => write!(f, "names may not end with a space"),
            Self::TrailingPeriod => write!(f, "names may not end with a period"),
            Self::PathTooLong { length, limit } => write!(
                f,
                "the server path is {length} characters long and must be fewer than {limit}"
            ),
            Self::CaseCollision { with } => {
                write!(
                    f,
                    "it differs only by case from {with:?} in the same folder"
                )
            }
        }
    }
}

/// A name or server path that cannot exist on the server, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{name:?} cannot be uploaded: {problem}")]
pub struct NameError {
    /// The rejected name, or the rejected server path for [`NameProblem::PathTooLong`].
    pub name: String,
    /// Why it was rejected.
    pub problem: NameProblem,
}

impl NameError {
    fn new(name: &str, problem: NameProblem) -> Self {
        Self {
            name: name.to_owned(),
            problem,
        }
    }
}

/// Checks whether a single name (one path component) can exist on the server in `context`.
///
/// Returns the first problem found. The checks, each citing its source on the constant or
/// variant it uses, are: empty name; [`FORBIDDEN_CHARACTERS`]; [`WORK_FORBIDDEN_CHARACTERS`]
/// in work accounts; leading space, trailing space, trailing period; [`FORBIDDEN_PREFIX`];
/// [`FORBIDDEN_SUBSTRING`]; [`RESERVED_WHOLE_NAMES`]; [`RESERVED_DEVICE_NAMES`]; and
/// [`LIBRARY_ROOT_RESERVED_NAME`] at a work library root.
///
/// Leading and trailing spaces: research note, "no leading or trailing spaces". Only U+0020
/// is checked; the source names no other whitespace.
///
/// Trailing period: not in the research note. Flow F04 step 12 and this work package list it,
/// and Windows rejects it, so the stricter reading rejects it. Unverified against Microsoft's
/// page.
pub fn validate_name(name: &str, context: NameContext) -> Result<(), NameError> {
    let fail = |problem| Err(NameError::new(name, problem));

    if name.is_empty() {
        return fail(NameProblem::Empty);
    }
    if let Some(c) = name.chars().find(|c| FORBIDDEN_CHARACTERS.contains(c)) {
        return fail(NameProblem::ForbiddenCharacter(c));
    }
    if context.account == AccountKind::Work
        && let Some(c) = name.chars().find(|c| WORK_FORBIDDEN_CHARACTERS.contains(c))
    {
        return fail(NameProblem::ForbiddenCharacter(c));
    }
    if name.starts_with(' ') {
        return fail(NameProblem::LeadingSpace);
    }
    if name.ends_with(' ') {
        return fail(NameProblem::TrailingSpace);
    }
    if name.ends_with('.') {
        return fail(NameProblem::TrailingPeriod);
    }
    if name.starts_with(FORBIDDEN_PREFIX) {
        return fail(NameProblem::ForbiddenPrefix(FORBIDDEN_PREFIX));
    }
    let lower = name.to_lowercase();
    if lower.contains(FORBIDDEN_SUBSTRING) {
        return fail(NameProblem::ForbiddenSubstring(FORBIDDEN_SUBSTRING));
    }
    if let Some(reserved) = RESERVED_WHOLE_NAMES.iter().find(|r| lower == **r) {
        return fail(NameProblem::ReservedName((*reserved).to_owned()));
    }
    // Windows also treats "CON .txt" as CON, so trailing spaces before the dot are ignored.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if let Some(reserved) = RESERVED_DEVICE_NAMES
        .iter()
        .find(|r| stem.eq_ignore_ascii_case(r))
    {
        return fail(NameProblem::ReservedName((*reserved).to_owned()));
    }
    if context.account == AccountKind::Work
        && context.at_library_root
        && lower == LIBRARY_ROOT_RESERVED_NAME
    {
        return fail(NameProblem::ReservedAtLibraryRoot(
            LIBRARY_ROOT_RESERVED_NAME,
        ));
    }
    Ok(())
}

/// Checks a full server-relative path against the [`SERVER_PATH_LIMIT`].
///
/// The research note says the counting rule (decoded or encoded, which segments are included)
/// was not confirmed, so this rule is unverified. The caller passes the server path exactly as
/// it will be sent, already encoded, and for a library including the site and library
/// segments (research note: "for a library includes the site and library path segments").
/// The length is counted in UTF-16 code units, which is never less than the number of Unicode
/// characters, so this is the stricter reading. A path of exactly 400 units is rejected
/// ("fewer than 400").
pub fn validate_server_path(path: &str) -> Result<(), NameError> {
    let length = path.encode_utf16().count();
    if length >= SERVER_PATH_LIMIT {
        return Err(NameError::new(
            path,
            NameProblem::PathTooLong {
                length,
                limit: SERVER_PATH_LIMIT,
            },
        ));
    }
    Ok(())
}

/// Finds names in one folder that differ only by case.
///
/// The server namespace is case-insensitive (research note: "SharePoint is case-insensitive,
/// so two local files differing only in case must be surfaced as a conflict"). Returns one
/// error per colliding name, in input order, each listing the other names it collides with.
/// Identical names passed twice are also reported.
///
/// The comparison approximates Unicode simple case folding with the standard library's case
/// mappings: each character is upper-cased then lower-cased, but only where each mapping is
/// one character to one character, as simple folding is. This folds `ς`/`σ`/`Σ` and `ſ`/`s`
/// together and keeps `ß` distinct from `ss`. The server's exact comparison rule is not
/// documented in the sources, so this is an approximation.
pub fn find_case_collisions<'a, I>(names: I) -> Vec<NameError>
where
    I: IntoIterator<Item = &'a str>,
{
    let names: Vec<&str> = names.into_iter().collect();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, name) in names.iter().enumerate() {
        groups.entry(fold_case(name)).or_default().push(index);
    }
    let mut errors = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let Some(group) = groups.get(&fold_case(name)) else {
            continue;
        };
        if group.len() < 2 {
            continue;
        }
        let with = group
            .iter()
            .filter(|&&other| other != index)
            .map(|&other| names[other].to_owned())
            .collect();
        errors.push(NameError::new(name, NameProblem::CaseCollision { with }));
    }
    errors
}

fn fold_case(name: &str) -> String {
    name.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    let upper = single(c.to_uppercase()).unwrap_or(c);
    single(upper.to_lowercase())
        .or_else(|| single(c.to_lowercase()))
        .unwrap_or(c)
}

fn single(mut mapping: impl Iterator<Item = char>) -> Option<char> {
    let first = mapping.next()?;
    match mapping.next() {
        None => Some(first),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const PERSONAL: NameContext = NameContext {
        account: AccountKind::Personal,
        at_library_root: false,
    };
    const PERSONAL_ROOT: NameContext = NameContext {
        account: AccountKind::Personal,
        at_library_root: true,
    };
    const WORK: NameContext = NameContext {
        account: AccountKind::Work,
        at_library_root: false,
    };
    const WORK_ROOT: NameContext = NameContext {
        account: AccountKind::Work,
        at_library_root: true,
    };
    const ALL_CONTEXTS: [NameContext; 4] = [PERSONAL, PERSONAL_ROOT, WORK, WORK_ROOT];

    fn problem(name: &str, context: NameContext) -> Option<NameProblem> {
        validate_name(name, context).err().map(|e| e.problem)
    }

    /// AC1, ADR-0015 "Names": reserved characters. Each is rejected and named.
    #[test]
    fn forbidden_characters_are_rejected_and_named() {
        for c in FORBIDDEN_CHARACTERS {
            for name in [format!("{c}"), format!("a{c}b"), format!("Q3{c} plan.txt")] {
                for context in ALL_CONTEXTS {
                    assert_eq!(
                        problem(&name, context),
                        Some(NameProblem::ForbiddenCharacter(c)),
                        "{name:?} in {context:?}"
                    );
                }
            }
        }
    }

    /// J05-AC9: the error for `Q3: plan.txt` names the invalid character.
    #[test]
    fn error_message_names_the_character() {
        let err = validate_name("Q3: plan.txt", PERSONAL).unwrap_err();
        assert_eq!(err.name, "Q3: plan.txt");
        assert!(err.to_string().contains("':'"), "{err}");
    }

    /// AC1: `#` and `%` are rejected in work accounts (stricter reading) and accepted in
    /// personal accounts.
    #[test]
    fn hash_and_percent_depend_on_account_kind() {
        for c in WORK_FORBIDDEN_CHARACTERS {
            let name = format!("100{c} done");
            assert_eq!(problem(&name, PERSONAL), None);
            assert_eq!(problem(&name, PERSONAL_ROOT), None);
            assert_eq!(
                problem(&name, WORK),
                Some(NameProblem::ForbiddenCharacter(c))
            );
            assert_eq!(
                problem(&name, WORK_ROOT),
                Some(NameProblem::ForbiddenCharacter(c))
            );
        }
    }

    /// AC2, ADR-0015 "Names": reserved device names, any case, with or without an extension.
    #[test]
    fn reserved_device_names_are_rejected() {
        for reserved in RESERVED_DEVICE_NAMES {
            let lower = reserved.to_lowercase();
            let mixed: String = reserved
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if i % 2 == 0 {
                        c
                    } else {
                        c.to_ascii_lowercase()
                    }
                })
                .collect();
            let cases = [
                reserved.to_owned(),
                lower.clone(),
                mixed,
                format!("{reserved}.txt"),
                format!("{lower}.tar.gz"),
                format!("{lower} .txt"),
            ];
            for name in cases {
                for context in ALL_CONTEXTS {
                    assert_eq!(
                        problem(&name, context),
                        Some(NameProblem::ReservedName(reserved.to_owned())),
                        "{name:?} in {context:?}"
                    );
                }
            }
        }
    }

    /// AC2: names that only contain or extend a device name are accepted.
    #[test]
    fn names_resembling_device_names_are_accepted() {
        for name in [
            "CONSOLE",
            "console.log",
            "COM10",
            "LPT",
            "auxiliary.txt",
            "my CON",
        ] {
            for context in ALL_CONTEXTS {
                assert_eq!(problem(name, context), None, "{name:?}");
            }
        }
    }

    /// ADR-0015 "Names": `.lock` and `desktop.ini` are rejected as whole names, any case.
    #[test]
    fn reserved_whole_names_are_rejected() {
        let cases = [
            (".lock", ".lock"),
            (".LOCK", ".lock"),
            ("desktop.ini", "desktop.ini"),
            ("Desktop.INI", "desktop.ini"),
        ];
        for (name, reserved) in cases {
            for context in ALL_CONTEXTS {
                assert_eq!(
                    problem(name, context),
                    Some(NameProblem::ReservedName(reserved.to_owned())),
                    "{name:?}"
                );
            }
        }
        for name in ["Cargo.lock", "desktop.ini.bak", "my desktop.ini"] {
            assert_eq!(problem(name, WORK_ROOT), None, "{name:?}");
        }
    }

    /// ADR-0015 "Names": the `~$` prefix is rejected; `~$` elsewhere is accepted.
    #[test]
    fn tilde_dollar_prefix_is_rejected() {
        for name in ["~$report.docx", "~$"] {
            for context in ALL_CONTEXTS {
                assert_eq!(
                    problem(name, context),
                    Some(NameProblem::ForbiddenPrefix("~$")),
                    "{name:?}"
                );
            }
        }
        assert_eq!(problem("report~$.docx", PERSONAL), None);
        assert_eq!(problem("~report.docx", PERSONAL), None);
    }

    /// ADR-0015 "Names": `_vti_` is rejected anywhere in a name, any case.
    #[test]
    fn vti_substring_is_rejected() {
        for name in ["_vti_", "_vti_cnf", "a_vti_b.txt", "x_VTI_", "_Vti_pvt"] {
            for context in ALL_CONTEXTS {
                assert_eq!(
                    problem(name, context),
                    Some(NameProblem::ForbiddenSubstring("_vti_")),
                    "{name:?}"
                );
            }
        }
        assert_eq!(problem("_vti", PERSONAL), None);
        assert_eq!(problem("vti_", PERSONAL), None);
    }

    /// AC3, ADR-0015 "Names": `forms` is rejected only at a work library root.
    #[test]
    fn forms_is_rejected_only_at_a_work_library_root() {
        for name in ["forms", "Forms", "FORMS"] {
            assert_eq!(
                problem(name, WORK_ROOT),
                Some(NameProblem::ReservedAtLibraryRoot("forms"))
            );
            assert_eq!(problem(name, WORK), None);
            assert_eq!(problem(name, PERSONAL), None);
            assert_eq!(problem(name, PERSONAL_ROOT), None);
        }
        assert_eq!(problem("forms.docx", WORK_ROOT), None);
        assert_eq!(problem("my forms", WORK_ROOT), None);
    }

    /// ADR-0015 "Names": leading and trailing spaces and trailing periods are rejected.
    #[test]
    fn leading_and_trailing_characters_are_rejected() {
        let cases = [
            (" report.txt", NameProblem::LeadingSpace),
            ("  ", NameProblem::LeadingSpace),
            ("report.txt ", NameProblem::TrailingSpace),
            ("report.", NameProblem::TrailingPeriod),
            ("report...", NameProblem::TrailingPeriod),
            (".", NameProblem::TrailingPeriod),
            ("..", NameProblem::TrailingPeriod),
        ];
        for (name, expected) in cases {
            for context in ALL_CONTEXTS {
                assert_eq!(problem(name, context), Some(expected.clone()), "{name:?}");
            }
        }
    }

    /// An empty name is rejected.
    #[test]
    fn empty_name_is_rejected() {
        for context in ALL_CONTEXTS {
            assert_eq!(problem("", context), Some(NameProblem::Empty));
        }
    }

    /// AC4: names valid on Linux and on the server are accepted in every context.
    #[test]
    fn valid_names_are_accepted() {
        let names = [
            "report.txt",
            "Quarterly report 2026.xlsx",
            "a.b.c.d",
            ".bashrc",
            ".config",
            "Cargo.lock",
            "naïve café.md",
            "日本語のファイル.txt",
            "Ελληνικά",
            "😀 holiday 🏖️.jpg",
            "x",
            "with  two  spaces",
            "tab\tinside",
            "dash-and_underscore",
            "~tilde",
            "$dollar",
            "Formsheet",
        ];
        for name in names {
            for context in ALL_CONTEXTS {
                assert_eq!(problem(name, context), None, "{name:?} in {context:?}");
            }
        }
    }

    /// AC5, ADR-0015 "Names": the 400-character limit, counted in UTF-16 code units, exclusive.
    #[test]
    fn server_path_length_limit() {
        let ok = "a".repeat(399);
        assert_eq!(validate_server_path(&ok), Ok(()));
        let at_limit = "a".repeat(400);
        assert_eq!(
            validate_server_path(&at_limit).unwrap_err().problem,
            NameProblem::PathTooLong {
                length: 400,
                limit: 400
            }
        );
        // An emoji outside the Basic Multilingual Plane counts as two UTF-16 units.
        let astral = format!("{}{}", "a".repeat(398), "😀");
        assert_eq!(astral.chars().count(), 399);
        assert_eq!(
            validate_server_path(&astral).unwrap_err().problem,
            NameProblem::PathTooLong {
                length: 400,
                limit: 400
            }
        );
        // A BMP character such as `é` counts once, though it is two bytes in UTF-8.
        let bmp = "é".repeat(399);
        assert_eq!(validate_server_path(&bmp), Ok(()));
        assert_eq!(validate_server_path("/Documents/report.txt"), Ok(()));
    }

    /// AC6, ADR-0015 "Names": names differing only by case collide.
    #[test]
    fn case_collisions_are_reported() {
        let errors = find_case_collisions(["Report.txt", "notes.md", "report.txt"]);
        assert_eq!(
            errors,
            vec![
                NameError::new(
                    "Report.txt",
                    NameProblem::CaseCollision {
                        with: vec!["report.txt".to_owned()]
                    }
                ),
                NameError::new(
                    "report.txt",
                    NameProblem::CaseCollision {
                        with: vec!["Report.txt".to_owned()]
                    }
                ),
            ]
        );
    }

    /// AC6: the comparison is Unicode-aware, approximating simple case folding.
    #[test]
    fn case_collisions_are_unicode_aware() {
        let colliding: [(&str, &str); 5] = [
            ("ÉTÉ.txt", "été.txt"),
            ("ΣΟΦΙΑ", "σοφια"),
            ("Οδός", "ΟΔΌΣ"),
            ("ſtraße", "Straße"),
            ("МОСКВА", "москва"),
        ];
        for (a, b) in colliding {
            assert_eq!(find_case_collisions([a, b]).len(), 2, "{a:?} vs {b:?}");
        }
        // Simple folding keeps ß distinct from ss; distinct names do not collide.
        let distinct: [(&str, &str); 3] = [("straße", "strasse"), ("a.txt", "b.txt"), ("e", "é")];
        for (a, b) in distinct {
            assert!(find_case_collisions([a, b]).is_empty(), "{a:?} vs {b:?}");
        }
    }

    /// AC6: three-way collisions list all the others.
    #[test]
    fn case_collision_lists_every_other_name() {
        let errors = find_case_collisions(["A", "a", "b"]);
        assert_eq!(errors.len(), 2);
        let errors = find_case_collisions(["README", "readme", "ReadMe"]);
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors[0].problem,
            NameProblem::CaseCollision {
                with: vec!["readme".to_owned(), "ReadMe".to_owned()]
            }
        );
        assert!(find_case_collisions(Vec::<&str>::new()).is_empty());
    }

    fn reserved_word(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or(&lower).trim_end();
        RESERVED_DEVICE_NAMES
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
            || lower == LIBRARY_ROOT_RESERVED_NAME
    }

    proptest! {
        /// Any name of ASCII letters, digits and inner spaces that is not a reserved word and is
        /// within length limits is accepted in every context.
        #[test]
        fn plain_ascii_names_are_accepted(
            name in "[A-Za-z0-9]([A-Za-z0-9 ]{0,200}[A-Za-z0-9])?"
                .prop_filter("not a reserved word", |n| !reserved_word(n))
        ) {
            for context in ALL_CONTEXTS {
                prop_assert_eq!(validate_name(&name, context), Ok(()));
            }
            prop_assert_eq!(validate_server_path(&format!("/{name}")), Ok(()));
        }
    }
}
