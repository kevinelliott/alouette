//! The nostalgia layer.
//!
//! Everything here is optional and switched by `[nostalgia]` in the config. Alouette is
//! inspired by Minuet, the Minnesota Internet Users Essential Tool from the University of
//! Minnesota (1993-1996). It does not use any of Minuet's code, art or text.

/// The splash logo. Plain ASCII so it renders on any terminal.
pub const LOGO: &[&str] = &[
    r"    _    _                  _   _       ",
    r"   / \  | | ___  _   _  ___| |_| |_ ___ ",
    r"  / _ \ | |/ _ \| | | |/ _ \ __| __/ _ \",
    r" / ___ \| | (_) | |_| |  __/ |_| ||  __/",
    r"/_/   \_\_|\___/ \__,_|\___|\__|\__\___|",
];

/// A one-line tagline under the logo.
pub const TAGLINE: &str = "Your whole internet desk, in one terminal.";

/// The fake boot sequence shown on the splash, one line at a time.
pub const BOOT_LINES: &[&str] = &[
    "Memory test: 640K OK",
    "Loading packet driver ............ OK",
    "Negotiating IPv6 ................. OK",
    "Starting TCP/IP stack (QUIC) ..... OK",
    "Opening the internet desk ........ OK",
];

/// Minuet's names for Alouette's modules, used when `minuet_names = true`.
pub fn minuet_name(module_id: &str) -> Option<&'static str> {
    Some(match module_id {
        "mail" => "POPmail",
        "news" => "NetNews",
        "burrow" => "Gopher",
        "terminal" => "Telnet",
        "files" => "FTP",
        "web" => "WWW",
        "contacts" => "Address Book",
        "nettools" => "Ping & Finger",
        "inquire" => "Archie",
        _ => return None,
    })
}

/// The text behind the `minuet` easter egg and about:minuet.
pub const MINUET_HISTORY: &str = "\
Minuet, the Minnesota Internet Users Essential Tool, was an integrated internet \
package for DOS PCs, made at the University of Minnesota between 1993 and 1996.

It put email, Usenet news, Gopher, FTP, Telnet and later a web browser behind one \
mouse-driven, windowed interface that ran in as little as 512 KB of memory.

Three ideas made it feel like one product:

  * Mail and news shared one reader. In the University's words, this \"erases some \
of the artificial boundaries between e-mail and netnews.\"
  * Everything ran at once. You could fetch news and mail while browsing.
  * Clicking a URL opened the right built-in tool, and a log kept recent URLs.

Alouette keeps those ideas and rebuilds everything else for today's internet. \
It is an independent project, inspired by Minuet and grateful to the people at \
the University of Minnesota who made it. It contains none of Minuet's code, art \
or text.";

/// Short taglines the status line can rotate through when nostalgia is on.
pub const TIPS: &[&str] = &[
    "Tip: Ctrl+K opens the command palette. Type a URL to open it.",
    "Tip: F6 cycles between windows. Ctrl+F6 switches layouts.",
    "Tip: every network job runs in the background. See Window > Tasks.",
    "Tip: change themes with the palette: type \"theme\".",
    "Tip: alouette.toml reloads as soon as you save it.",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_lines_are_ascii_and_same_width() {
        let w = LOGO[0].len();
        for line in LOGO {
            assert!(line.is_ascii());
            assert!(line.len() <= w + 1);
        }
    }

    #[test]
    fn minuet_names_cover_the_classics() {
        assert_eq!(minuet_name("mail"), Some("POPmail"));
        assert_eq!(minuet_name("news"), Some("NetNews"));
        assert_eq!(minuet_name("demo"), None);
    }
}
