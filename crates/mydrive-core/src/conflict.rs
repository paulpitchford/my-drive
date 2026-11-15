//! Conflict-copy naming (ADR-0012 "Conflicts", flow F07 step 5, journey J10).
//!
//! When a file changed both here and on the server, the server version keeps the original
//! name and the local version is kept as a copy named `<name>-<hostname>.<ext>`, or
//! `<name>-<hostname>` without an extension (ADR-0012 R7). This module only chooses that
//! name. Deciding that a conflict exists, and carrying out the split, belong to the engine.
//!
//! The rule, in the order it is applied ([`conflict_copy_name`]):
//!
//! 1. The name is split at its **last** dot: `archive.tar.gz` has stem `archive.tar` and
//!    extension `.gz`. A dot at the very start does not start an extension, so `.profile` has
//!    no extension. A trailing dot is an empty extension: `notes.` gives `notes-laptop.`.
//! 2. The host name becomes a name segment ([`host_segment`]): each character
//!    [`validate_name`] forbids in the context is replaced with [`HOST_REPLACEMENT`]; a host
//!    with no usable character becomes [`HOST_FALLBACK`] (overview, "Rulings on details").
//! 3. The suffix is `-<host>`, then `-<host>-2`, `-<host>-3` and so on while the caller says
//!    the candidate is taken, up to [`MAX_COPY_NUMBER`].
//! 4. If a candidate is longer than [`MAX_NAME_BYTES`], the stem is shortened at a character
//!    boundary; the extension and the suffix are never cut.

use std::fmt;

use crate::names::{FORBIDDEN_SUBSTRING, NameContext, NameError, NameProblem, validate_name};

/// The longest name, in bytes of UTF-8, that a conflict copy may have.
///
/// This is a **local** limit, not a OneDrive rule: Linux file systems limit one path component
/// to 255 bytes (`NAME_MAX`), and the copy must be creatable under the mount. The server's own
/// limit is on the whole path (see [`crate::names::SERVER_PATH_LIMIT`]), which the engine
/// checks separately.
pub const MAX_NAME_BYTES: usize = 255;

/// The character that replaces each character of the host name that the server forbids.
///
/// Source: overview, "Rulings on details": "Conflict copy names replace characters the server
/// forbids with `_`".
pub const HOST_REPLACEMENT: char = '_';

/// The host segment used when the host name has no usable character.
///
/// Source: overview, "Rulings on details": "a host name with no usable characters becomes
/// `device`".
pub const HOST_FALLBACK: &str = "device";

/// The longest host segment, in bytes of UTF-8. A longer host name is cut to this length at a
/// character boundary before it becomes part of the suffix.
///
/// Linux limits a host name to 64 bytes (`HOST_NAME_MAX`), so a real host name is never cut.
/// The bound exists so that an arbitrary host string still leaves room for the stem within
/// [`MAX_NAME_BYTES`].
pub const MAX_HOST_BYTES: usize = 64;

/// The highest copy number tried. After `-<host>` and `-<host>-2` up to
/// `-<host>-MAX_COPY_NUMBER` are all reported taken, [`conflict_copy_name`] gives up with
/// [`ConflictNameError::AllTaken`] instead of looping on.
///
/// Ten thousand copies of one file in one folder is far beyond any real use; reaching the
/// bound means the caller's predicate is wrong, and guessing a name would risk a collision.
pub const MAX_COPY_NUMBER: u32 = 10_000;

/// Why no conflict-copy name could be chosen. In every case the caller keeps the local
/// version where it is; nothing is renamed or discarded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConflictNameError {
    /// The original name is itself not valid on the server in this context, so no copy name
    /// derived from it can be relied on.
    #[error("cannot name a conflict copy of {name:?}: the original name is invalid")]
    InvalidName {
        /// The original name.
        name: String,
        /// Why the original name is invalid.
        #[source]
        source: NameError,
    },
    /// The extension is so long that no suffix and no stem fit within [`MAX_NAME_BYTES`]
    /// without cutting the extension.
    #[error(
        "cannot name a conflict copy of {name:?}: its extension leaves no room for the stem \
         and the suffix within {MAX_NAME_BYTES} bytes"
    )]
    NoRoom {
        /// The original name.
        name: String,
    },
    /// Every candidate up to [`MAX_COPY_NUMBER`] was reported taken.
    #[error(
        "cannot name a conflict copy of {name:?}: all {MAX_COPY_NUMBER} candidates are taken, \
         the last tried was {last_tried:?}"
    )]
    AllTaken {
        /// The original name.
        name: String,
        /// The last candidate tried.
        last_tried: String,
    },
    /// A candidate failed [`validate_name`]. The rule above is built so that this does not
    /// happen; it is reported instead of returning a name the server would refuse.
    #[error("cannot name a conflict copy of {name:?}: candidate {candidate:?} is invalid")]
    CandidateInvalid {
        /// The original name.
        name: String,
        /// The candidate that failed.
        candidate: String,
        /// Why it failed.
        #[source]
        source: NameError,
    },
}

/// Chooses the name of the conflict copy of `name` made on host `host` (ADR-0012 R7).
///
/// `context` is where the copy will live: the same folder as the original. `is_taken` is
/// asked about each candidate in turn and must return true if that name is already used in
/// the folder, locally or on the server. The server namespace is case-insensitive, so the
/// predicate should compare as [`crate::names::find_case_collisions`] does. A candidate equal
/// to `name` itself is never returned, whatever the predicate says.
///
/// `host` should be the short host name (without the domain); it is used as given apart from
/// the replacements described in [`host_segment`].
///
/// On success the result passes [`validate_name`] in `context`, is at most
/// [`MAX_NAME_BYTES`] bytes, ends with the original extension, and is a name `is_taken`
/// reported free. The stem is shortened, at a character boundary, only as far as needed.
///
/// # Errors
///
/// [`ConflictNameError::InvalidName`] if `name` fails [`validate_name`];
/// [`ConflictNameError::NoRoom`] if the extension is too long to leave room;
/// [`ConflictNameError::AllTaken`] after [`MAX_COPY_NUMBER`] taken candidates. The predicate
/// is called at most [`MAX_COPY_NUMBER`] times, so the function always terminates.
pub fn conflict_copy_name<F>(
    name: &str,
    host: &str,
    context: NameContext,
    mut is_taken: F,
) -> Result<String, ConflictNameError>
where
    F: FnMut(&str) -> bool,
{
    validate_name(name, context).map_err(|source| ConflictNameError::InvalidName {
        name: name.to_owned(),
        source,
    })?;
    let host = host_segment(host, context);
    let (stem, extension) = split_extension(name);
    let mut last_tried = String::new();
    for number in 1..=MAX_COPY_NUMBER {
        let suffix = Suffix {
            host: &host,
            number,
        }
        .to_string();
        let candidate =
            assemble(stem, &suffix, extension).ok_or_else(|| ConflictNameError::NoRoom {
                name: name.to_owned(),
            })?;
        if let Err(source) = validate_name(&candidate, context) {
            return Err(ConflictNameError::CandidateInvalid {
                name: name.to_owned(),
                candidate,
                source,
            });
        }
        if candidate != name && !is_taken(&candidate) {
            return Ok(candidate);
        }
        last_tried = candidate;
    }
    Err(ConflictNameError::AllTaken {
        name: name.to_owned(),
        last_tried,
    })
}

/// Turns a host name into the segment used in a conflict-copy suffix.
///
/// Control characters in the original file name are not this function's concern: the name is
/// used as given.
///
/// - Each character that [`validate_name`] rejects as a forbidden character in `context` is
///   replaced with [`HOST_REPLACEMENT`]. That includes `#` and `%` where the tenant refuses
///   them.
/// - Each control character (U+0000 to U+001F and U+007F) is replaced with
///   [`HOST_REPLACEMENT`] too, so a copy name never gains a control character from the host.
///   `validate_name` accepts them, so this goes beyond the server's rules (lead ruling).
/// - The result is cut to [`MAX_HOST_BYTES`] at a character boundary.
/// - Leading and trailing spaces are removed, because the segment can end the name (a name
///   without an extension) and the server refuses a trailing space.
/// - If no character is left that was neither forbidden nor a space, or if the segment
///   contains the forbidden substring `_vti_` (which replacement can create, as in `?vti?`),
///   the segment is [`HOST_FALLBACK`].
pub fn host_segment(host: &str, context: NameContext) -> String {
    let mut segment: Vec<(char, bool)> = Vec::new();
    let mut bytes = 0;
    for c in host.chars() {
        let forbidden = c.is_ascii_control() || is_forbidden_character(c, context);
        let out = if forbidden { HOST_REPLACEMENT } else { c };
        bytes += out.len_utf8();
        if bytes > MAX_HOST_BYTES {
            break;
        }
        segment.push((out, !forbidden && c != ' '));
    }
    let start = segment.iter().position(|&(c, _)| c != ' ');
    let end = segment.iter().rposition(|&(c, _)| c != ' ');
    let trimmed = match (start, end) {
        (Some(start), Some(end)) => &segment[start..=end],
        _ => &[],
    };
    if !trimmed.iter().any(|&(_, usable)| usable) {
        return HOST_FALLBACK.to_owned();
    }
    let segment: String = trimmed.iter().map(|&(c, _)| c).collect();
    if segment.to_lowercase().contains(FORBIDDEN_SUBSTRING) {
        return HOST_FALLBACK.to_owned();
    }
    segment
}

/// Whether `validate_name` rejects `c` as a forbidden character in `context`. Asking
/// `validate_name` itself keeps this in step with the name rules.
fn is_forbidden_character(c: char, context: NameContext) -> bool {
    let mut buffer = [0; 4];
    matches!(
        validate_name(c.encode_utf8(&mut buffer), context),
        Err(NameError {
            problem: NameProblem::ForbiddenCharacter(_),
            ..
        })
    )
}

/// Splits `name` at its last dot into stem and extension (the extension keeps its dot). A
/// dot at index 0 does not start an extension.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// Joins stem, suffix and extension, shortening the stem at a character boundary to fit
/// [`MAX_NAME_BYTES`]. Returns `None` if not even one character of the stem fits.
///
/// Shortening cannot make the result invalid: the kept stem starts as the original name does
/// (so no new leading space or `~$` prefix), it is followed by `-` (so a trailing space or dot
/// in it is not at the end), and `_vti_` cannot span the `-` into the suffix.
fn assemble(stem: &str, suffix: &str, extension: &str) -> Option<String> {
    let room = MAX_NAME_BYTES.checked_sub(suffix.len() + extension.len())?;
    let kept = &stem[..stem.floor_char_boundary(room)];
    if kept.is_empty() {
        return None;
    }
    Some(format!("{kept}{suffix}{extension}"))
}

/// `-<host>` for the first candidate, `-<host>-<number>` after it.
struct Suffix<'a> {
    host: &'a str,
    number: u32,
}

impl fmt::Display for Suffix<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.number <= 1 {
            write!(f, "-{}", self.host)
        } else {
            write!(f, "-{}-{}", self.host, self.number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{AccountKind, FORBIDDEN_CHARACTERS};
    use proptest::prelude::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    const fn context(account: AccountKind, at_library_root: bool, hash: bool) -> NameContext {
        NameContext {
            account,
            at_library_root,
            hash_and_percent_allowed: hash,
        }
    }

    const PERSONAL: NameContext = context(AccountKind::Personal, false, true);
    const WORK_NO_HASH: NameContext = context(AccountKind::Work, false, false);
    const ALL_CONTEXTS: [NameContext; 6] = [
        PERSONAL,
        context(AccountKind::Personal, true, true),
        context(AccountKind::Work, false, true),
        context(AccountKind::Work, true, true),
        WORK_NO_HASH,
        context(AccountKind::Work, true, false),
    ];

    fn free(name: &str, host: &str) -> String {
        conflict_copy_name(name, host, PERSONAL, |_| false).unwrap()
    }

    /// ADR-0012 R7: the local version is kept as `<name>-<hostname>.<ext>`, or
    /// `<name>-<hostname>` with no extension (J10-AC5's names).
    #[test]
    fn adr_0012_r7_copy_is_name_hostname_ext() {
        assert_eq!(free("budget.xlsx", "sam-laptop"), "budget-sam-laptop.xlsx");
        assert_eq!(free("README", "sam-laptop"), "README-sam-laptop");
    }

    /// AC1: the split is at the last dot; a leading dot does not start an extension.
    #[test]
    fn split_at_the_last_dot() {
        let cases = [
            ("report.docx", "report-laptop.docx"),
            ("Makefile", "Makefile-laptop"),
            ("archive.tar.gz", "archive.tar-laptop.gz"),
            (".profile", ".profile-laptop"),
            (".config.bak", ".config-laptop.bak"),
            ("notes.", "notes-laptop."),
            ("a.b.c.d", "a.b.c-laptop.d"),
        ];
        for (name, expected) in cases {
            assert_eq!(free(name, "laptop"), expected, "{name:?}");
        }
    }

    /// AC2: a taken candidate gives `-2`, then `-3`, asking about each in order.
    #[test]
    fn taken_names_get_a_number() {
        let taken: HashSet<&str> = ["report-laptop.docx", "report-laptop-2.docx"]
            .into_iter()
            .collect();
        let asked = RefCell::new(Vec::new());
        let result = conflict_copy_name("report.docx", "laptop", PERSONAL, |c| {
            asked.borrow_mut().push(c.to_owned());
            taken.contains(c)
        });
        assert_eq!(result.unwrap(), "report-laptop-3.docx");
        assert_eq!(
            asked.into_inner(),
            vec![
                "report-laptop.docx",
                "report-laptop-2.docx",
                "report-laptop-3.docx"
            ]
        );
        let result = conflict_copy_name("Makefile", "laptop", PERSONAL, |c| c == "Makefile-laptop");
        assert_eq!(result.unwrap(), "Makefile-laptop-2");
    }

    /// AC2: a predicate that reports everything taken ends in a typed error after
    /// `MAX_COPY_NUMBER` questions, never a loop or a guess.
    #[test]
    fn everything_taken_is_a_bounded_error() {
        let mut asked = 0_u32;
        let result = conflict_copy_name("report.docx", "laptop", PERSONAL, |_| {
            asked += 1;
            true
        });
        assert_eq!(asked, MAX_COPY_NUMBER);
        assert_eq!(
            result,
            Err(ConflictNameError::AllTaken {
                name: "report.docx".to_owned(),
                last_tried: format!("report-laptop-{MAX_COPY_NUMBER}.docx"),
            })
        );
    }

    /// AC2: the original name is never returned, even if the predicate says it is free. It
    /// can come up when shortening the stem removes exactly the part the suffix adds back.
    #[test]
    fn original_name_is_never_returned() {
        let ext = ".txt";
        let host = "laptop";
        // A stem ending in `-laptop` whose copy needs exactly `-laptop` cut from the stem.
        let stem = format!("{}-{host}", "a".repeat(MAX_NAME_BYTES - ext.len() - 7));
        let name = format!("{stem}{ext}");
        assert_eq!(name.len(), MAX_NAME_BYTES);
        let result = conflict_copy_name(&name, host, PERSONAL, |_| false).unwrap();
        assert_ne!(result, name);
        assert!(result.ends_with("-laptop-2.txt"), "{result}");
    }

    /// AC3: characters the server forbids in a host name become `_`.
    #[test]
    fn forbidden_host_characters_become_underscores() {
        assert_eq!(free("report.docx", "lap:top"), "report-lap_top.docx");
        assert_eq!(free("report.docx", "a<b>c|d"), "report-a_b_c_d.docx");
        assert_eq!(free("report.docx", "laptop?"), "report-laptop_.docx");
        // `#` and `%` only where the tenant refuses them.
        assert_eq!(free("report.docx", "lab#1"), "report-lab#1.docx");
        let refused = conflict_copy_name("report.docx", "lab#1%", WORK_NO_HASH, |_| false);
        assert_eq!(refused.unwrap(), "report-lab_1_.docx");
    }

    /// AC3: a host with no usable character becomes `device`.
    #[test]
    fn unusable_host_becomes_device() {
        let only_forbidden: String = FORBIDDEN_CHARACTERS.iter().collect();
        for host in [only_forbidden.as_str(), "", "   ", "?", " : "] {
            assert_eq!(free("report.docx", host), "report-device.docx", "{host:?}");
            assert_eq!(free("README", host), "README-device", "{host:?}");
        }
        assert_eq!(
            conflict_copy_name("a.txt", "#%", WORK_NO_HASH, |_| false).unwrap(),
            "a-device.txt"
        );
    }

    /// A host that replacement would turn into `_vti_`, or that contains it, becomes
    /// `device`; spaces at its ends are removed so the copy never ends with a space.
    #[test]
    fn host_cannot_make_the_name_invalid() {
        assert_eq!(free("a.txt", "?vti?"), "a-device.txt");
        assert_eq!(free("a.txt", "x_VTI_y"), "a-device.txt");
        assert_eq!(free("README", "laptop "), "README-laptop");
        assert_eq!(free("README", "  my laptop  "), "README-my laptop");
    }

    /// AC4: a long stem is shortened at a character boundary, keeping the extension and the
    /// suffix whole, to at most 255 bytes.
    #[test]
    fn long_stem_is_shortened_at_a_character_boundary() {
        let name = format!("{}.docx", "日".repeat(100));
        let result = free(&name, "laptop");
        assert!(result.len() <= MAX_NAME_BYTES, "{}", result.len());
        assert!(result.ends_with("-laptop.docx"));
        let kept = result.trim_end_matches("-laptop.docx");
        assert_eq!(kept, "日".repeat(kept.chars().count()));
        // Three bytes per character: 255 - 12 = 243 bytes leaves 81 characters.
        assert_eq!(kept.chars().count(), 81);
    }

    /// A stem cut so that it ends in a space or a dot still gives a valid name.
    #[test]
    fn shortened_stem_ending_in_space_or_dot_is_valid() {
        for filler in [' ', '.'] {
            // The cut falls right after the filler character.
            let stem = format!("a{}{filler}bbbbbbbb", "x".repeat(MAX_NAME_BYTES - 8));
            let name = format!("{stem}.md");
            let result = free(&name, "pc");
            assert!(result.len() <= MAX_NAME_BYTES);
            assert_eq!(validate_name(&result, PERSONAL), Ok(()), "{result:?}");
            assert!(result.ends_with(&format!("{filler}-pc.md")), "{result:?}");
        }
    }

    /// An extension too long to leave room is a typed error, not a cut extension.
    #[test]
    fn extension_too_long_is_an_error() {
        let name = format!("a.{}", "e".repeat(MAX_NAME_BYTES));
        assert_eq!(
            conflict_copy_name(&name, "laptop", PERSONAL, |_| false),
            Err(ConflictNameError::NoRoom { name: name.clone() })
        );
    }

    /// An invalid original name is a typed error.
    #[test]
    fn invalid_original_name_is_an_error() {
        let result = conflict_copy_name("bad:name.txt", "laptop", PERSONAL, |_| false);
        assert!(
            matches!(result, Err(ConflictNameError::InvalidName { .. })),
            "{result:?}"
        );
    }

    /// A long host is cut to `MAX_HOST_BYTES` at a character boundary.
    #[test]
    fn long_host_is_bounded() {
        let host = "é".repeat(100);
        let segment = host_segment(&host, PERSONAL);
        assert_eq!(segment, "é".repeat(MAX_HOST_BYTES / 2));
        // A two-byte character straddling the cut is dropped whole, not split.
        let straddling = format!("{}é", "a".repeat(MAX_HOST_BYTES - 1));
        assert_eq!(
            host_segment(&straddling, PERSONAL),
            "a".repeat(MAX_HOST_BYTES - 1)
        );
    }

    /// A host whose first `MAX_HOST_BYTES` are all replaced or all spaces still falls back to
    /// `device`, even though usable characters follow the cut.
    #[test]
    fn host_unusable_after_the_cut_becomes_device() {
        for filler in ["?", " ", "\0", "\u{7f}", "? "] {
            let host = format!("{}laptop", filler.repeat(MAX_HOST_BYTES));
            assert_eq!(host_segment(&host, PERSONAL), HOST_FALLBACK, "{host:?}");
            assert_eq!(free("README", &host), "README-device");
        }
        // Spaces before the cut are trimmed after it, leaving the usable part.
        let host = format!("x{}", " ".repeat(MAX_HOST_BYTES + 5));
        assert_eq!(host_segment(&host, PERSONAL), "x");
    }

    /// Control characters in the host become `_`; a host of only control characters becomes
    /// `device`. Control characters in the original name are kept as they are.
    #[test]
    fn control_characters_in_the_host_become_underscores() {
        assert_eq!(free("a.txt", "lap\ttop\0\n\u{7f}x"), "a-lap_top___x.txt");
        assert_eq!(free("a.txt", "\u{1}\u{1f}"), "a-device.txt");
        assert_eq!(free("README", "\0\r\n"), "README-device");
        // U+0080 and above are not in the ruled range and are kept.
        assert_eq!(free("a.txt", "pc\u{85}"), "a-pc\u{85}.txt");
        assert_eq!(free("tab\tname.txt", "pc"), "tab\tname-pc.txt");
        for c in (0_u8..0x20).chain([0x7f]) {
            let host = format!("pc{}", char::from(c));
            assert_eq!(free("a.txt", &host), "a-pc_.txt", "{c:#x}");
        }
    }

    /// AC6: conflict naming does no I/O and never reads the system time or host name. The
    /// forbidden names are split so that this test does not match itself.
    #[test]
    fn source_reads_no_system_state_and_does_no_io() {
        let source = include_str!("conflict.rs");
        for forbidden in [
            concat!("System", "Time"),
            concat!("Instant", "::now"),
            concat!("std::", "fs"),
            concat!("std::", "io"),
            concat!("std::", "net"),
            concat!("std::", "env"),
            concat!("std::", "process"),
            concat!("get", "hostname"),
        ] {
            assert!(!source.contains(forbidden), "conflict.rs uses {forbidden}");
        }
    }

    /// Characters chosen to stress the rules: multi-byte (2, 3 and 4 bytes), combining
    /// marks, spaces, dots and the characters the rules treat specially.
    const NAME_CHARS: &[char] = &[
        'a', 'Z', '0', ' ', '.', '-', '_', '~', '$', '#', '%', 'v', 't', 'i', 'é', 'ß', 'Σ', '日',
        '本', '😀', '🏖', '\u{301}', '\u{130}',
    ];

    fn name_chars(max: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(NAME_CHARS), 1..max)
            .prop_map(|chars| chars.into_iter().collect())
    }

    /// Names that pass `validate_name` in the chosen context, often longer than 255 bytes.
    fn valid_name() -> impl Strategy<Value = (String, NameContext)> {
        (
            name_chars(300),
            prop::option::of(name_chars(12)),
            prop::sample::select(ALL_CONTEXTS.to_vec()),
        )
            .prop_map(|(stem, ext, context)| {
                // Without an extension the stem must hold no dot, or it would have one.
                let name = match ext {
                    Some(ext) => format!("{stem}.{ext}"),
                    None => format!("a{}", stem.replace('.', "")),
                };
                (name, context)
            })
            .prop_filter("passes validate_name", |(name, context)| {
                validate_name(name, *context).is_ok()
            })
    }

    /// Any host string, including forbidden characters, spaces and `_vti_` fragments.
    fn any_host() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            prop::collection::vec(
                prop::sample::select(vec![
                    'a', ' ', '?', ':', '/', '#', '%', '_', 'v', 't', 'i', 'V', '日', '😀', '.',
                    '\0', '\t', '\n', '\u{1f}', '\u{7f}',
                ]),
                0..90,
            )
            .prop_map(|chars| chars.into_iter().collect()),
            // Ending in spaces, which matters when the host ends the name.
            "[a-zé日?:#%_ ]{0,70} {1,3}",
        ]
    }

    proptest! {
        /// AC4: for any host and any valid name, the result is valid in the same context,
        /// at most 255 bytes, and differs from the original only by a stem shortened at a
        /// character boundary and the inserted suffix. With an extension of up to 100 bytes
        /// it always succeeds.
        #[test]
        fn result_is_valid_and_bounded(
            (name, context) in valid_name(),
            host in any_host(),
        ) {
            let result = conflict_copy_name(&name, &host, context, |_| false);
            let (stem, extension) = split_extension(&name);
            let segment = host_segment(&host, context);
            if extension.len() <= 100 {
                prop_assert!(result.is_ok(), "{:?}", result);
            }
            let Ok(result) = result else {
                prop_assert!(
                    matches!(result, Err(ConflictNameError::NoRoom { .. })),
                    "{:?}",
                    result
                );
                return Ok(());
            };
            prop_assert_eq!(validate_name(&result, context), Ok(()));
            prop_assert!(result.len() <= MAX_NAME_BYTES, "{} bytes", result.len());
            prop_assert!(result.ends_with(extension));
            let suffix = format!("-{segment}");
            let kept = &result[..result.len() - extension.len()];
            let kept = kept.strip_suffix(suffix.as_str());
            prop_assert!(kept.is_some(), "suffix {:?} missing from {:?}", suffix, result);
            let kept = kept.unwrap_or_default();
            prop_assert!(!kept.is_empty());
            // The suffix brings no control character into the name.
            prop_assert!(
                !suffix.chars().any(|c| c.is_ascii_control()),
                "control character in suffix {:?}",
                suffix
            );
            prop_assert_eq!(
                result.chars().filter(char::is_ascii_control).count(),
                name.chars().filter(char::is_ascii_control).count()
                    - stem[kept.len()..].chars().filter(char::is_ascii_control).count()
            );
            prop_assert!(stem.starts_with(kept), "{:?} is not a prefix of {:?}", kept, stem);
            if stem.len() + suffix.len() + extension.len() <= MAX_NAME_BYTES {
                prop_assert_eq!(kept, stem);
            }
        }

        /// AC2: the result is never a name the predicate reports as taken. When the first n
        /// candidates are taken, the result is candidate n + 1, numbered n + 1.
        #[test]
        fn result_is_never_taken(
            (name, context) in valid_name(),
            host in any_host(),
            taken_first in 0_usize..40,
            extra_taken in prop::collection::hash_set(name_chars(20), 0..10),
        ) {
            let asked = RefCell::new(Vec::new());
            let result = conflict_copy_name(&name, &host, context, |c| {
                let mut asked = asked.borrow_mut();
                asked.push(c.to_owned());
                asked.len() <= taken_first || extra_taken.contains(c)
            });
            let asked = asked.into_inner();
            let Ok(result) = result else {
                // Only an extension too long for any copy, never "all taken" at this size.
                prop_assert!(
                    matches!(result, Err(ConflictNameError::NoRoom { .. })),
                    "{:?}",
                    result
                );
                return Ok(());
            };
            prop_assert_eq!(validate_name(&result, context), Ok(()));
            prop_assert!(result.len() <= MAX_NAME_BYTES, "{} bytes", result.len());
            prop_assert!(!extra_taken.contains(&result));
            prop_assert_ne!(&result, &name);
            let position = asked.iter().position(|c| *c == result);
            prop_assert_eq!(position, Some(asked.len() - 1));
            prop_assert!(asked.len() > taken_first);
            // Every candidate asked about is distinct, so a taken one is never re-offered.
            let distinct: HashSet<&String> = asked.iter().collect();
            prop_assert_eq!(distinct.len(), asked.len());
            let segment = host_segment(&host, context);
            // A candidate equal to the original is skipped without asking, which shifts the
            // numbering; that needs the original to contain the suffix already.
            if asked.len() > 1 && !name.contains(&format!("-{segment}")) {
                let number = asked.len();
                let (_, extension) = split_extension(&name);
                let ending = format!("-{segment}-{number}{extension}");
                prop_assert!(result.ends_with(&ending), "{:?} lacks {:?}", result, ending);
            }
        }
    }
}
