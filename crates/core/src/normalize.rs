//! Text normalisation used by the matcher. Pure functions, no I/O.

/// Words that mark a bracketed or dashed title suffix as a distinct version of
/// a recording (a remix is not the same track as the original).
const VERSION_KEYWORDS: &[&str] = &[
    "remix",
    "mix",
    "edit",
    "dub",
    "version",
    "vip",
    "rework",
    "bootleg",
    "live",
    "instrumental",
    "acoustic",
    "radio",
    "extended",
    "club",
    "remake",
    "reprise",
    "demo",
    "flip",
];

/// Suffixes that carry no information about which recording it is, so they
/// are dropped entirely (e.g. "Remastered 2015", "Original Mix").
const IGNORED_SUFFIXES: &[&str] = &[
    "remaster",
    "original mix",
    "album version",
    "original version",
    "explicit",
    "clean",
    "mono",
    "stereo",
];

const FEAT_PREFIXES: &[&str] = &["feat.", "feat ", "ft.", "ft ", "featuring ", "with "];

/// Lowercase, fold accents to ASCII, spell out `&`/`+`, drop punctuation and
/// collapse whitespace. `"Chloé & DJ-KAY!"` → `"chloe and dj kay"`.
pub fn normalize(s: &str) -> String {
    let folded = deunicode::deunicode(s).to_lowercase();
    let mut out = String::with_capacity(folded.len());
    for c in folded.chars() {
        match c {
            '&' | '+' => out.push_str(" and "),
            '\'' | '’' | '`' => {} // "don't" -> "dont"
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A title split into the parts the matcher compares separately.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleParts {
    /// Normalised title without version or featuring info.
    pub base: String,
    /// Normalised version descriptor ("extended mix", "burial remix"), or empty.
    pub version: String,
    /// Normalised featured artist names.
    pub featured: Vec<String>,
}

enum Suffix {
    Featured(String),
    Version(String),
    Ignored,
    Other,
}

fn classify_suffix(raw: &str) -> Suffix {
    let lower = raw.trim().to_lowercase();
    for p in FEAT_PREFIXES {
        if let Some(rest) = lower.strip_prefix(p) {
            return Suffix::Featured(rest.to_string());
        }
    }
    let norm = normalize(&lower);
    if norm.is_empty() {
        return Suffix::Ignored;
    }
    if IGNORED_SUFFIXES.iter().any(|ign| norm.contains(ign)) {
        return Suffix::Ignored;
    }
    if norm
        .split(' ')
        .any(|w| VERSION_KEYWORDS.contains(&w) || w.ends_with("mix"))
    {
        return Suffix::Version(norm);
    }
    Suffix::Other
}

/// Split a raw title into base title, version and featured artists.
///
/// Handles bracketed suffixes (`Title (Extended Mix)`, `Title [feat. X]`),
/// dashed suffixes (`Title - Radio Edit`) and bare `feat.` (`Title feat. X`).
pub fn split_title(raw: &str) -> TitleParts {
    let mut base = String::new();
    let mut versions: Vec<String> = Vec::new();
    let mut featured: Vec<String> = Vec::new();

    // 1. Pull out top-level bracket groups.
    let mut depth = 0usize;
    let mut group = String::new();
    for c in raw.chars() {
        match c {
            '(' | '[' | '{' => {
                if depth > 0 {
                    group.push(c);
                }
                depth += 1;
            }
            ')' | ']' | '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    match classify_suffix(&group) {
                        Suffix::Featured(f) => featured.extend(split_artists(&f)),
                        Suffix::Version(v) => versions.push(v),
                        Suffix::Ignored => {}
                        Suffix::Other => {
                            base.push(' ');
                            base.push_str(&group);
                        }
                    }
                    group.clear();
                } else {
                    group.push(c);
                }
            }
            _ if depth > 0 => group.push(c),
            _ => base.push(c),
        }
    }
    if depth > 0 {
        // Unbalanced bracket: keep the text rather than lose it.
        base.push(' ');
        base.push_str(&group);
    }

    // 2. Dashed suffixes, right to left: "Title - Extended Mix - 2011 Remaster".
    while let Some(idx) = base.rfind(" - ") {
        let suffix = base[idx + 3..].to_string();
        match classify_suffix(&suffix) {
            Suffix::Featured(f) => featured.extend(split_artists(&f)),
            Suffix::Version(v) => versions.push(v),
            Suffix::Ignored => {}
            Suffix::Other => break,
        }
        base.truncate(idx);
    }

    // 3. Bare "feat." inside the remaining title.
    let lower = base.to_lowercase();
    for marker in [" feat. ", " feat ", " ft. ", " featuring "] {
        if let Some(idx) = lower.find(marker) {
            featured.extend(split_artists(&base[idx + marker.len()..]));
            base.truncate(idx);
            break;
        }
    }

    versions.reverse();
    TitleParts {
        base: normalize(&base),
        version: versions.join(" "),
        featured,
    }
}

/// Split an artist credit into normalised individual names.
/// `"Artist A, Artist B & Artist C feat. D"` → `["artist a", "artist b", "artist c", "d"]`.
pub fn split_artists(raw: &str) -> Vec<String> {
    const SEPARATORS: &[&str] = &[
        ",",
        ";",
        "/",
        " & ",
        " + ",
        " and ",
        " x ",
        " vs. ",
        " vs ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " with ",
    ];
    let mut s = format!(" {} ", raw.to_lowercase());
    for sep in SEPARATORS {
        s = s.replace(sep, "\u{1}");
    }
    s.split('\u{1}')
        .map(|name| {
            let n = normalize(name);
            n.strip_prefix("the ").map(str::to_string).unwrap_or(n)
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// Normalise an ISRC: uppercase, strip separators. Returns `None` for values
/// that are not 12 alphanumeric characters.
pub fn normalize_isrc(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == 12).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_accents_and_punctuation() {
        assert_eq!(normalize("Chloé & DJ-KAY!"), "chloe and dj kay");
        assert_eq!(normalize("  Don't   Stop  "), "dont stop");
        assert_eq!(normalize("Nuit Rosé"), "nuit rose");
    }

    #[test]
    fn splits_bracketed_version_and_feat() {
        let p = split_title("Night Drive (feat. Ana Luz) [Extended Mix]");
        assert_eq!(p.base, "night drive");
        assert_eq!(p.version, "extended mix");
        assert_eq!(p.featured, vec!["ana luz"]);
    }

    #[test]
    fn drops_remaster_and_original_mix() {
        assert_eq!(split_title("Hey Ocean (Remastered 2015)").version, "");
        assert_eq!(split_title("Hey Ocean (Remastered 2015)").base, "hey ocean");
        assert_eq!(split_title("Low Tide - Original Mix").version, "");
        assert_eq!(split_title("Low Tide - 2011 Remaster").base, "low tide");
    }

    #[test]
    fn splits_dashed_version() {
        let p = split_title("Low Tide - Kora Blue Remix");
        assert_eq!(p.base, "low tide");
        assert_eq!(p.version, "kora blue remix");
        // A dash that is part of the title is kept.
        assert_eq!(split_title("Left - Right").base, "left right");
    }

    #[test]
    fn keeps_non_version_brackets() {
        assert_eq!(split_title("Suite (Part 2)").base, "suite part 2");
    }

    #[test]
    fn splits_bare_feat() {
        let p = split_title("Glass Rooms feat. Mira Sol");
        assert_eq!(p.base, "glass rooms");
        assert_eq!(p.featured, vec!["mira sol"]);
    }

    #[test]
    fn splits_artist_credits() {
        assert_eq!(
            split_artists("The Paper Kites, Nova & Echo feat. Rain"),
            vec!["paper kites", "nova", "echo", "rain"]
        );
    }

    #[test]
    fn isrc_normalisation() {
        assert_eq!(
            normalize_isrc("gb-abc-12-00001").as_deref(),
            Some("GBABC1200001")
        );
        assert_eq!(normalize_isrc("short"), None);
    }
}
