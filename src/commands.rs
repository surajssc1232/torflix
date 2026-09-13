//! Slash commands typed into the home search box.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Browse,
    Favorites,
    History,
    Downloads,
    Tv,
    Addons,
    Settings,
    Help,
    Quit,
}

impl Command {
    pub const ALL: [Command; 9] = [
        Command::Browse,
        Command::Favorites,
        Command::History,
        Command::Downloads,
        Command::Tv,
        Command::Addons,
        Command::Settings,
        Command::Help,
        Command::Quit,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Command::Browse => "/browse",
            Command::Favorites => "/favorites",
            Command::History => "/history",
            Command::Downloads => "/downloads",
            Command::Tv => "/tv",
            Command::Addons => "/addons",
            Command::Settings => "/settings",
            Command::Help => "/help",
            Command::Quit => "/quit",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Command::Browse => "popular and top-rated movies & series",
            Command::Favorites => "titles you've starred",
            Command::History => "everything you've watched or downloaded",
            Command::Downloads => "torrents in progress",
            Command::Tv => "live TV from M3U playlists",
            Command::Addons => "manage Stremio addons (streams, subtitles)",
            Command::Settings => "player, subtitles, download folder",
            Command::Help => "keyboard shortcuts",
            Command::Quit => "exit torflix",
        }
    }

    pub fn parse(input: &str) -> Option<Command> {
        let word = input.trim().split_whitespace().next()?.to_ascii_lowercase();
        Some(match word.as_str() {
            "/browse" | "/discover" => Command::Browse,
            "/favorites" | "/favourites" | "/fav" => Command::Favorites,
            "/history" => Command::History,
            "/downloads" | "/dl" => Command::Downloads,
            "/tv" | "/live" => Command::Tv,
            "/addons" | "/addon" => Command::Addons,
            "/settings" | "/config" | "/prefs" => Command::Settings,
            "/help" | "/?" => Command::Help,
            "/quit" | "/exit" | "/q" => Command::Quit,
            _ => return None,
        })
    }

    /// Commands matching what's typed so far, for the suggestion line under the search box.
    pub fn suggest(input: &str) -> Vec<Command> {
        let input = input.trim_start();
        if !input.starts_with('/') || input.contains(char::is_whitespace) {
            return Vec::new();
        }
        let typed = input.to_ascii_lowercase();
        Command::ALL
            .into_iter()
            .filter(|c| c.name().starts_with(&typed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for c in Command::ALL {
            assert_eq!(Command::parse(c.name()), Some(c));
        }
    }

    #[test]
    fn aliases_case_and_arguments() {
        assert_eq!(Command::parse("/EXIT"), Some(Command::Quit));
        assert_eq!(Command::parse("  /config  "), Some(Command::Settings));
        assert_eq!(Command::parse("/favourites now"), Some(Command::Favorites));
        assert_eq!(Command::parse("/nope"), None);
        assert_eq!(Command::parse("breaking bad"), None);
        assert_eq!(Command::parse(""), None);
    }

    #[test]
    fn suggestions() {
        assert_eq!(Command::suggest("/").len(), Command::ALL.len());
        assert_eq!(Command::suggest("/h"), vec![Command::History, Command::Help]);
        assert_eq!(Command::suggest("/TV"), vec![Command::Tv]);
        assert!(Command::suggest("/history extra").is_empty());
        assert!(Command::suggest("dune").is_empty());
    }
}
