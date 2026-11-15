# Alouette manual

## The idea

Alouette puts your whole internet desk in one terminal: mail, news, Gopher and Gemini, file transfer, terminals, the web, search and more. It follows Minuet's three big ideas: one reader for everything you read, everything runs in the background, and every URL opens in the right built-in tool.

## Layouts

Desktop is the Minuet look: a menu bar, overlapping windows you can drag, zoom and resize, and drop shadows. Tiled fills the screen with a main pane and a stack. Tabs shows one window at a time with a tab bar, which suits small terminals and screen readers. Switch at any time from the Window menu, the palette, or the layout key. Nothing is lost when you switch.

## The command palette

Press Ctrl+K (or the palette key for your keymap) and start typing. The palette finds commands, open windows, modules, themes, keymaps and recent URLs. If what you type looks like a URL, the first entry opens it.

## Windows

Every window has a number. Alt plus that number focuses it. In the Desktop layout, drag a title bar to move a window, drag its bottom-right corner to resize it, click [■] to close it and [↑] to zoom it. From the keyboard, the move key enters move mode: arrow keys move, Shift plus arrow keys resize, and Enter or Esc finishes.

## Background tasks

Network work runs as tasks. The Tasks window lists them with progress. Select one and press c to cancel, r to retry, or x to clear finished tasks.

## URLs

Alouette opens these URLs today: about: pages (welcome, tasks, history, settings, keys, help), gopher, gophers, gemini and finger (Burrow), inquire (Inquire), net (NetTools), term, ssh, telnet and mosh (Terminal), files, file, sftp, ftp, ftps, ftpes, dav, davs and s3 (Files), mail and mailto (Mail), news, nntp and nntps (News), feeds and feed (Feeds), social (Social), http, https and web (Web), contacts (Contacts), calendar, webcal and webcals (Calendar), and chat, irc, ircs and matrix (Chat).

## Terminal

term: opens a shell on this computer. ssh: lists the hosts in your ~/.ssh/config and the ones saved under [[ssh.hosts]]; ssh://user@host connects. Alouette uses your SSH agent, your key files (asking for passphrases), known_hosts, ProxyJump and LocalForward, and asks before trusting a new host key. A changed host key is refused.

While a terminal has the keyboard, every key goes to the program inside it. Press Ctrl+] and then a key to give that key to Alouette instead: Ctrl+] then k opens the palette, Ctrl+] then F6 goes to the next window. Shift+PgUp and Shift+PgDn scroll back. When a session ends, Enter closes the window and r reconnects.

telnet://host connects with Telnet, VT100 and all. mosh://user@host runs the mosh program if it is installed.

## Files

Files shows two panes. The left starts in your home folder; the right shows the URL you opened, such as sftp://me@host/, ftp://ftp.gnu.org/gnu/, davs://cloud.example.com/remote.php/dav/files/me/ or s3://bucket/. Save sites under [[files.sites]] and open them with files:name.

Tab switches panes. Enter opens a folder or shows a file, Backspace goes up, Space marks. c (or F5) copies the marked items, or the selected one, to the other pane, in the background; watch them in Tasks. n makes a folder, r renames, d deletes (after asking), o opens a path or URL in this pane, h shows hidden files, g reloads. A README in a folder is shown as you arrive, as Minuet did.

FTP passwords travel in the clear: use ftpes:// (TLS on port 21) or ftps:// (TLS on port 990) when the server offers it. S3 keys come from AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY or ~/.aws/credentials; set an endpoint and region on a saved site for MinIO, R2 and other stores.

## Mail

mail: opens your first account; mail:name opens another. With no accounts, mail:setup asks for one: pick IMAP, Gmail, Microsoft 365 or JMAP, type your address and the servers, and Alouette asks for your password when it first connects, then keeps it in your keychain. Accounts live under [[mail.accounts]] in alouette.toml.

Tab moves between folders, messages and the open message. Enter opens a message and Space pages through it. c writes a new message, r replies, a replies to everyone, f forwards. s stars, u marks read or unread, e archives, m moves to a folder you name, d moves to Trash (and in Trash deletes for good after asking), / filters by subject or sender, g reloads. New mail shows up by itself where the server supports IMAP IDLE, and otherwise every mail.poll_secs seconds.

In the writing window, Tab moves between the fields and the text, and Ctrl+B shows Cc and Bcc. Ctrl+S sends, Ctrl+D saves a draft, and Esc closes (asking first if you have written something). mailto: links open the writing window too.

Gmail and Microsoft 365 sign in with OAuth, which needs an app of your own: create an OAuth client in Google Cloud (type Desktop app) or an app registration in Microsoft Entra (public client, with IMAP.AccessAsUser.All, SMTP.Send and offline_access), and put its client_id (and client_secret, for Google) on the account. Google opens your browser; Microsoft shows a code to type at microsoft.com/devicelogin.

Mail is not saved on this computer unless you set mail.offline_cache = true, which keeps folder lists, message lists and the messages you open, so you can read them without a connection. Pictures in HTML mail are never fetched.

## Inquire

inquire:anything works out what you typed and asks every source that applies at once. A domain gets DNS, RDAP, TLS, certificate logs, WHOIS and the Wayback Machine; an IP address or AS number gets reverse DNS, routing and RDAP; user@host gets WebFinger, Finger, OpenPGP keys and mail servers; a web address gets TLS, DNS and its archived copies.

Words go to Wikipedia, arXiv and Crossref for papers, the Library of Congress catalogue over SRU, DICT for definitions, Veronica-2 for Gopherspace and Kennedy for Geminispace. A DOI goes straight to Crossref and arXiv:2401.01234 straight to arXiv. Add a SearXNG instance or any OpenSearch engine under [inquire] to search the web as well. arXiv and OpenSearch results end with a link that follows the search as a feed, so new results show up in Feeds.

## News

news: opens your first news server, saved under [[news.servers]] with its host (port 563 for TLS) and, if it needs one, your login. nntps://host/ opens a server that is not saved; it is saved once you pick a group there. Press L to list every group on the server, type to narrow the list, and Enter to add one. D removes a group.

Enter opens a group and then an article. Space pages through an article and then goes on to the next unread one, as newsreaders always have; n jumps to the next unread. u marks an article read or unread and c catches up on the whole group. p posts a new article, f follows up in the group, and r replies to the author by mail. Set your name and address under [news], or Alouette uses your first mail account's.

## Feeds

feeds: shows the feeds you follow on the left and their items on the right. Press a and type a feed's address, or just a site's: Alouette finds the feed the page points to. Enter reads an item, Space goes through the unread ones, o opens the item's link, m marks a feed read, D stops following it and g refreshes. Feeds refresh by themselves every feeds.refresh_mins minutes. feed:URL shows one feed without following it; press s to follow it.

## Social

social: opens your first account under [[social.accounts]]. social:@you@mastodon.social adds an account: paste an access token from your server's Preferences > Development page, or leave it empty to approve Alouette in your browser and paste back the code it shows. The token is kept in your keychain. Mastodon and servers that speak its API, such as GoToSocial, Pleroma and Akkoma, all work.

1 to 4 (or Tab) switch between Home, Notifications, Local and Federated. Enter opens a post's thread beside the list, or unfolds a content warning. c writes a post, r replies, f favourites, b boosts, o opens the post in your browser and g refreshes. Timelines refresh by themselves every social.poll_secs seconds, and scrolling to the end loads older posts. While writing, Ctrl+V changes who can see the post, the top line takes a content warning, and Ctrl+S posts.

## Web

web: opens the start page, with a search box, your bookmarks and recent pages. Any http:// or https:// address opens in Web, from the command line, the palette or a link in mail, news or feeds. Pages load over HTTP/2 when the server offers it, otherwise HTTP/1.1, and the status line shows which along with the connection.

Press o to go to an address. Words that are not an address are searched with web.search (DuckDuckGo's HTML page unless you change it). f shows a letter by every link and field on screen: type the letters to follow it, or a capital to open it in a new window. Tab and Shift+Tab move between links and fields and Enter follows or edits; a page's only text box is sent when you press Enter in it. Checkboxes and radio buttons toggle with Enter, and lists open as a menu.

b goes back and F forward (Alt+Left and Alt+Right work too), r reloads, / finds text and n finds the next match. R switches reader mode, which keeps only the article; Alouette remembers your choice for each site, and web.reader turns it on everywhere. a bookmarks the page, B lists bookmarks and H shows history. s saves the page as Markdown and d downloads the selected link, both into your downloads folder; files that are not pages download by themselves. O opens the page, or the selected link, in your own browser, which is the way to read pages that need JavaScript; Alouette says when a page looks like one of those. i shows how you are connected.

Cookies last until you quit. Mail written in HTML is shown through the same renderer, with each link's address after its text.

## Contacts

contacts: opens the address book: people on the left, the chosen card on the right. Cards are kept as vCard files in Alouette's data folder, in the local book or in a CardDAV book (Nextcloud, iCloud, Fastmail, Radicale and others) saved under [[contacts.books]] with its server and user name; Alouette asks for the password once and keeps it in your keychain, and syncs every contacts.sync_mins minutes and when you press g.

a adds a card and e edits one; Tab moves between the fields and Ctrl+S saves. A card can hold mail addresses, phone numbers, fediverse handles (@you@server), SSH hosts (user@host, or the name of a saved host), chat addresses and web pages, and each becomes an action on the card: press its number, or Tab to the card and Enter. / finds people, d deletes, i imports a .vcf file and m writes to the selected person. L searches the LDAP directories under [[contacts.directories]], such as a company's; s saves a person found there. In Mail, a sender who is in your contacts is shown as such, and + adds the sender of the open message.

## Calendar

calendar: shows your agenda; v switches to the week and then the month, and 1, 2 and 3 pick one. Left and Right move a day, PgUp and PgDn a week (a month in the month view), t comes back to today and o goes to a day you type, such as 2026-03-14, fri or +7. The chosen event is shown below the view.

a adds an event: a title, a day, a start and end time (leave the start empty for an all-day event), how it repeats (daily, weekdays, weekly, fortnightly, monthly, yearly, or an RRULE), where, a reminder in minutes, notes and the calendar it goes in. e edits the whole series, and d deletes, asking whether to delete one occurrence or every one. Reminders show as notifications while Alouette runs, whichever window you are in.

Calendars live in Alouette's data folder: the local one, CalDAV calendars saved under [[calendar.accounts]], and calendars you follow by address, which are read-only. Press s and paste an address (webcal:// or https://) to follow one, or open a webcal:// link. i imports a .ics file. CalDAV calendars and followed ones refresh every calendar.sync_mins minutes and when you press g.

When a message in Mail holds an invitation, the reader shows what, when, where and who, and Y accepts, N declines and T says maybe. Accepted events go in your first CalDAV calendar (or the local one), and a reply to the organiser opens ready to send with Ctrl+S. Answers to your own invitations update the guest list by themselves, and a cancellation can be taken out of your calendar with Y.

## Chat

chat: opens one window for every IRC network and Matrix account: buffers on the left (each network's own buffer, then its channels, private chats and rooms), the conversation in the middle, the people in it on the right, and a line to type in at the bottom. Alt+Up and Alt+Down (or Ctrl+P and Ctrl+N) change buffer, Tab completes a nick, Up and Down bring back what you typed, and PgUp and PgDn scroll. A number beside a buffer counts new lines; it is highlighted when you are mentioned. Connections belong to Alouette, not the window, so closing the window keeps you connected.

Add an IRC network with /server irc.libera.chat (no port means TLS on 6697; host:+7000 is TLS on another port, host:6667 is plain text), or open a link such as ircs://irc.libera.chat/#rust. Networks are saved under [[chat.irc]], where channels lists the channels to join and autoconnect = true connects when Alouette starts. sasl = "plain" signs in to services with the account (or your nick) and a password Alouette asks for once and keeps in your keychain; sasl = "external" uses the client certificate in cert, a PEM file, for CertFP. Alouette speaks IRCv3: server times, echoed messages, multiple prefixes, away and account notices, batches, and recent history on join where the server keeps it (chathistory, as soju and Ergo do). Type /help in Chat for the commands: /join, /part, /msg, /query, /me, /topic, /nick, /whois, /mode, /kick, /away and the rest; other /commands go to the server as typed, and // starts a line with a slash.

Add a Matrix account with /matrix @you:matrix.org (and the homeserver's address after it when it differs from the server name). Alouette asks for your password once, signs in as a new session called Alouette, keeps that session in your keychain rather than the password, and keeps its encryption keys in an encrypted store in the data folder. Encrypted rooms are marked in the buffer list. To read encrypted history, verify this session: type /verify and accept in another of your sessions, then compare the emoji and type /confirm if they match (/mismatch if not). When another session asks to verify this one, /accept starts the same comparison. /recover followed by your recovery key unlocks your key backup instead. /devices lists your sessions and whether each is verified, /join #room:server joins a room, /part leaves it, and /logout ends the session on the server. matrix: links, such as matrix:r/alouette:matrix.org, join rooms too.

Mentions of your nick (or of the words in chat.highlights) and private messages raise a notification; chat.notify = "all" notifies for every line and "off" for none. chat.logs = true keeps a plain-text log of every buffer, one file a day, under the data folder's chat/logs; it is off unless you turn it on.

## Configuration

Settings live in alouette.toml. Run "alouette config path" to find it. Alouette reloads the file as soon as you save it. The Settings window changes common options for you and saves the file.

## Network

Every module connects through the same layer. It races IPv6 and IPv4 (Happy Eyeballs), offers the post-quantum X25519MLKEM768 key exchange first in TLS 1.3, and can resolve names over DNS over HTTPS, TLS or QUIC (network.dns). Proxies and per-host routes send traffic through SOCKS5 or HTTP proxies; route *.onion through Tor's SOCKS port to reach onion sites. Web pages use HTTP/3 over QUIC when a site offers it and the connection is direct (network.http3), and Encrypted Client Hello hides the site's name when its DNS HTTPS record publishes a key (network.ech), which is most useful with encrypted DNS. The status line badge shows what each window's connection uses, such as "IPv6 · TLS 1.3 · PQ · ECH · DoH HTTP/3".

## Themes

Built-in themes: minuet, turbo, amber, green, modern-dark, modern-light, high-contrast and mono. Put your own theme files in the themes folder next to alouette.toml. A theme can inherit a built-in and override any color slot.

## Keymaps

Three presets ship: cua (F-keys and Alt menus, like Minuet), vim (Ctrl+W window commands and : for the palette) and emacs (C-x prefixes and M-x for the palette). Override any binding in the [keys] table.

## Nostalgia

The splash screen, drop shadows, desktop pattern, Minuet's module names, sounds and retro commands are each a switch in [nostalgia]. Set enabled = false to turn them all off.

## Privacy

Alouette sends no telemetry. The offline cache and AI features are off until you turn them on. Passwords and tokens are kept in your operating system's keychain.
