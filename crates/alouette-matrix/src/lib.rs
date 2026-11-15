//! Matrix for Alouette: sign in, rooms, messages with end-to-end encryption, key backup
//! recovery and emoji device verification, on top of the Matrix Rust SDK.
//!
//! The window talks to [`run`] through two channels: it sends [`Cmd`]s and receives
//! [`Update`]s. Everything Matrix-specific stays in this crate.

#![recursion_limit = "256"]

#[cfg(feature = "test-server")]
pub mod testserver;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, Verification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::key::verification::start::ToDeviceKeyVerificationStartEvent;
use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
use matrix_sdk::ruma::events::room::member::{MembershipState, OriginalSyncRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent};
use matrix_sdk::ruma::events::room::power_levels::UserPowerLevel;
use matrix_sdk::ruma::events::room::topic::OriginalSyncRoomTopicEvent;
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::{OwnedRoomId, RoomId, RoomOrAliasId, UserId};
use matrix_sdk::{Client, Room, RoomMemberships, authentication::matrix::MatrixSession};
use tokio::sync::{Mutex, mpsc};

/// Where to sign in.
#[derive(Debug, Clone)]
pub struct Account {
    /// What to call it.
    pub name: String,
    /// A server name (`matrix.org`) or homeserver URL.
    pub homeserver: String,
    /// `@me:matrix.org` or just `me`.
    pub user: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Emote,
    Notice,
    /// An event we can't decrypt (yet).
    Undecryptable,
    Join,
    Leave,
    Topic,
    Info,
}

#[derive(Debug, Clone)]
pub struct Msg {
    /// Unix milliseconds.
    pub time: i64,
    pub kind: Kind,
    /// Display name of the sender.
    pub from: String,
    pub text: String,
    pub own: bool,
    pub history: bool,
    pub highlight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub topic: String,
    pub encrypted: bool,
    pub direct: bool,
    pub unread: u64,
    pub highlights: u64,
}

/// News from the connection.
#[derive(Debug, Clone)]
pub enum Update {
    /// A line for the account's own buffer.
    Status(String),
    Error(String),
    /// Signed in. `session` is JSON to keep in the secret store and pass back next time.
    Ready {
        user_id: String,
        device_id: String,
        session: Option<String>,
    },
    Rooms(Vec<RoomInfo>),
    Message {
        room: String,
        msg: Msg,
    },
    Members {
        room: String,
        users: Vec<(String, String)>,
    },
    /// The emoji to compare, or another step of a verification.
    Verify(String),
}

pub enum Cmd {
    Send {
        room: String,
        text: String,
        emote: bool,
    },
    Join(String),
    Leave(String),
    /// Load older messages for a room.
    History(String),
    Members(String),
    /// Verify one of our other sessions, or another person.
    Verify(Option<String>),
    Accept,
    Confirm,
    Mismatch,
    Cancel,
    Recover(String),
    Devices,
    Logout,
    Quit,
}

/// Why `run` stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    Quit,
    /// No saved session and no password: ask for one and call again.
    NeedPassword,
    LoggedOut,
    Failed(String),
}

/// A random passphrase for the local encrypted store.
pub fn new_passphrase() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("system randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `me` on `matrix.org` becomes `@me:matrix.org`.
pub fn full_user_id(user: &str, homeserver: &str) -> String {
    if user.starts_with('@') && user.contains(':') {
        return user.to_string();
    }
    let host = homeserver.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
    let host = host.split('/').next().unwrap_or(host);
    format!("@{}:{host}", user.trim_start_matches('@'))
}

fn ms(ts: matrix_sdk::ruma::MilliSecondsSinceUnixEpoch) -> i64 {
    i64::from(ts.0)
}

fn body(m: &MessageType) -> (Kind, String) {
    match m {
        MessageType::Text(t) => (Kind::Text, t.body.clone()),
        MessageType::Emote(t) => (Kind::Emote, t.body.clone()),
        MessageType::Notice(t) => (Kind::Notice, t.body.clone()),
        MessageType::Image(i) => (Kind::Text, format!("[image] {}", i.body)),
        MessageType::File(f) => (Kind::Text, format!("[file] {}", f.body)),
        MessageType::Video(v) => (Kind::Text, format!("[video] {}", v.body)),
        MessageType::Audio(a) => (Kind::Text, format!("[audio] {}", a.body)),
        MessageType::Location(l) => (Kind::Text, format!("[location] {} {}", l.body, l.geo_uri)),
        MessageType::VerificationRequest(_) => (Kind::Info, "asked to verify; /accept to go ahead".into()),
        other => (Kind::Text, other.body().to_string()),
    }
}

fn prefix(p: UserPowerLevel) -> String {
    match p {
        UserPowerLevel::Infinite => "~".into(),
        UserPowerLevel::Int(i) if i64::from(i) >= 100 => "@".into(),
        UserPowerLevel::Int(i) if i64::from(i) >= 50 => "%".into(),
        _ => String::new(),
    }
}

struct Shared {
    tx: mpsc::UnboundedSender<Update>,
    /// The first sync brings old messages: show them as history, without highlights.
    initial: AtomicBool,
    request: Mutex<Option<VerificationRequest>>,
    sas: Mutex<Option<SasVerification>>,
    /// The room list last sent, so unchanged lists aren't sent again.
    rooms: Mutex<Vec<RoomInfo>>,
}

impl Shared {
    fn say(&self, u: Update) {
        let _ = self.tx.send(u);
    }
    fn status(&self, s: impl Into<String>) {
        self.say(Update::Status(s.into()));
    }
}

async fn sender_name(room: &Room, user: &UserId) -> String {
    match room.get_member_no_sync(user).await {
        Ok(Some(m)) => m.name().to_string(),
        _ => user.localpart().to_string(),
    }
}

fn mentions(client: &Client, text: &str, name: &str) -> bool {
    let local = client.user_id().map(|u| u.localpart().to_lowercase()).unwrap_or_default();
    let t = text.to_lowercase();
    (!local.is_empty() && t.contains(&local)) || (!name.is_empty() && t.contains(&name.to_lowercase()))
}

async fn room_info(room: &Room) -> RoomInfo {
    let counts = room.unread_notification_counts();
    RoomInfo {
        id: room.room_id().to_string(),
        name: room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| room.room_id().to_string()),
        topic: room.topic().unwrap_or_default(),
        encrypted: room.encryption_state().is_encrypted(),
        direct: room.is_direct().await.unwrap_or(false),
        unread: counts.notification_count,
        highlights: counts.highlight_count,
    }
}

async fn send_rooms(client: &Client, sh: &Shared) {
    let mut rooms = vec![];
    for r in client.joined_rooms() {
        rooms.push(room_info(&r).await);
    }
    rooms.sort_by(|a, b| a.direct.cmp(&b.direct).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let mut last = sh.rooms.lock().await;
    if *last != rooms {
        last.clone_from(&rooms);
        sh.say(Update::Rooms(rooms));
    }
}

fn on_messages(client: &Client, sh: Arc<Shared>) {
    let s = sh.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room, client: Client| {
        let sh = s.clone();
        async move {
            let (kind, text) = body(&ev.content.msgtype);
            let own = client.user_id() == Some(&*ev.sender);
            let from = sender_name(&room, &ev.sender).await;
            let history = sh.initial.load(Ordering::Relaxed);
            let me = match client.user_id() {
                Some(u) => sender_name(&room, u).await,
                None => String::new(),
            };
            let direct = room.is_direct().await.unwrap_or(false);
            let highlight = !own && !history && (direct || mentions(&client, &text, &me));
            let msg = Msg { time: ms(ev.origin_server_ts), kind, from, text, own, history, highlight };
            sh.say(Update::Message { room: room.room_id().to_string(), msg });
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype
                && !own
                && let Some(req) = client.encryption().get_verification_request(&ev.sender, ev.event_id.as_str()).await
            {
                *sh.request.lock().await = Some(req);
            }
        }
    });
    let s = sh.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
        let sh = s.clone();
        async move {
            let from = sender_name(&room, &ev.sender).await;
            let msg = Msg {
                time: ms(ev.origin_server_ts),
                kind: Kind::Undecryptable,
                from,
                text: "[can't decrypt this message yet: verify this session or /recover your key]".into(),
                own: false,
                history: sh.initial.load(Ordering::Relaxed),
                highlight: false,
            };
            sh.say(Update::Message { room: room.room_id().to_string(), msg });
        }
    });
    let s = sh.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomMemberEvent, room: Room| {
        let sh = s.clone();
        async move {
            let who = ev.content.displayname.clone().unwrap_or_else(|| ev.state_key.localpart().to_string());
            let (kind, text) = match ev.content.membership {
                MembershipState::Join => {
                    if ev.prev_content().is_some_and(|p| p.membership == MembershipState::Join) {
                        return;
                    }
                    (Kind::Join, format!("{who} joined"))
                }
                MembershipState::Leave => (Kind::Leave, format!("{who} left")),
                MembershipState::Ban => (Kind::Leave, format!("{who} was banned")),
                MembershipState::Invite => (Kind::Info, format!("{who} was invited")),
                _ => return,
            };
            let msg = Msg {
                time: ms(ev.origin_server_ts),
                kind,
                from: who,
                text,
                own: false,
                history: sh.initial.load(Ordering::Relaxed),
                highlight: false,
            };
            sh.say(Update::Message { room: room.room_id().to_string(), msg });
        }
    });
    let s = sh.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomTopicEvent, room: Room| {
        let sh = s.clone();
        async move {
            let from = sender_name(&room, &ev.sender).await;
            let msg = Msg {
                time: ms(ev.origin_server_ts),
                kind: Kind::Topic,
                text: format!("{from} set the topic: {}", ev.content.topic),
                from,
                own: false,
                history: sh.initial.load(Ordering::Relaxed),
                highlight: false,
            };
            sh.say(Update::Message { room: room.room_id().to_string(), msg });
        }
    });
    // Verification requests and starts from other sessions.
    let s = sh.clone();
    client.add_event_handler(move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| {
        let sh = s.clone();
        async move {
            if let Some(req) =
                client.encryption().get_verification_request(&ev.sender, &ev.content.transaction_id).await
            {
                let who = if req.is_self_verification() {
                    "Another of your sessions".to_string()
                } else {
                    ev.sender.to_string()
                };
                sh.say(Update::Verify(format!(
                    "{who} wants to verify this session. Type /accept to compare emoji, or /cancel."
                )));
                *sh.request.lock().await = Some(req);
            }
        }
    });
    let s = sh.clone();
    client.add_event_handler(move |ev: ToDeviceKeyVerificationStartEvent, client: Client| {
        let sh = s.clone();
        async move {
            if let Some(Verification::SasV1(sas)) =
                client.encryption().get_verification(&ev.sender, ev.content.transaction_id.as_str()).await
                && !sas.we_started()
            {
                if let Err(e) = sas.accept().await {
                    sh.say(Update::Error(format!("Verification: {e}")));
                    return;
                }
                watch_sas(sh.clone(), sas).await;
            }
        }
    });
}

/// Follow a SAS verification to the end, showing the emoji when they arrive.
async fn watch_sas(sh: Arc<Shared>, sas: SasVerification) {
    *sh.sas.lock().await = Some(sas.clone());
    tokio::spawn(async move {
        let mut changes = sas.changes();
        while let Some(state) = changes.next().await {
            match state {
                SasState::KeysExchanged { emojis, decimals } => {
                    let text = match emojis {
                        Some(e) => e
                            .emojis
                            .iter()
                            .map(|e| format!("{} {}", e.symbol, e.description))
                            .collect::<Vec<_>>()
                            .join("   "),
                        None => format!("{} {} {}", decimals.0, decimals.1, decimals.2),
                    };
                    sh.say(Update::Verify(format!(
                        "Compare these with the other session:  {text}\nType /confirm if they match, /mismatch if they don't."
                    )));
                }
                SasState::Done { .. } => {
                    let who = sas.other_device();
                    sh.say(Update::Verify(format!(
                        "Verified {} ({}). Encrypted messages to and from it are now trusted.",
                        who.display_name().unwrap_or("the other session"),
                        who.device_id()
                    )));
                    break;
                }
                SasState::Cancelled(info) => {
                    sh.say(Update::Verify(format!("Verification cancelled: {}", info.reason())));
                    break;
                }
                _ => {}
            }
        }
    });
}

/// Follow a request until it turns into an emoji comparison.
async fn watch_request(sh: Arc<Shared>, req: VerificationRequest) {
    *sh.request.lock().await = Some(req.clone());
    tokio::spawn(async move {
        let mut changes = req.changes();
        while let Some(state) = changes.next().await {
            match state {
                VerificationRequestState::Ready { .. } if req.we_started() => match req.start_sas().await {
                    Ok(Some(sas)) => {
                        watch_sas(sh.clone(), sas).await;
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        sh.say(Update::Error(format!("Verification: {e}")));
                        break;
                    }
                },
                VerificationRequestState::Transitioned { verification, .. } => {
                    if let Some(sas) = verification.sas() {
                        if !sas.we_started() {
                            let _ = sas.accept().await;
                        }
                        watch_sas(sh.clone(), sas).await;
                    }
                    break;
                }
                VerificationRequestState::Done => break,
                VerificationRequestState::Cancelled(info) => {
                    sh.say(Update::Verify(format!("Verification cancelled: {}", info.reason())));
                    break;
                }
                _ => {}
            }
        }
    });
}

async fn history(client: &Client, sh: &Shared, room_id: &str) -> Result<(), String> {
    let id = <&RoomId>::try_from(room_id).map_err(|e| e.to_string())?;
    let room = client.get_room(id).ok_or("No such room")?;
    let mut opts = MessagesOptions::backward();
    opts.limit = 40u32.into();
    let msgs = room.messages(opts).await.map_err(|e| e.to_string())?;
    let me = client.user_id().map(|u| u.to_owned());
    for ev in msgs.chunk.into_iter().rev() {
        let Ok(ev) = ev.raw().deserialize() else { continue };
        let (sender, ts) = (ev.sender().to_owned(), ev.origin_server_ts());
        let (kind, text) = match ev {
            AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
                SyncMessageLikeEvent::Original(m),
            )) => body(&m.content.msgtype),
            AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_)) => {
                (Kind::Undecryptable, "[can't decrypt this message]".to_string())
            }
            _ => continue,
        };
        let from = sender_name(&room, &sender).await;
        let own = me.as_deref() == Some(&*sender);
        let msg = Msg { time: ms(ts), kind, from, text, own, history: true, highlight: false };
        sh.say(Update::Message { room: room_id.to_string(), msg });
    }
    Ok(())
}

async fn members(client: &Client, sh: &Shared, room_id: &str) -> Result<(), String> {
    let id = <&RoomId>::try_from(room_id).map_err(|e| e.to_string())?;
    let room = client.get_room(id).ok_or("No such room")?;
    let list = room.members(RoomMemberships::JOIN).await.map_err(|e| e.to_string())?;
    let mut users: Vec<(String, String)> =
        list.iter().map(|m| (prefix(m.power_level()), m.name().to_string())).collect();
    users.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    sh.say(Update::Members { room: room_id.to_string(), users });
    Ok(())
}

async fn command(client: &Client, sh: &Arc<Shared>, cmd: Cmd) -> Result<Option<Stop>, String> {
    let e = |e: &dyn std::fmt::Display| e.to_string();
    match cmd {
        Cmd::Quit => return Ok(Some(Stop::Quit)),
        Cmd::Logout => {
            client.logout().await.map_err(|x| e(&x))?;
            return Ok(Some(Stop::LoggedOut));
        }
        Cmd::Send { room, text, emote } => {
            let id = <&RoomId>::try_from(room.as_str()).map_err(|x| e(&x))?;
            let r = client.get_room(id).ok_or("No such room")?;
            let content = if emote {
                RoomMessageEventContent::emote_plain(text)
            } else {
                RoomMessageEventContent::text_plain(text)
            };
            r.send(content).await.map_err(|x| e(&x))?;
        }
        Cmd::Join(what) => {
            let id = <&RoomOrAliasId>::try_from(what.as_str()).map_err(|_| format!("{what} is not a room address"))?;
            let r = client.join_room_by_id_or_alias(id, &[]).await.map_err(|x| e(&x))?;
            sh.status(format!("Joined {}", room_info(&r).await.name));
            send_rooms(client, sh).await;
        }
        Cmd::Leave(room) => {
            let id: OwnedRoomId = room.parse().map_err(|x| e(&x))?;
            if let Some(r) = client.get_room(&id) {
                r.leave().await.map_err(|x| e(&x))?;
            }
            send_rooms(client, sh).await;
        }
        Cmd::History(room) => history(client, sh, &room).await?,
        Cmd::Members(room) => members(client, sh, &room).await?,
        Cmd::Verify(who) => {
            let me = client.user_id().ok_or("Not signed in")?.to_owned();
            let user = match &who {
                Some(u) => UserId::parse(u.as_str()).map_err(|x| e(&x))?,
                None => me.clone(),
            };
            let identity = client
                .encryption()
                .get_user_identity(&user)
                .await
                .map_err(|x| e(&x))?
                .ok_or("No cross-signing identity yet; set up encryption in another client first")?;
            let req = identity.request_verification().await.map_err(|x| e(&x))?;
            sh.say(Update::Verify(match who {
                Some(u) => format!("Asked {u} to verify. They need to accept in their client."),
                None => "Asked your other sessions to verify this one. Accept in one of them.".into(),
            }));
            watch_request(sh.clone(), req).await;
        }
        Cmd::Accept => {
            let req = sh.request.lock().await.clone().ok_or("Nothing to accept")?;
            req.accept().await.map_err(|x| e(&x))?;
            sh.say(Update::Verify("Accepted. Waiting for the emoji…".into()));
            watch_request(sh.clone(), req).await;
        }
        Cmd::Confirm => {
            let sas = sh.sas.lock().await.clone().ok_or("No emoji to confirm")?;
            sas.confirm().await.map_err(|x| e(&x))?;
            sh.say(Update::Verify("Confirmed. Waiting for the other session…".into()));
        }
        Cmd::Mismatch => {
            let sas = sh.sas.lock().await.clone().ok_or("No verification running")?;
            sas.mismatch().await.map_err(|x| e(&x))?;
        }
        Cmd::Cancel => {
            if let Some(sas) = sh.sas.lock().await.take() {
                sas.cancel().await.map_err(|x| e(&x))?;
            } else if let Some(req) = sh.request.lock().await.take() {
                req.cancel().await.map_err(|x| e(&x))?;
            } else {
                return Err("No verification running".into());
            }
        }
        Cmd::Recover(key) => {
            client.encryption().recovery().recover(key.trim()).await.map_err(|x| e(&x))?;
            sh.status("Recovered your keys from backup. Older encrypted messages will decrypt as they load.");
        }
        Cmd::Devices => {
            let me = client.user_id().ok_or("Not signed in")?.to_owned();
            let devices = client.encryption().get_user_devices(&me).await.map_err(|x| e(&x))?;
            let mine = client.device_id().map(|d| d.to_owned());
            for d in devices.devices() {
                let this = if Some(d.device_id()) == mine.as_deref() { " (this one)" } else { "" };
                let trust = if d.is_verified() { "verified" } else { "not verified" };
                sh.status(format!("{} {}{this}: {trust}", d.device_id(), d.display_name().unwrap_or("")));
            }
        }
    }
    Ok(None)
}

/// How to start a connection.
pub struct Options {
    pub account: Account,
    /// The folder for the encrypted local store (keys, room state).
    pub store: PathBuf,
    /// The store's passphrase; see [`new_passphrase`].
    pub passphrase: String,
    /// JSON from an earlier [`Update::Ready`].
    pub session: Option<String>,
    pub password: Option<String>,
    /// `socks5://host:port` or `http://host:port`, as the network settings say.
    pub proxy: Option<String>,
}

/// Sign in (restoring the session when given, else with the password) and stay connected
/// until a [`Cmd::Quit`] or a failure.
pub async fn run(opts: Options, tx: mpsc::UnboundedSender<Update>, mut cmds: mpsc::UnboundedReceiver<Cmd>) -> Stop {
    let Options { account, store, passphrase, session, password, proxy } = opts;
    let sh = Arc::new(Shared {
        tx,
        initial: AtomicBool::new(true),
        request: Mutex::new(None),
        sas: Mutex::new(None),
        rooms: Mutex::new(vec![]),
    });
    let fail = |e: &dyn std::fmt::Display| Stop::Failed(e.to_string());
    let mut builder = Client::builder()
        .sqlite_store(&store, Some(&passphrase))
        .user_agent(concat!("Alouette/", env!("CARGO_PKG_VERSION")));
    if let Some(p) = proxy.filter(|p| !p.is_empty()) {
        builder = builder.proxy(p);
    }
    let builder = if account.homeserver.starts_with("http") {
        builder.homeserver_url(&account.homeserver)
    } else {
        builder.server_name_or_homeserver_url(&account.homeserver)
    };
    let client = match builder.build().await {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let restored = match session.as_deref().map(serde_json::from_str::<MatrixSession>) {
        Some(Ok(s)) => client.matrix_auth().restore_session(s, Default::default()).await.is_ok(),
        _ => false,
    };
    let mut new_session = None;
    if !restored {
        let Some(pw) = password else { return Stop::NeedPassword };
        let user = full_user_id(&account.user, &account.homeserver);
        sh.status(format!("Signing in as {user}…"));
        if let Err(e) =
            client.matrix_auth().login_username(&user, &pw).initial_device_display_name("Alouette").send().await
        {
            return fail(&e);
        }
        new_session = client.matrix_auth().session().and_then(|s| serde_json::to_string(&s).ok());
    }
    let (user_id, device_id) = (
        client.user_id().map(|u| u.to_string()).unwrap_or_default(),
        client.device_id().map(|d| d.to_string()).unwrap_or_default(),
    );
    sh.say(Update::Ready { user_id, device_id, session: new_session });
    on_messages(&client, sh.clone());

    sh.status("Catching up…");
    let first = match client.sync_once(SyncSettings::default().timeout(Duration::from_secs(5))).await {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    sh.initial.store(false, Ordering::Relaxed);
    send_rooms(&client, &sh).await;
    match client.encryption().cross_signing_status().await {
        Some(s) if s.is_complete() => {}
        _ => sh.status(
            "This session isn't verified yet, so older encrypted messages may not decrypt. \
             Type /verify and accept in another session, or /recover with your recovery key.",
        ),
    }

    let settings = SyncSettings::default().token(first.next_batch).timeout(Duration::from_secs(30));
    let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<Stop>();
    let sync_client = client.clone();
    let sync_sh = sh.clone();
    let sync = tokio::spawn(async move {
        let r = sync_client
            .sync_with_callback(settings, |_resp| {
                let (c, s) = (sync_client.clone(), sync_sh.clone());
                async move {
                    send_rooms(&c, &s).await;
                    matrix_sdk::LoopCtrl::Continue
                }
            })
            .await;
        if let Err(e) = r {
            let _ = stop_tx.send(Stop::Failed(e.to_string()));
        }
    });
    let stop = loop {
        tokio::select! {
            c = cmds.recv() => match c {
                None => break Stop::Quit,
                Some(c) => match command(&client, &sh, c).await {
                    Ok(Some(stop)) => break stop,
                    Ok(None) => {}
                    Err(e) => sh.say(Update::Error(e)),
                },
            },
            s = stop_rx.recv() => break s.unwrap_or(Stop::Quit),
        }
    };
    sync.abort();
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids() {
        assert_eq!(full_user_id("me", "matrix.org"), "@me:matrix.org");
        assert_eq!(full_user_id("@me", "https://matrix.example.org/"), "@me:matrix.example.org");
        assert_eq!(full_user_id("@me:other.org", "matrix.org"), "@me:other.org");
        assert_eq!(new_passphrase().len(), 64);
    }
}
