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
//! [rclone OneDrive docs][rclone].
//!
//! # Doubtful rules
//!
//! The server is the final authority. A name wrongly accepted here fails at upload and is
//! shown as an error; a name wrongly rejected here never syncs. So where the source is silent
//! or ambiguous this module accepts, and the rule's doc comment records the doubt:
//!
//! - `#` and `%`: the source says only "some organisations" refuse them. They are accepted
//!   unless [`NameContext::hash_and_percent_allowed`] is false for a work account.
//! - Reserved device names: only the bare names are rejected (`CON`, any case). The source
//!   does not say whether `CON.txt` is reserved, so it is accepted.
//! - `.lock` and `desktop.ini`: rejected as whole names only, so `Cargo.lock` is accepted.
//! - Case: `_vti_`, `forms`, `.lock`, `desktop.ini` and device names are matched
//!   case-insensitively. The source does not state a case rule; the server namespace is
//!   case-insensitive, so a different-case spelling is taken to name the same thing.
//! - Trailing period: not in the source, so it is accepted (flow F04 step 12 lists it; the
//!   lead ruled it out as unsourced).
//! - The 400-character path limit: how it is counted is unverified (see
//!   [`validate_server_path`]).
//! - Case folding: an approximation of the server's comparison (see
//!   [`find_case_collisions`]).
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
    /// Whether the tenant accepts `#` and `%` in names. True in the normal case; the engine
    /// sets it false for a tenant known to refuse them. Ignored for personal accounts.
    pub hash_and_percent_allowed: bool,
}

/// Characters the server rejects anywhere in a name, for every account kind.
///
/// Source: research note, "Disallowed characters: `" * : < > ? / \ |`", citing Microsoft's
/// restrictions page.
pub const FORBIDDEN_CHARACTERS: [char; 9] = ['"', '*', ':', '<', '>', '?', '/', '\\', '|'];

/// Characters some work tenants refuse.
///
/// Source: research note, "some organisations do not yet support `#` and `%`". Which
/// organisations cannot be known from the name, so they are accepted unless the context says
/// the tenant refuses them ([`NameContext::hash_and_percent_allowed`]). Personal accounts are
/// not organisations and always accept them.
pub const TENANT_RESTRICTED_CHARACTERS: [char; 2] = ['#', '%'];

/// Reserved device names, rejected when they are the whole name, in any case.
///
/// Source: research note, "Reserved names: ... `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
/// `LPT0`–`LPT9`". The source does not say whether a name with an extension (`CON.txt`) is
/// also reserved, as it is on Windows; it is accepted here and left to the server. Matching is
/// case-insensitive because the server namespace is.
pub const RESERVED_DEVICE_NAMES: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Names reserved as a whole.
///
/// Source: research note, "Reserved names: `.lock`, ... `desktop.ini`". They are matched as
/// the whole name, case-insensitively, because the server namespace is case-insensitive. The
/// source lists them among names, not extensions, so `Cargo.lock` is accepted.
pub const RESERVED_WHOLE_NAMES: [&str; 2] = [".lock", "desktop.ini"];

/// Prefix the server rejects.
///
/// Source: research note, "any name starting with `~$`".
pub const FORBIDDEN_PREFIX: &str = "~$";

/// Substring the server rejects anywhere in a name.
///
/// Source: research note, "`_vti_` cannot appear anywhere in a name". Matched
/// case-insensitively; the source does not state the case rule, and the server namespace is
/// case-insensitive. Applied to personal accounts too, since the source lists it for both.
pub const FORBIDDEN_SUBSTRING: &str = "_vti_";

/// Name the server rejects at a library root in work accounts.
///
/// Source: research note, "`forms` cannot be used for a file or folder at the library root".
/// Matched case-insensitively; the source does not state the case rule. The root of a
/// OneDrive for Business drive counts as a library root.
pub const LIBRARY_ROOT_RESERVED_NAME: &str = "forms";

/// The server path must be shorter than this many characters, counted as UTF-16 code units.
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
    /// The name is a reserved name.
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
            Self::PathTooLong { length, limit } => write!(
                f,
                "the server path is {length} UTF-16 code units long and must be fewer than {limit}"
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
/// variant it uses, are: empty name; [`FORBIDDEN_CHARACTERS`];
/// [`TENANT_RESTRICTED_CHARACTERS`] in a work account whose tenant refuses them; leading
/// space, trailing space; [`FORBIDDEN_PREFIX`]; [`FORBIDDEN_SUBSTRING`];
/// [`RESERVED_WHOLE_NAMES`]; [`RESERVED_DEVICE_NAMES`]; and [`LIBRARY_ROOT_RESERVED_NAME`] at
/// a work library root.
///
/// Leading and trailing spaces: research note, "no leading or trailing spaces". Only U+0020
/// is checked; the source names no other whitespace.
///
/// A trailing period is accepted: the source does not list it (see the module's doubtful
/// rules).
pub fn validate_name(name: &str, context: NameContext) -> Result<(), NameError> {
    let fail = |problem| Err(NameError::new(name, problem));

    if name.is_empty() {
        return fail(NameProblem::Empty);
    }
    if let Some(c) = name.chars().find(|c| FORBIDDEN_CHARACTERS.contains(c)) {
        return fail(NameProblem::ForbiddenCharacter(c));
    }
    if context.account == AccountKind::Work
        && !context.hash_and_percent_allowed
        && let Some(c) = name
            .chars()
            .find(|c| TENANT_RESTRICTED_CHARACTERS.contains(c))
    {
        return fail(NameProblem::ForbiddenCharacter(c));
    }
    if name.starts_with(' ') {
        return fail(NameProblem::LeadingSpace);
    }
    if name.ends_with(' ') {
        return fail(NameProblem::TrailingSpace);
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
    if let Some(reserved) = RESERVED_DEVICE_NAMES
        .iter()
        .find(|r| name.eq_ignore_ascii_case(r))
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
/// The caller passes the decoded server-relative path: plain text, not percent-encoded, and
/// for a library including the site and library segments (research note: "for a library
/// includes the site and library path segments"). The length is counted in UTF-16 code units,
/// so a character outside the Basic Multilingual Plane counts twice. A path of exactly 400
/// units is rejected ("fewer than 400").
///
/// The counting rule is unverified: the research note says the Microsoft article on how the
/// limit is counted (decoded or encoded, which segments, which unit) was not fetched.
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

    const fn context(account: AccountKind, at_library_root: bool, hash: bool) -> NameContext {
        NameContext {
            account,
            at_library_root,
            hash_and_percent_allowed: hash,
        }
    }

    const PERSONAL: NameContext = context(AccountKind::Personal, false, true);
    const PERSONAL_ROOT: NameContext = context(AccountKind::Personal, true, true);
    const WORK: NameContext = context(AccountKind::Work, false, true);
    const WORK_ROOT: NameContext = context(AccountKind::Work, true, true);
    const WORK_NO_HASH: NameContext = context(AccountKind::Work, false, false);
    const WORK_ROOT_NO_HASH: NameContext = context(AccountKind::Work, true, false);
    const ALL_CONTEXTS: [NameContext; 6] = [
        PERSONAL,
        PERSONAL_ROOT,
        WORK,
        WORK_ROOT,
        WORK_NO_HASH,
        WORK_ROOT_NO_HASH,
    ];

    fn problem(name: &str, context: NameContext) -> Option<NameProblem> {
        validate_name(name, context).err().map(|e| e.problem)
    }

    /// AC1, ADR-0015 "Names": reserved characters. Each character the source lists is rejected
    /// and named. The list is written out here, not taken from the module's constant, so that
    /// dropping a character from the constant fails this test.
    #[test]
    fn forbidden_characters_are_rejected_and_named() {
        // Research note: "Disallowed characters: `" * : < > ? / \ |`".
        let source = ['"', '*', ':', '<', '>', '?', '/', '\\', '|'];
        for c in source {
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

    /// AC1: `#` and `%` are accepted unless a work tenant is marked as refusing them, and then
    /// the error names the character.
    #[test]
    fn hash_and_percent_are_rejected_only_where_the_tenant_refuses_them() {
        // Research note: "some organisations do not yet support `#` and `%`".
        for c in ['#', '%'] {
            let name = format!("100{c} done");
            for accepting in [
                PERSONAL,
                PERSONAL_ROOT,
                WORK,
                WORK_ROOT,
                context(AccountKind::Personal, false, false),
            ] {
                assert_eq!(problem(&name, accepting), None, "{accepting:?}");
            }
            for refusing in [WORK_NO_HASH, WORK_ROOT_NO_HASH] {
                assert_eq!(
                    problem(&name, refusing),
                    Some(NameProblem::ForbiddenCharacter(c))
                );
            }
        }
    }

    /// AC2, ADR-0015 "Names": bare reserved device names are rejected in any case.
    #[test]
    fn reserved_device_names_are_rejected() {
        // Research note: "`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`".
        let mut source = vec!["CON".to_owned(), "PRN".into(), "AUX".into(), "NUL".into()];
        for n in 0..=9 {
            source.push(format!("COM{n}"));
            source.push(format!("LPT{n}"));
        }
        for reserved in source {
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
            for name in [reserved.clone(), lower, mixed] {
                for context in ALL_CONTEXTS {
                    assert_eq!(
                        problem(&name, context),
                        Some(NameProblem::ReservedName(reserved.clone())),
                        "{name:?} in {context:?}"
                    );
                }
            }
        }
    }

    /// AC2: names that contain or extend a device name are accepted; the source does not say
    /// a name with an extension is reserved.
    #[test]
    fn names_resembling_device_names_are_accepted() {
        for name in [
            "con.txt",
            "aux.c",
            "CON .txt",
            "nul.tar.gz",
            "LPT1.log",
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
            for root in [WORK_ROOT, WORK_ROOT_NO_HASH] {
                assert_eq!(
                    problem(name, root),
                    Some(NameProblem::ReservedAtLibraryRoot("forms"))
                );
            }
            for elsewhere in [WORK, WORK_NO_HASH, PERSONAL, PERSONAL_ROOT] {
                assert_eq!(problem(name, elsewhere), None, "{elsewhere:?}");
            }
        }
        assert_eq!(problem("forms.docx", WORK_ROOT), None);
        assert_eq!(problem("my forms", WORK_ROOT), None);
    }

    /// ADR-0015 "Names": leading and trailing spaces are rejected.
    #[test]
    fn leading_and_trailing_spaces_are_rejected() {
        let cases = [
            (" report.txt", NameProblem::LeadingSpace),
            ("  ", NameProblem::LeadingSpace),
            ("report.txt ", NameProblem::TrailingSpace),
        ];
        for (name, expected) in cases {
            for context in ALL_CONTEXTS {
                assert_eq!(problem(name, context), Some(expected.clone()), "{name:?}");
            }
        }
    }

    /// A trailing period is not in the source, so it is accepted and left to the server.
    #[test]
    fn trailing_period_is_accepted() {
        for name in ["report.", "report...", "v1.2."] {
            for context in ALL_CONTEXTS {
                assert_eq!(problem(name, context), None, "{name:?}");
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

    /// AC5, ADR-0015 "Names": the 400-character limit on the decoded path, counted in UTF-16
    /// code units, exclusive.
    #[test]
    fn server_path_length_limit() {
        let ok = "a".repeat(399);
        assert_eq!(validate_server_path(&ok), Ok(()));
        let at_limit = "a".repeat(400);
        let err = validate_server_path(&at_limit).unwrap_err();
        assert_eq!(
            err.problem,
            NameProblem::PathTooLong {
                length: 400,
                limit: 400
            }
        );
        assert!(err.to_string().contains("400 UTF-16 code units"), "{err}");
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
        // A BMP character such as `é` counts once, though it is two bytes in UTF-8 and six
        // characters percent-encoded.
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

    /// AC6: identical names passed twice are reported, as documented.
    #[test]
    fn identical_names_are_reported_as_collisions() {
        let errors = find_case_collisions(["same.txt", "other.txt", "same.txt"]);
        let expected = NameError::new(
            "same.txt",
            NameProblem::CaseCollision {
                with: vec!["same.txt".to_owned()],
            },
        );
        assert_eq!(errors, vec![expected.clone(), expected]);
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
        RESERVED_DEVICE_NAMES
            .iter()
            .any(|r| name.eq_ignore_ascii_case(r))
            || name.eq_ignore_ascii_case(LIBRARY_ROOT_RESERVED_NAME)
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
