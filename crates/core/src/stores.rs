//! Search-URL builders for DRM-free stores. digshelf only builds links for a
//! human to open; it never requests these pages, scrapes or checks out.
//!
//! Formats checked 2026-09 (see docs/DESIGN.md).

use reqwest::Url;
use serde::Serialize;

/// Default Qobuz storefront (`{country}-{language}`), e.g. `gb-en`, `fr-fr`.
pub const DEFAULT_QOBUZ_LOCALE: &str = "us-en";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreLinks {
    pub bandcamp: String,
    pub qobuz: String,
    pub beatport: String,
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    Url::parse_with_params(base, pairs)
        .expect("static base URL is valid")
        .to_string()
}

fn query(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Title as typed into a store search box: drop bracketed "feat." credits and
/// remaster notes, which only narrow results; keep remix/edit names.
pub fn search_title(title: &str) -> String {
    let mut out = String::new();
    let mut group = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                if depth == 1 {
                    group.clear();
                }
                group.push(c);
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                group.push(c);
                if depth == 0 {
                    let g = group.to_lowercase();
                    let drop = ["feat", "ft.", "with ", "remaster"]
                        .iter()
                        .any(|k| g.contains(k));
                    if !drop {
                        out.push_str(&group);
                    }
                }
            }
            _ if depth > 0 => group.push(c),
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Links that search for a single track.
pub fn track_links(artist: &str, title: &str, qobuz_locale: &str) -> StoreLinks {
    let q = query(&[artist, &search_title(title)]);
    let mut qobuz = Url::parse(&format!(
        "https://www.qobuz.com/{qobuz_locale}/search/tracks/"
    ))
    .unwrap_or_else(|_| Url::parse("https://www.qobuz.com/us-en/search/tracks/").unwrap());
    // Qobuz puts the query in the path; an encoded '/' there is a 404.
    qobuz
        .path_segments_mut()
        .expect("https URL has path segments")
        .pop_if_empty()
        .push(&q.replace('/', " "));
    StoreLinks {
        bandcamp: with_query(
            "https://bandcamp.com/search",
            &[("q", &q), ("item_type", "t")],
        ),
        qobuz: qobuz.to_string(),
        beatport: with_query("https://www.beatport.com/search/tracks", &[("q", &q)]),
    }
}

/// Links that search for a release/album.
pub fn album_links(artist: &str, album: &str, qobuz_locale: &str) -> StoreLinks {
    let q = query(&[artist, &search_title(album)]);
    let qobuz_base = format!("https://www.qobuz.com/{qobuz_locale}/search/");
    let qobuz = Url::parse_with_params(&qobuz_base, &[("q", &q)])
        .map(|u| u.to_string())
        .unwrap_or_else(|_| with_query("https://www.qobuz.com/us-en/search/", &[("q", &q)]));
    StoreLinks {
        bandcamp: with_query(
            "https://bandcamp.com/search",
            &[("q", &q), ("item_type", "a")],
        ),
        qobuz,
        beatport: with_query("https://www.beatport.com/search/releases", &[("q", &q)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_links_are_encoded() {
        let l = track_links("Élan Vital", "Café Nuit (feat. Ana Luz)", "gb-en");
        assert_eq!(
            l.bandcamp,
            "https://bandcamp.com/search?q=%C3%89lan+Vital+Caf%C3%A9+Nuit&item_type=t"
        );
        assert_eq!(
            l.qobuz,
            "https://www.qobuz.com/gb-en/search/tracks/%C3%89lan%20Vital%20Caf%C3%A9%20Nuit"
        );
        assert_eq!(
            l.beatport,
            "https://www.beatport.com/search/tracks?q=%C3%89lan+Vital+Caf%C3%A9+Nuit"
        );
    }

    #[test]
    fn qobuz_track_path_has_no_slash() {
        let l = track_links("AC/DC", "Back?In#Black", "us-en");
        assert_eq!(
            l.qobuz,
            "https://www.qobuz.com/us-en/search/tracks/AC%20DC%20Back%3FIn%23Black"
        );
    }

    #[test]
    fn album_links() {
        let l = super::album_links("Vela Nine", "Coastal Lights (Remastered)", "us-en");
        assert_eq!(
            l.bandcamp,
            "https://bandcamp.com/search?q=Vela+Nine+Coastal+Lights&item_type=a"
        );
        assert_eq!(
            l.qobuz,
            "https://www.qobuz.com/us-en/search/?q=Vela+Nine+Coastal+Lights"
        );
        assert_eq!(
            l.beatport,
            "https://www.beatport.com/search/releases?q=Vela+Nine+Coastal+Lights"
        );
    }

    #[test]
    fn search_title_keeps_remix_names() {
        assert_eq!(
            search_title("Low Tide (Kora Blue Remix) [feat. X]"),
            "Low Tide (Kora Blue Remix)"
        );
    }
}
