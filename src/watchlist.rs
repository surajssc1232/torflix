//! Your Letterboxd watchlist, as a list to browse.
//!
//! Letterboxd's write API is invite-only, but a public watchlist is ordinary
//! HTML: `letterboxd.com/<user>/watchlist/page/<n>/` lists 28 films a page,
//! each carrying its own name, year and slug in the markup. That is enough to
//! find the film in Cinemeta afterwards, so no second request per film is
//! needed to turn the list into something torflix can play.
//!
//! The pages are asked for gzipped (about 23 KB each) and the whole list is
//! kept on disk for a few hours, so opening the list again costs nothing.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// How long a saved watchlist is used before it's fetched again.
const TTL: u64 = 6 * 60 * 60;
/// Letterboxd paginates at 28; this caps a very long list at ~2800 films.
const MAX_PAGES: usize = 100;

/// One film on the watchlist, before it's been matched to anything.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// Release year as Letterboxd gives it, or empty.
    pub year: String,
    /// Letterboxd's own slug, e.g. `black-bag-2025`.
    pub slug: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Cached {
    #[serde(default)]
    films: Vec<Entry>,
    #[serde(default)]
    at: u64,
}

fn cache_path(user: &str) -> std::path::PathBuf {
    crate::config::cache_dir()
        .join("letterboxd")
        .join(format!("watchlist-{}.json", crate::config::safe_file_name(user)))
}

/// The watchlist for `user`, from disk when it was fetched recently.
/// `force` fetches it again regardless.
pub fn fetch(user: &str, force: bool) -> Result<Vec<Entry>> {
    let user = user.trim().trim_matches('/');
    if user.is_empty() {
        return Err(anyhow!("no Letterboxd username set — try /letterboxd <username>"));
    }
    let path = cache_path(user);
    if !force {
        let cached: Cached = crate::config::load_json(&path);
        if cached.at > 0 && crate::history::now().saturating_sub(cached.at) < TTL {
            return Ok(cached.films);
        }
    }

    let first = page(user, 1)?;
    let pages = page_count(&first).min(MAX_PAGES);
    let mut films = parse(&first);
    for n in 2..=pages {
        match page(user, n) {
            Ok(html) => films.extend(parse(&html)),
            // A page that fails leaves the rest of the list usable.
            Err(_) => break,
        }
    }
    if films.is_empty() {
        return Err(anyhow!("no films on {user}'s watchlist — is it public?"));
    }
    crate::config::write_json(&path, &Cached { films: films.clone(), at: crate::history::now() });
    Ok(films)
}

fn page(user: &str, n: usize) -> Result<String> {
    let url = format!("https://letterboxd.com/{user}/watchlist/page/{n}/");
    let resp = minreq::get(&url)
        .with_header("Accept-Encoding", "gzip")
        .with_header("User-Agent", crate::config::USER_AGENT)
        .with_timeout(30)
        .send()?;
    match resp.status_code {
        404 => Err(anyhow!("no watchlist for '{user}' — check the username, and that it's public")),
        s if s >= 400 => Err(anyhow!("letterboxd returned HTTP {s}")),
        _ => Ok(crate::letterboxd::gunzip(resp.as_bytes())),
    }
}

/// How many pages the list has, read off the paging links.
fn page_count(html: &str) -> usize {
    html.match_indices("/watchlist/page/")
        .filter_map(|(i, _)| {
            let rest = &html[i + "/watchlist/page/".len()..];
            let end = rest.find(|c: char| !c.is_ascii_digit())?;
            rest[..end].parse::<usize>().ok()
        })
        .max()
        .unwrap_or(1)
}

/// Pull the films out of one page.
///
/// Each is a `LazyPoster` component carrying `data-item-name="Name (Year)"`
/// and `data-item-slug`. The poster image itself is loaded later by their
/// javascript and isn't in the page, which is why the name is what we take.
fn parse(html: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for (i, _) in html.match_indices("data-item-slug=\"") {
        let rest = &html[i..];
        let Some(slug) = attr(rest, "data-item-slug=\"") else { continue };
        // The name sits just before the slug on the same element.
        let start = html[..i].rfind("data-item-name=\"").unwrap_or(i);
        let Some(full) = attr(&html[start..], "data-item-name=\"") else { continue };
        let (name, year) = split_year(&unescape(&full));
        if name.is_empty() {
            continue;
        }
        out.push(Entry { name, year, slug });
    }
    out.dedup_by(|a, b| a.slug == b.slug);
    out
}

fn attr(s: &str, key: &str) -> Option<String> {
    let rest = s.strip_prefix(key).or_else(|| s.split_once(key).map(|p| p.1))?;
    rest.split_once('"').map(|(v, _)| v.to_string())
}

/// `"Black Bag (2025)"` → `("Black Bag", "2025")`.
fn split_year(full: &str) -> (String, String) {
    let trimmed = full.trim();
    if let Some(open) = trimmed.rfind(" (") {
        let year = &trimmed[open + 2..];
        if let Some(year) = year.strip_suffix(')') {
            if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                return (trimmed[..open].to_string(), year.to_string());
            }
        }
    }
    (trimmed.to_string(), String::new())
}

/// The few entities Letterboxd puts in an attribute.
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#039;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
<div class="paginate-pages"><li><a href="/dave/watchlist/page/2/">2</a></li>
<li><a href="/dave/watchlist/page/23/">23</a></li></div>
<li class="griditem">
  <div class="react-component" data-component-class="LazyPoster" data-item-name="Black Bag (2025)"
       data-item-slug="black-bag-2025" data-item-link="/film/black-bag-2025/"></div>
</li>
<li class="griditem">
  <div class="react-component" data-item-name="Peter &amp; Ben&#039;s Film" data-item-slug="peter-and-bens-film"></div>
</li>"#;

    #[test]
    fn reads_the_films_off_a_page() {
        let films = parse(PAGE);
        assert_eq!(films.len(), 2);
        assert_eq!(films[0], Entry {
            name: "Black Bag".into(),
            year: "2025".into(),
            slug: "black-bag-2025".into(),
        });
        // A film with no year keeps its whole name, entities and all.
        assert_eq!(films[1].name, "Peter & Ben's Film");
        assert_eq!(films[1].year, "");
    }

    #[test]
    fn counts_the_pages_from_the_paging_links() {
        assert_eq!(page_count(PAGE), 23);
        assert_eq!(page_count("<html>one page only</html>"), 1);
    }

    #[test]
    fn years_come_off_the_end_only_when_they_look_like_years() {
        assert_eq!(split_year("Black Bag (2025)"), ("Black Bag".into(), "2025".into()));
        assert_eq!(split_year("Se7en"), ("Se7en".into(), String::new()));
        // Not a year, so it stays part of the title.
        assert_eq!(split_year("Film (Director's Cut)"), ("Film (Director's Cut)".into(), String::new()));
        assert_eq!(split_year("8½ (1963)"), ("8½".into(), "1963".into()));
    }

    #[test]
    fn an_empty_page_is_no_films_rather_than_an_error() {
        assert!(parse("<html><body>nothing here</body></html>").is_empty());
    }

    #[test]
    #[ignore = "hits letterboxd.com"]
    fn live_watchlist() {
        let dir = std::env::temp_dir().join(format!("torflix-wl-{}", std::process::id()));
        std::env::set_var("XDG_CACHE_HOME", &dir);
        let films = fetch("dave", true).expect("watchlist");
        assert!(films.len() > 100, "{} films", films.len());
        assert!(films.iter().all(|f| !f.slug.is_empty()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
