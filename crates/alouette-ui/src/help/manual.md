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

Alouette opens these URLs today: about: pages (welcome, tasks, history, settings, keys, help), gopher, gophers, gemini and finger (Burrow), inquire (Inquire), net (NetTools), term, ssh, telnet and mosh (Terminal), and files, file, sftp, ftp, ftps, ftpes, dav, davs and s3 (Files). Each later milestone adds more: mailto and imap in M3, news in M4, and so on.

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

## News

news: opens your first news server, saved under [[news.servers]] with its host (port 563 for TLS) and, if it needs one, your login. nntps://host/ opens a server that is not saved; it is saved once you pick a group there. Press L to list every group on the server, type to narrow the list, and Enter to add one. D removes a group.

Enter opens a group and then an article. Space pages through an article and then goes on to the next unread one, as newsreaders always have; n jumps to the next unread. u marks an article read or unread and c catches up on the whole group. p posts a new article, f follows up in the group, and r replies to the author by mail. Set your name and address under [news], or Alouette uses your first mail account's.

## Feeds

feeds: shows the feeds you follow on the left and their items on the right. Press a and type a feed's address, or just a site's: Alouette finds the feed the page points to. Enter reads an item, Space goes through the unread ones, o opens the item's link, m marks a feed read, D stops following it and g refreshes. Feeds refresh by themselves every feeds.refresh_mins minutes. feed:URL shows one feed without following it; press s to follow it.

## Social

social: opens your first account under [[social.accounts]]. social:@you@mastodon.social adds an account: paste an access token from your server's Preferences > Development page, or leave it empty to approve Alouette in your browser and paste back the code it shows. The token is kept in your keychain. Mastodon and servers that speak its API, such as GoToSocial, Pleroma and Akkoma, all work.

1 to 4 (or Tab) switch between Home, Notifications, Local and Federated. Enter opens a post's thread beside the list, or unfolds a content warning. c writes a post, r replies, f favourites, b boosts, o opens the post in your browser and g refreshes. Timelines refresh by themselves every social.poll_secs seconds, and scrolling to the end loads older posts. While writing, Ctrl+V changes who can see the post, the top line takes a content warning, and Ctrl+S posts.

## Configuration

Settings live in alouette.toml. Run "alouette config path" to find it. Alouette reloads the file as soon as you save it. The Settings window changes common options for you and saves the file.

## Themes

Built-in themes: minuet, turbo, amber, green, modern-dark, modern-light, high-contrast and mono. Put your own theme files in the themes folder next to alouette.toml. A theme can inherit a built-in and override any color slot.

## Keymaps

Three presets ship: cua (F-keys and Alt menus, like Minuet), vim (Ctrl+W window commands and : for the palette) and emacs (C-x prefixes and M-x for the palette). Override any binding in the [keys] table.

## Nostalgia

The splash screen, drop shadows, desktop pattern, Minuet's module names, sounds and retro commands are each a switch in [nostalgia]. Set enabled = false to turn them all off.

## Privacy

Alouette sends no telemetry. The offline cache and AI features are off until you turn them on. Passwords and tokens are kept in your operating system's keychain.
