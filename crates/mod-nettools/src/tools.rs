//! Running a command in the background and streaming what it finds.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use alouette_core::TaskCtx;
use alouette_lookup::sources::{self, PortState};
use alouette_lookup::{Out, Section, Style};
use alouette_net::Net;

use crate::command::Command;

/// A message from a running command to its window.
#[derive(Debug, Clone)]
pub enum Msg {
    Line(Out),
    Section(Section),
    Done,
}

pub async fn run(cmd: Command, net: Net, ctx: TaskCtx, send: impl Fn(Msg)) -> Result<(), String> {
    let r = match cmd {
        Command::Ping { host, count, tcp_port } => ping(&net, &host, count, tcp_port, &ctx, &send).await,
        Command::Trace { host } => trace(&host, &ctx, &send).await,
        Command::Dns { rtype, name } => {
            send(Msg::Section(sources::dns_lookup(&net, &name, &rtype).await));
            Ok(())
        }
        Command::Whois { query } => {
            send(Msg::Section(sources::whois(&net, &query).await));
            Ok(())
        }
        Command::Rdap { query } => {
            let q = alouette_lookup::Query::parse(&query);
            let section = match q.kind {
                alouette_lookup::QueryKind::Ip(ip) => sources::rdap(&net, "ip", &ip.to_string()).await,
                alouette_lookup::QueryKind::Asn(n) => sources::rdap(&net, "autnum", &n.to_string()).await,
                _ => sources::rdap(&net, "domain", &query).await,
            };
            send(Msg::Section(section));
            Ok(())
        }
        Command::Asn { query } => {
            send(Msg::Section(sources::asn(&net, &query).await));
            Ok(())
        }
        Command::Ports { host, ports } => scan(&net, &host, &ports, &ctx, &send).await,
        Command::Tls { host, port } => {
            send(Msg::Section(alouette_lookup::tls::inspect(&net, &host, port).await));
            Ok(())
        }
        Command::Mdns => mdns(&ctx, &send).await,
    };
    send(Msg::Done);
    r
}

fn line(style: Style, text: impl Into<String>) -> Msg {
    Msg::Line(Out::new(style, text))
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

async fn ping(
    net: &Net,
    host: &str,
    count: u32,
    tcp_port: Option<u16>,
    ctx: &TaskCtx,
    send: &impl Fn(Msg),
) -> Result<(), String> {
    let addrs = net.resolve(host, tcp_port.unwrap_or(0)).await.map_err(|e| e.to_string())?;
    let ip = addrs.first().map(|a| a.ip()).ok_or("no address")?;
    let mut icmp = None;
    if tcp_port.is_none() {
        match icmp_client(ip) {
            Ok(c) => icmp = Some(c),
            Err(e) => send(line(
                Style::Dim,
                format!("ICMP is not available here ({e}); timing TCP handshakes on port 443 instead"),
            )),
        }
    }
    let port = tcp_port.unwrap_or(443);
    let how = if icmp.is_some() { "ICMP echo".to_string() } else { format!("TCP port {port}") };
    send(line(Style::Heading, format!("PING {host} ({ip}) with {how}, {count} times")));
    let mut times = vec![];
    let mut pinger = match &icmp {
        Some(c) => Some(c.pinger(ip, ping_id()).await),
        None => None,
    };
    if let Some(p) = pinger.as_mut() {
        p.timeout(Duration::from_secs(2));
    }
    for seq in 0..count {
        if ctx.is_cancelled() {
            break;
        }
        let started = Instant::now();
        let result = match pinger.as_mut() {
            Some(p) => p
                .ping(ping_seq(seq), &[0u8; 56])
                .await
                .map(|(_, d)| d)
                .map_err(|e| e.to_string()),
            None => sources::tcp_ping(net, &ip.to_string(), port).await.map(|(d, _)| d),
        };
        match result {
            Ok(d) => {
                times.push(d);
                send(line(Style::Normal, format!("reply from {ip}: seq={seq} time={}", ms(d))));
            }
            Err(e) => send(line(Style::Bad, format!("seq={seq}: {e}"))),
        }
        ctx.progress((seq + 1) as f32 / count as f32);
        let wait = Duration::from_secs(1).saturating_sub(started.elapsed());
        if seq + 1 < count {
            tokio::time::sleep(wait).await;
        }
    }
    let sent = count as usize;
    let got = times.len();
    let loss = 100.0 * (sent - got) as f64 / sent.max(1) as f64;
    if got > 0 {
        let min = times.iter().min().copied().unwrap_or_default();
        let max = times.iter().max().copied().unwrap_or_default();
        let avg = times.iter().sum::<Duration>() / got as u32;
        send(line(
            if loss == 0.0 { Style::Good } else { Style::Bad },
            format!("{got} of {sent} replied ({loss:.0}% loss) · min {} · avg {} · max {}", ms(min), ms(avg), ms(max)),
        ));
        ctx.detail(format!("avg {}", ms(avg)));
        Ok(())
    } else {
        send(line(Style::Bad, format!("no replies from {host}")));
        Err(format!("no replies from {host}"))
    }
}

#[cfg(unix)]
fn ping_id() -> surge_ping::PingIdentifier {
    surge_ping::PingIdentifier(std::process::id() as u16)
}
#[cfg(unix)]
fn ping_seq(seq: u32) -> surge_ping::PingSequence {
    surge_ping::PingSequence(seq as u16)
}
#[cfg(not(unix))]
fn ping_id() {}
#[cfg(not(unix))]
fn ping_seq(_seq: u32) {}

#[cfg(unix)]
fn icmp_client(ip: IpAddr) -> std::io::Result<surge_ping::Client> {
    let kind = if ip.is_ipv6() { surge_ping::ICMP::V6 } else { surge_ping::ICMP::V4 };
    surge_ping::Client::new(&surge_ping::Config::builder().kind(kind).build())
}

#[cfg(not(unix))]
fn icmp_client(_ip: IpAddr) -> std::io::Result<NoIcmp> {
    Err(std::io::Error::other("ICMP needs administrator rights on this system"))
}

/// Stands in for the ICMP client where unprivileged ICMP is not available.
#[cfg(not(unix))]
struct NoIcmp;

#[cfg(not(unix))]
impl NoIcmp {
    async fn pinger(&self, _ip: IpAddr, _id: ()) -> NoPinger {
        NoPinger
    }
}

#[cfg(not(unix))]
struct NoPinger;

#[cfg(not(unix))]
impl NoPinger {
    fn timeout(&mut self, _d: Duration) {}
    async fn ping(&mut self, _seq: (), _payload: &[u8]) -> Result<((), Duration), String> {
        Err("unavailable".into())
    }
}

async fn trace(host: &str, ctx: &TaskCtx, send: &impl Fn(Msg)) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("tracert", &["-d", "-w", "2000"])]
    } else {
        &[("traceroute", &["-n", "-q", "1", "-w", "2"]), ("tracepath", &["-n"])]
    };
    for (prog, args) in candidates {
        let child = tokio::process::Command::new(prog)
            .args(*args)
            .arg(host)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let Ok(mut child) = child else { continue };
        send(line(Style::Dim, format!("using the system {prog}")));
        let out = child.stdout.take().ok_or("no output")?;
        let mut lines = BufReader::new(out).lines();
        loop {
            tokio::select! {
                l = lines.next_line() => match l {
                    Ok(Some(l)) => send(line(Style::Normal, l)),
                    _ => break,
                },
                _ = ctx.cancelled() => {
                    let _ = child.kill().await;
                    return Ok(());
                }
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        return if status.success() { Ok(()) } else { Err(format!("{prog} exited with {status}")) };
    }
    send(line(Style::Bad, "No traceroute program found. Install traceroute (or tracepath) to use this."));
    Err("no traceroute program".into())
}

async fn scan(net: &Net, host: &str, ports: &[u16], ctx: &TaskCtx, send: &impl Fn(Msg)) -> Result<(), String> {
    use futures::StreamExt;
    send(line(Style::Heading, format!("Checking {} TCP ports on {host}", ports.len())));
    let timeout = Duration::from_secs(3);
    let total = ports.len();
    let mut done = 0;
    let mut open = 0;
    let mut results: Vec<(u16, PortState)> = vec![];
    let mut stream = futures::stream::iter(ports.iter().copied())
        .map(|p| async move { (p, sources::check_port(net, host, p, timeout).await) })
        .buffer_unordered(32);
    while let Some((p, st)) = stream.next().await {
        done += 1;
        ctx.progress(done as f32 / total as f32);
        if let PortState::Open(d) = &st {
            open += 1;
            let name = sources::port_name(p);
            send(line(Style::Good, format!("{p:>5}/tcp  open      {name:<14} {}", ms(*d))));
        }
        results.push((p, st));
        if ctx.is_cancelled() {
            break;
        }
    }
    let filtered = results.iter().filter(|(_, s)| *s == PortState::Filtered).count();
    let closed = results.len() - open - filtered;
    send(line(Style::Dim, format!("{open} open, {closed} closed, {filtered} no answer (filtered)")));
    ctx.detail(format!("{open} open"));
    Ok(())
}

async fn mdns(ctx: &TaskCtx, send: &impl Fn(Msg)) -> Result<(), String> {
    const TYPES: &[(&str, &str)] = &[
        ("_http._tcp.local.", "Web"),
        ("_https._tcp.local.", "Web (TLS)"),
        ("_ssh._tcp.local.", "SSH"),
        ("_sftp-ssh._tcp.local.", "SFTP"),
        ("_smb._tcp.local.", "File sharing"),
        ("_afpovertcp._tcp.local.", "File sharing (AFP)"),
        ("_ipp._tcp.local.", "Printer"),
        ("_printer._tcp.local.", "Printer"),
        ("_airplay._tcp.local.", "AirPlay"),
        ("_googlecast._tcp.local.", "Cast"),
        ("_raop._tcp.local.", "Speaker"),
        ("_workstation._tcp.local.", "Computer"),
        ("_device-info._tcp.local.", "Device"),
        ("_gopher._tcp.local.", "Gopher"),
    ];
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| format!("mDNS is not available: {e}"))?;
    send(line(Style::Heading, "Listening for services on the local network for 6 seconds…"));
    let receivers: Vec<_> = TYPES.iter().filter_map(|(t, label)| daemon.browse(t).ok().map(|r| (r, *label))).collect();
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut found = 0;
    while Instant::now() < deadline && !ctx.is_cancelled() {
        let mut idle = true;
        for (rx, label) in &receivers {
            while let Ok(ev) = rx.try_recv() {
                if let mdns_sd::ServiceEvent::ServiceResolved(info) = ev {
                    idle = false;
                    found += 1;
                    let mut addrs: Vec<String> = info.addresses.iter().map(|a| a.to_string()).collect();
                    addrs.sort();
                    let name = info.fullname.split("._").next().unwrap_or(&info.fullname).to_string();
                    send(line(
                        Style::Normal,
                        format!(
                            "{label:<18} {name}  {}:{}  {}",
                            info.host.trim_end_matches('.'),
                            info.port,
                            addrs.join(" ")
                        ),
                    ));
                }
            }
        }
        if idle {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        ctx.progress(1.0 - deadline.saturating_duration_since(Instant::now()).as_secs_f32() / 6.0);
    }
    let _ = daemon.shutdown();
    send(line(Style::Dim, format!("{found} services found")));
    Ok(())
}
