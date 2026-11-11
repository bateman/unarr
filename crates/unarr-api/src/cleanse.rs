//! `CleanseLogMessage`: what must not reach a log is taken out of every
//! line before it is written anywhere, the console included. An error of
//! an HTTP request carries its URL, and that URL the API key, the passkey
//! or the token the request was made with.
//!
//! The rules are the originals', one for one and in their order. Each
//! names what it removes as a group called `secret` (`secret2` where the
//! original names two groups alike, which this engine does not allow).

use std::borrow::Cow;
use std::net::Ipv4Addr;
use std::sync::LazyLock;

use fancy_regex::Regex;

const REMOVED: &str = "(removed)";

/// A rule, and for the one whose secret repeats inside a match the rule
/// that finds each of them there.
struct Rule {
    /// The rule can only match a text that holds one of these, in lower
    /// case. Most lines hold none of any rule, and finding that out this
    /// way costs a fraction of evaluating the patterns.
    needs: &'static [&'static str],
    pattern: Regex,
    within: Option<Regex>,
}

fn rule(needs: &'static [&'static str], pattern: &str) -> Rule {
    Rule {
        needs,
        pattern: Regex::new(pattern).expect("a cleansing rule"),
        within: None,
    }
}

// `$` in the originals is the end of the message; here it is the end of a
// line as well (`(?m)`), so that a secret ending a line of a longer text
// goes too.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // Url
        rule(
            &["?", "&"],
            r#"(?im)(?<=\?|&)(apikey|token|passkey|auth|authkey|user|uid|api|[a-z_]*apikey|account|passwd)=(?<secret>[^&="]+?)(?=[ "&=]|$)"#,
        ),
        rule(
            &["username=", "password="],
            r"(?im)(?<=\?|&)[^=]*?(username|password)=(?<secret>[^&=]+?)(?= |&|$)",
        ),
        rule(
            &["torrentleech"],
            r"(?i)rss(24h)?\.torrentleech\.org/(?!rss)(?<secret>[0-9a-z]+)",
        ),
        rule(
            &["torrentleech"],
            r"(?i)torrentleech\.org/rss/download/[0-9]+/(?<secret>[0-9a-z]+)",
        ),
        // The original captures `secret` once per repetition; here the
        // address is found first and each of its secrets inside it.
        Rule {
            needs: &["iptorrents"],
            pattern: Regex::new(r"(?i)iptorrents\.com/[/a-z0-9?&;=]*").expect("a cleansing rule"),
            within: Some(Regex::new(r"(?i)(?<=[?&;])(u|tp)=(?<secret>[^&=;]+)").expect("a cleansing rule")),
        },
        rule(&["/fetch/"], r"/fetch/[a-z0-9]{32}/(?<secret>[a-z0-9]{32})"),
        rule(
            &["getnzb"],
            r"(?im)getnzb.*?(?<=\?|&)(r)=(?<secret>[^&=]+?)(?= |&|$)",
        ),
        rule(
            &["token=", "username=", "password=", "passwrd="],
            r"(?im)\b(\w*)?(_?(?<!use)(?<!get_)token|username|passwo?rd)=(?<secret>[^&=]+?)(?= |&|$|;)",
        ),
        rule(
            &["-hd"],
            r"(?i)-hd.me/torrent/[a-z0-9-]\.[0-9]+\.(?<secret>[0-9a-z]+)",
        ),
        // Trackers' announce keys
        rule(
            &["announce"],
            r"(?i)announce(\.php)?(/|%2f|%3fpasskey%3d)(?<secret>[a-z0-9]{16,})|(?<secret2>[a-z0-9]{16,})(/|%2f)announce",
        ),
        // Path
        rule(&["c:\\users\\"], r#"(?im)C:\\Users\\(?<secret>[^\"]+?)(\\|$)"#),
        rule(
            &["/home/", "/users/"],
            r#"(?im)/(home|Users)/(?<secret>[^/"]+?)(/|$)"#,
        ),
        // NzbGet
        rule(
            &["\"name\""],
            r#"(?i)"Name"\s*:\s*"[^"]*(username|password)"\s*,\s*"Value"\s*:\s*"(?<secret>[^"]+?)""#,
        ),
        // Sabnzbd
        rule(
            &["username", "password", "key\""],
            r#"(?i)"[^"]*(username|password|api_?key|nzb_key)"\s*:\s*"(?<secret>[^"]+?)""#,
        ),
        rule(
            &["\"email_"],
            r#"(?i)"email_(account|to|from|pwd)"\s*:\s*"(?<secret>[^"]+?)""#,
        ),
        // uTorrent
        rule(
            &["username\"", "password\""],
            r#"(?i)\["[a-z._]*(username|password)",\d,"(?<secret>[^"]+?)""#,
        ),
        rule(
            &["boss_key", "proxy.proxy"],
            r#"(?i)\["(boss_key|boss_key_salt|proxy\.proxy)",\d,"(?<secret>[^"]+?)""#,
        ),
        // Deluge
        rule(&["login("], r#"(?i)auth.login\("(?<secret>[^"]+?)""#),
        // BroadcastheNet
        rule(
            &["gettorrents"],
            r#"(?i)"?method"?\s*:\s*"(getTorrents)",\s*"?params"?\s*:\s*\[\s*"(?<secret>[^"]+?)""#,
        ),
        rule(&["gettorrents("], r#"(?i)getTorrents\("(?<secret>[^"]+?)""#),
        rule(
            &["authkey=", "torrent_pass="],
            r#"(?im)(?<=\?|&)(authkey|torrent_pass)=(?<secret>[^&=]+?)(?="|&|$)"#,
        ),
        // Plex
        rule(
            &["x-plex-"],
            r"(?im)(?<=\?|&)(X-Plex-Client-Identifier|X-Plex-Token)=(?<secret>[^&=]+?)(?= |&|$)",
        ),
        // Notifiarr
        rule(
            &["/notification/sonarr/"],
            r"(?i)api/v[0-9]/notification/sonarr/(?<secret>[\w-]+)",
        ),
        // Discord
        rule(
            &["/api/webhooks/"],
            r"(?i)discord.com/api/webhooks/((?<secret>[\w-]+)/)?(?<secret2>[\w-]+)",
        ),
        // Telegram
        rule(
            &["telegram"],
            r"(?i)api.telegram.org/bot(?<id>[\d]+):(?<secret>[\w-]+)/",
        ),
    ]
});

static REMOTE_IP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:Auth-\w+(?<!Failure)(?<!Unauthorized) ip|from) (\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})")
        .expect("the remote address rule")
});

/// `text` with the groups called `secret...` of every match of `pattern`
/// replaced; `None` when nothing matched.
fn remove(pattern: &Regex, text: &str) -> Option<String> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for found in pattern.captures_iter(text) {
        // A rule that cannot be evaluated on this text removes nothing.
        let Ok(found) = found else { break };
        for name in pattern.capture_names().flatten() {
            if name.starts_with("secret") {
                if let Some(group) = found.name(name) {
                    ranges.push((group.start(), group.end()));
                }
            }
        }
    }
    if ranges.is_empty() {
        return None;
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in ranges {
        if start < at {
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(REMOVED);
        at = end;
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// The matches of `outer` in `text`, each with the secrets `inner` finds
/// in it replaced.
fn remove_within(outer: &Regex, inner: &Regex, text: &str) -> Option<String> {
    let mut out = String::new();
    let mut at = 0;
    let mut changed = false;
    for found in outer.find_iter(text) {
        let Ok(found) = found else { break };
        if let Some(cleansed) = remove(inner, found.as_str()) {
            out.push_str(&text[at..found.start()]);
            out.push_str(&cleansed);
            at = found.end();
            changed = true;
        }
    }
    out.push_str(&text[at..]);
    changed.then_some(out)
}

/// `IsLocalAddress`, for the IPv4 addresses the rule finds.
fn is_local(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local()
}

/// A remote address keeps its first and last number.
fn remove_remote_ips(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut at = 0;
    let mut changed = false;
    for found in REMOTE_IP.captures_iter(text) {
        let Ok(found) = found else { break };
        let Some(group) = found.get(1) else { continue };
        let Ok(ip) = group.as_str().parse::<Ipv4Addr>() else {
            continue;
        };
        if is_local(ip) {
            continue;
        }
        let [first, _, _, last] = ip.octets();
        out.push_str(&text[at..group.start()]);
        out.push_str(&format!("{first}.*.*.{last}"));
        at = group.end();
        changed = true;
    }
    out.push_str(&text[at..]);
    changed.then_some(out)
}

/// `Cleanse`: `message` without the secrets the rules know.
pub fn cleanse(message: &str) -> Cow<'_, str> {
    if message.trim().is_empty() {
        return Cow::Borrowed(message);
    }
    let lower = message.to_ascii_lowercase();
    let mut text = Cow::Borrowed(message);
    for rule in RULES.iter() {
        if !rule.needs.iter().any(|needed| lower.contains(needed)) {
            continue;
        }
        let cleansed = match &rule.within {
            Some(inner) => remove_within(&rule.pattern, inner, &text),
            None => remove(&rule.pattern, &text),
        };
        if let Some(cleansed) = cleansed {
            text = Cow::Owned(cleansed);
        }
    }
    if lower.contains("from ") || lower.contains(" ip ") {
        if let Some(cleansed) = remove_remote_ips(&text) {
            text = Cow::Owned(cleansed);
        }
    }
    text
}

/// A writer that cleanses what goes through it: for the console, whose
/// lines `tracing` formats itself. Each event reaches it as one write.
pub struct Cleansing<W>(pub W);

impl<W: std::io::Write> std::io::Write for Cleansing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match std::str::from_utf8(buf) {
            Ok(text) => self.0.write_all(cleanse(text).as_bytes())?,
            Err(_) => self.0.write_all(buf)?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CleanseLogMessageFixture.should_clean_message`, case for case.
    #[test]
    fn secrets_are_removed() {
        let cases = [
            // Indexer Urls
            r#"https://iptorrents.com/torrents/rss?u=mySecret;tp=mySecret;l5;download"#,
            r#"http://rss.torrentleech.org/mySecret"#,
            r#"https://rss24h.torrentleech.org/mySecret"#,
            r#"http://rss.torrentleech.org/rss/download/12345/01233210/file.name-RLSGRP.torrent"#,
            r#"https://www.torrentleech.org/rss/download/12345/01233210/file.name-RLSGRP.torrent"#,
            r#"https://rss.omgwtfnzbs.org/rss-search.php?catid=19,20&user=sonarr&api=mySecret&eng=1"#,
            r#"https://dognzb.cr/fetch/2b51db35e1912ffc138825a12b9933d2/2b51db35e1910123321025a12b9933d2"#,
            r#"https://baconbits.org/feeds.php?feed=torrents_tv&user=12345&auth=2b51db35e1910123321025a12b9933d2&passkey=mySecret&authkey=2b51db35e1910123321025a12b9933d2"#,
            r#"http://127.0.0.1:9117/dl/indexername?jackett_apikey=flwjiefewklfjacketmySecretsdfldskjfsdlk&path=we0re9f0sdfbase64sfdkfjsdlfjk&file=The+Torrent+File+Name.torrent"#,
            r#"http://nzb.su/getnzb/2b51db35e1912ffc138825a12b9933d2.nzb&i=37292&r=2b51db35e1910123321025a12b9933d2"#,
            r#"https://b-hd.me/torrent/download/auto.343756.is1t1pl127p1sfwur8h4kgyhg1wcsn05"#,
            r#"https://b-hd.me/torrent/download/a-slug-in-the-url.343756.is1t1pl127p1sfwur8h4kgyhg1wcsn05"#,
            // NzbGet
            r#"{ "Name" : "ControlUsername", "Value" : "mySecret" }, { "Name" : "ControlPassword", "Value" : "mySecret" }, "#,
            r#"{ "Name" : "Server1.Username", "Value" : "mySecret" }, { "Name" : "Server1.Password", "Value" : "mySecret" }, "#,
            // Sabnzbd
            r#"http://127.0.0.1:1234/api/call?vv=1&apikey=mySecret"#,
            r#"http://127.0.0.1:1234/api/call?vv=1&ma_username=mySecret&ma_password=mySecret"#,
            r#""config":{"newzbin":{"username":"mySecret","password":"mySecret"}"#,
            r#""nzbxxx":{"username":"mySecret","apikey":"mySecret"}"#,
            r#""growl":{"growl_password":"mySecret","growl_server":""}"#,
            r#""nzbmatrix":{"username":"mySecret","apikey":"mySecret"}"#,
            r#""misc":{"username":"mySecret","api_key":"mySecret","password":"mySecret","nzb_key":"mySecret"}"#,
            r#""servers":[{"username":"mySecret","password":"mySecret"}]"#,
            r#""misc":{"email_account":"mySecret","email_to":[],"email_from":"","email_pwd":"mySecret"}"#,
            // uTorrent
            r#"http://localhost:9091/gui/?token=wThmph5l0ZXfH-a6WOA4lqiLvyjCP0FpMrMeXmySecret_VXBO11HoKL751MAAAAA&list=1"#,
            r#",["boss_key",0,"mySecret",{"access":"Y"}],["boss_key_salt",0,"mySecret",{"access":"W"}]"#,
            r#",["webui.username",2,"mySecret",{"access":"Y"}],["webui.password",2,"mySecret",{"access":"Y"}]"#,
            r#",["webui.uconnect_username",2,"mySecret",{"access":"Y"}],["webui.uconnect_password",2,"mySecret",{"access":"Y"}]"#,
            r#",["proxy.proxy",2,"mySecret",{"access":"Y"}]"#,
            r#",["proxy.username",2,"mySecret",{"access":"Y"}],["proxy.password",2,"mySecret",{"access":"Y"}]"#,
            // Deluge
            r#",{"download_location": "C:\Users\\mySecret mySecret\\Downloads"}"#,
            r#",{"download_location": "/home/mySecret/Downloads"}"#,
            r#",{"download_location": "/Users/mySecret/Downloads"}"#,
            r#"auth.login("mySecret")"#,
            // Download Station
            r#"webapi/entry.cgi?api=(removed)&version=2&method=login&account=01233210&passwd=mySecret&format=sid&session=DownloadStation"#,
            // BroadcastheNet
            r#"method: "getTorrents", "params": [ "mySecret","#,
            r#"getTorrents("mySecret", [asdfasdf], 100, 0)"#,
            r#""DownloadURL":"https:\/\/broadcasthe.net\/torrents.php?action=download&id=123&authkey=mySecret&torrent_pass=mySecret""#,
            // Plex
            r#" http://localhost:32400/library/metadata/12345/refresh?X-Plex-Client-Identifier=1234530f-422f-4aac-b6b3-01233210aaaa&X-Plex-Product=Sonarr&X-Plex-Platform=Windows&X-Plex-Platform-Version=7&X-Plex-Device-Name=Sonarr&X-Plex-Version=3.0.3.833&X-Plex-Token=mySecret"#,
            // Internal
            r#"OutputPath=/home/mySecret/Downloads"#,
            r#"OutputPath=/Users/mySecret/Downloads"#,
            r#"Hardlinking episode file: /home/mySecret/Downloads to /media/abc.mkv"#,
            r#"Hardlinking episode file: /Users/mySecret/Downloads to /media/abc.mkv"#,
            r#"Hardlink '/home/mySecret/Downloads/abs.mkv' to '/media/abc.mkv' failed."#,
            r#"Hardlink '/Users/mySecret/Downloads/abs.mkv' to '/media/abc.mkv' failed."#,
            r#"/sonarr/signalr/messages/negotiate?access_token=1234530f422f4aacb6b301233210aaaa&negotiateVersion=1"#,
            r#"[Info] MigrationController: *** Migrating Database=sonarr-main;Host=postgres14;Username=mySecret;Password=mySecret;Port=5432;Enlist=False ***"#,
            // Announce URLs (passkeys), magnet and tracker
            r#"magnet_uri":"magnet:?xt=urn:btih:9pr04sgkillroyimaveql2tyu8xyui&dn=&tr=https%3a%2f%2fxxx.yyy%2f9pr04sg601233210IMAveQL2tyu8xyui%2fannounce"}"#,
            r#"magnet_uri":"magnet:?xt=urn:btih:9pr04sgkillroyimaveql2tyu8xyui&dn=&tr=https%3a%2f%2fxxx.yyy%2ftracker.php%2f9pr04sg601233210IMAveQL2tyu8xyui%2fannounce"}"#,
            r#"magnet_uri":"magnet:?xt=urn:btih:9pr04sgkillroyimaveql2tyu8xyui&dn=&tr=https%3a%2f%2fxxx.yyy%2fannounce%2f9pr04sg601233210IMAveQL2tyu8xyui"}"#,
            r#"magnet_uri":"magnet:?xt=urn:btih:9pr04sgkillroyimaveql2tyu8xyui&dn=&tr=https%3a%2f%2fxxx.yyy%2fannounce.php%3fpasskey%3d9pr04sg601233210IMAveQL2tyu8xyui"}"#,
            r#"tracker":"https://xxx.yyy/9pr04sg601233210IMAveQL2tyu8xyui/announce"}"#,
            r#"tracker":"https://xxx.yyy/tracker.php/9pr04sg601233210IMAveQL2tyu8xyui/announce"}"#,
            r#"tracker":"https://xxx.yyy/announce/9pr04sg601233210IMAveQL2tyu8xyui"}"#,
            r#"tracker":"https://xxx.yyy/announce.php?passkey=9pr04sg601233210IMAveQL2tyu8xyui"}"#,
            r#"tracker":"http://xxx.yyy/announce.php?passkey=9pr04sg601233210IMAveQL2tyu8xyui","info":"http://xxx.yyy/info?a=b""#,
            // Notifiarr
            r#"https://xxx.yyy/api/v1/notification/sonarr/9pr04sg6-0123-3210-imav-eql2tyu8xyui"#,
            // Discord
            r#"https://discord.com/api/webhooks/mySecret"#,
            r#"https://discord.com/api/webhooks/mySecret/01233210"#,
            // Telegram
            r#"https://api.telegram.org/bot1234567890:mySecret/sendmessage: chat_id=123456&parse_mode=HTML&text=<text>"#,
            r#"https://api.telegram.org/bot1234567890:mySecret/"#,
        ];
        for case in cases {
            let cleansed = cleanse(case);
            for secret in ["mySecret", "123%@%_@!#^#@", "01233210"] {
                assert!(!cleansed.contains(secret), "{case}\n-> {cleansed}");
            }
        }
    }

    /// `should_keep_message`.
    #[test]
    fn what_only_looks_like_a_secret_stays() {
        let case = "[Info] MigrationController: *** Migrating Database=sonarr-main;Host=postgres14;Username=mySecret;Password=mySecret;Port=5432;token=mySecret;Enlist=False&username=mySecret;mypassword=mySecret;mypass=shouldkeep1;test_token=mySecret;password=123%@%_@!#^#@;use_password=mySecret;get_token=shouldkeep2;usetoken=shouldkeep3;passwrd=mySecret;";
        let cleansed = cleanse(case);
        for secret in ["mySecret", "123%@%_@!#^#@", "01233210"] {
            assert!(!cleansed.contains(secret), "{cleansed}");
        }
        for kept in ["shouldkeep1", "shouldkeep2", "shouldkeep3"] {
            assert!(cleansed.contains(kept), "{cleansed}");
        }
        // Nothing to remove: the same text, not a copy.
        let plain = "RSS Sync Completed. Reports found: 869, Reports grabbed: 1";
        assert!(matches!(cleanse(plain), Cow::Borrowed(_)));
        assert_eq!(cleanse(plain), plain);
    }

    /// `should_clean_ipaddress` and `should_not_clean_ipaddress`.
    #[test]
    fn remote_addresses_lose_their_middle() {
        for case in [
            "Some message (from 32.2.3.5 user agent)",
            "Auth-Invalidated ip 32.2.3.5",
            "Auth-Success ip 32.2.3.5",
            "Auth-Logout ip 32.2.3.5",
        ] {
            let cleansed = cleanse(case);
            assert!(!cleansed.contains(".2.3."), "{cleansed}");
            assert!(cleansed.contains("32.*.*.5"), "{cleansed}");
        }
        for case in [
            "Some message (from 10.2.3.2 user agent)",
            "Auth-Unauthorized ip 32.2.3.5",
            "Auth-Failure ip 32.2.3.5",
        ] {
            assert_eq!(cleanse(case), case);
        }
    }

    /// What the trial on a NAS showed: a failed request to Plex logged its
    /// address, token included, as the error under the line.
    #[test]
    fn a_secret_ending_a_line_of_a_longer_text_goes() {
        let error = "Unable to connect to Plex Media Server, error sending request for url (http://plex.test:32400/identity?X-Plex-Client-Identifier=abc-def&X-Plex-Product=Sonarr&X-Plex-Token=mySecret)\nCaused by: connection refused";
        let cleansed = cleanse(error);
        assert!(!cleansed.contains("mySecret"), "{cleansed}");
        assert!(!cleansed.contains("abc-def"), "{cleansed}");
        assert!(cleansed.contains("X-Plex-Product=Sonarr"), "{cleansed}");
        assert!(cleansed.ends_with("Caused by: connection refused"), "{cleansed}");
    }

    /// What it costs: an RSS sync at debug logs tens of thousands of lines.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn cost_per_line() {
        let line = "2026-11-11T07:13:25Z DEBUG unarr_core::decision::engine: release rejected title=NCIS.S24E05.Speechless.2160p.AMZN.WEB-DL.DDP5.1.H.265-NTb reasons=[Permanent] Unknown Series";
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            std::hint::black_box(cleanse(line));
        }
        eprintln!("{:?} per line", start.elapsed() / 10_000);
    }

    #[test]
    fn the_console_writer_cleanses_each_line() {
        use std::io::Write;
        let mut out = Cleansing(Vec::new());
        out.write_all(
            b"WARN indexer request failed url=http://indexer.test/api?t=tvsearch&apikey=mySecret&q=x\n",
        )
        .unwrap();
        let text = String::from_utf8(out.0).unwrap();
        assert_eq!(
            text,
            "WARN indexer request failed url=http://indexer.test/api?t=tvsearch&apikey=(removed)&q=x\n"
        );
    }
}
