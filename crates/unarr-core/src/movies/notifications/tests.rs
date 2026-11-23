//! What the recordings of the real Radarr do not show: the `SupportsOn*`
//! of each implementation, which connection an event reaches (its flag,
//! its tags against the movie's), the Plex path mapping when a section
//! matches, and the payloads no recorded event produced.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::*;
use crate::db::Db;
use crate::model::{ProviderKind, RootFolder};
use crate::movies::decision::testing::movie;
use crate::movies::import::events::{MovieFileDeleted, MovieImported};
use crate::movies::import::local::LocalMovie;
use crate::notifications::plex::{section_updates, Section, Settings as PlexSettings};
use crate::notifications::Identity;
use crate::store;

fn now() -> DateTime<Utc> {
    "2026-11-12T09:00:00Z".parse().unwrap()
}

fn file(id: i64, movie_id: i64) -> MovieFile {
    MovieFile {
        id,
        movie_id,
        relative_path: "Candyman (1992) [Bluray-1080p].mkv".into(),
        original_file_path: None,
        size: 1106640,
        date_added: now(),
        scene_name: Some("Candyman.1992.1080p.BluRay.x264-GRP".into()),
        release_group: Some("GRP".into()),
        edition: None,
        quality: QualityModel {
            quality_id: 7,
            version: 1,
            real: 0,
            is_repack: false,
        },
        languages: vec![1],
        media_info: None,
        indexer_flags: 1,
    }
}

/// A Plex connection at `host` with the flags of `attributes` set.
fn plex(id: i64, host: &str, tags: &[i64], attributes: Value) -> Provider {
    Provider {
        kind: ProviderKind::Notification,
        id,
        name: format!("Plex {id}"),
        implementation: "PlexServer".into(),
        config_contract: "PlexServerSettings".into(),
        settings: json!({"host": host, "port": 32400, "updateLibrary": true}),
        tags: tags.to_vec(),
        attributes,
    }
}

/// A notifier over a Radarr database holding `providers`.
fn notifier(providers: Vec<Provider>) -> MovieNotifier {
    let db = Db::open_in_memory_for(crate::Personality::Radarr).unwrap();
    db.call_blocking(move |conn| {
        for p in &providers {
            store::providers::insert_provider(conn, p)?;
        }
        Ok(())
    })
    .unwrap();
    let base = Notifier::new(db, Identity::default(), now());
    // plex.tv is pinged before each library update: here a port that
    // refuses, never the real host.
    base.set_upstreams(crate::metadata::Upstreams::mock("http://127.0.0.1:9"));
    MovieNotifier::new(Arc::new(base))
}

fn imported(m: &Movie, old_files: Vec<DeletedMovieFile>) -> MovieImportEvent {
    MovieImportEvent::Imported(Box::new(MovieImported {
        local: LocalMovie {
            movie: Some(m.clone()),
            ..LocalMovie::default()
        },
        file: file(2, m.id),
        old_files,
        new_download: true,
        client: None,
        download_id: None,
    }))
}

fn deleted(m: &Movie, reason: DeleteReason) -> MovieImportEvent {
    MovieImportEvent::FileDeleted(Box::new(MovieFileDeleted {
        file: file(1, m.id),
        movie: m.clone(),
        reason,
    }))
}

fn queued(n: &MovieNotifier, host: &str) -> Vec<i64> {
    n.plex_pending(host).iter().map(|m| m.id).collect()
}

#[test]
fn each_implementation_supports_what_the_original_declares() {
    let all = Supports {
        on_grab: true,
        on_download: true,
        on_upgrade: true,
        on_rename: true,
        on_movie_added: true,
        on_movie_delete: true,
        on_movie_file_delete: true,
        on_movie_file_delete_for_upgrade: true,
        on_health_issue: true,
        on_health_restored: true,
        on_application_update: true,
        on_manual_interaction_required: true,
    };
    assert_eq!(Supports::of(Kind::Discord), all);
    assert_eq!(Supports::of(Kind::Webhook), all);
    let plex = Supports::of(Kind::PlexServer);
    assert!(plex.on_download && plex.on_upgrade);
    assert!(plex.on_rename);
    assert!(plex.on_movie_delete);
    assert!(plex.on_movie_file_delete && plex.on_movie_file_delete_for_upgrade);
    assert!(!plex.on_grab);
    assert!(!plex.on_movie_added);
    assert!(!plex.on_health_issue && !plex.on_health_restored);
    assert!(!plex.on_application_update);
    assert!(!plex.on_manual_interaction_required);
    // By the names the resource gives them.
    assert_eq!(plex.get("supportsOnGrab"), Some(false));
    assert_eq!(plex.get("supportsOnMovieFileDeleteForUpgrade"), Some(true));
    assert_eq!(plex.get("onGrab"), None);
    // An implementation Unarr does not drive supports nothing here.
    let other = MovieNotificationDefinition::from_provider(&Provider {
        implementation: "Telegram".into(),
        ..plex_provider_without_flags()
    });
    assert_eq!(other.supports(), Supports::default());
}

fn plex_provider_without_flags() -> Provider {
    plex(1, "plex.test", &[], json!({}))
}

#[test]
fn a_definition_is_enabled_by_an_event_of_its_own() {
    let def = |attributes: Value| {
        MovieNotificationDefinition::from_provider(&plex(1, "plex.test", &[], attributes)).enabled()
    };
    assert!(!def(json!({})));
    for flag in [
        "onGrab",
        "onDownload",
        "onRename",
        "onMovieAdded",
        "onMovieDelete",
        "onMovieFileDelete",
        "onHealthIssue",
        "onHealthRestored",
        "onApplicationUpdate",
        "onManualInteractionRequired",
    ] {
        assert!(def(json!({flag: true})), "{flag}");
    }
    // The flags that refine another one do not enable alone, nor does
    // asking for warnings.
    for flag in [
        "onUpgrade",
        "onMovieFileDeleteForUpgrade",
        "includeHealthWarnings",
    ] {
        assert!(!def(json!({flag: true})), "{flag}");
    }
    // Sonarr's names mean nothing to Radarr's definition.
    assert!(!def(json!({"onSeriesAdd": true, "onImportComplete": true})));
}

#[test]
fn tags_decide_which_movies_a_connection_hears_about() {
    let def =
        |tags: &[i64]| MovieNotificationDefinition::from_provider(&plex(1, "plex.test", tags, json!({})));
    let mut tagged = movie(1, "Candyman", 1992);
    tagged.tags = vec![3, 5];
    let untagged = movie(2, "Lamb", 2021);
    assert!(def(&[]).handles(&tagged));
    assert!(def(&[]).handles(&untagged));
    assert!(def(&[5]).handles(&tagged));
    assert!(def(&[7, 3]).handles(&tagged));
    assert!(!def(&[7]).handles(&tagged));
    assert!(!def(&[5]).handles(&untagged));
}

#[tokio::test]
async fn an_event_reaches_the_connections_its_flag_and_tags_allow() {
    let on = json!({"onDownload": true, "onUpgrade": true, "onMovieFileDelete": true});
    let n = notifier(vec![
        plex(1, "all.test", &[], on.clone()),
        plex(2, "tagged.test", &[5], on.clone()),
        plex(3, "other-tag.test", &[7], on.clone()),
        // Enabled, but not for imports.
        plex(4, "renames.test", &[], json!({"onRename": true})),
        // No event at all: not among the available providers.
        plex(5, "off.test", &[], json!({})),
    ]);
    let mut m = movie(1, "Candyman", 1992);
    m.tags = vec![5];
    n.on_import_events(&[imported(&m, Vec::new())], now()).await;
    assert_eq!(queued(&n, "all.test"), [1]);
    assert_eq!(queued(&n, "tagged.test"), [1]);
    assert!(queued(&n, "other-tag.test").is_empty());
    assert!(queued(&n, "renames.test").is_empty());
    assert!(queued(&n, "off.test").is_empty());
    // A file found by a scan is not a download: nobody is told.
    let other = movie(2, "Lamb", 2021);
    let MovieImportEvent::Imported(mut scanned) = imported(&other, Vec::new()) else {
        unreachable!()
    };
    scanned.new_download = false;
    n.on_import_events(&[MovieImportEvent::Imported(scanned)], now())
        .await;
    assert_eq!(queued(&n, "all.test"), [1]);
    // A movie waits once, however many events name it.
    n.on_import_events(
        &[deleted(&m, DeleteReason::Manual), imported(&m, Vec::new())],
        now(),
    )
    .await;
    assert_eq!(queued(&n, "all.test"), [1]);
}

#[tokio::test]
async fn upgrades_need_their_own_flags() {
    let n = notifier(vec![
        plex(
            1,
            "upgrades.test",
            &[],
            json!({
                "onDownload": true, "onUpgrade": true,
                "onMovieFileDelete": true, "onMovieFileDeleteForUpgrade": true,
            }),
        ),
        plex(
            2,
            "first-only.test",
            &[],
            json!({"onDownload": true, "onMovieFileDelete": true}),
        ),
    ]);
    let m = movie(1, "Candyman", 1992);
    let replaced = vec![DeletedMovieFile {
        file: file(1, 1),
        recycle_bin_path: None,
    }];
    n.on_import_events(&[imported(&m, replaced)], now()).await;
    assert_eq!(queued(&n, "upgrades.test"), [1]);
    assert!(queued(&n, "first-only.test").is_empty());

    let other = movie(2, "Lamb", 2021);
    n.on_import_events(&[deleted(&other, DeleteReason::Upgrade)], now())
        .await;
    assert_eq!(queued(&n, "upgrades.test"), [1, 2]);
    assert!(queued(&n, "first-only.test").is_empty());
    // Any other reason needs only the first flag.
    n.on_import_events(&[deleted(&other, DeleteReason::MissingFromDisk)], now())
        .await;
    assert_eq!(queued(&n, "first-only.test"), [2]);
}

#[tokio::test]
async fn a_connection_that_does_not_update_the_library_queues_nothing() {
    let mut p = plex(1, "plex.test", &[], json!({"onDownload": true}));
    p.settings["updateLibrary"] = json!(false);
    let n = notifier(vec![p]);
    n.on_import_events(&[imported(&movie(1, "Candyman", 1992), Vec::new())], now())
        .await;
    assert!(queued(&n, "plex.test").is_empty());
}

fn section(id: i64, locations: &[&str]) -> Section {
    Section {
        id,
        locations: locations.iter().map(|l| l.to_string()).collect(),
    }
}

fn mapping(from: &str, to: &str) -> PlexSettings {
    PlexSettings::from_value(&json!({"host": "plex.test", "mapFrom": from, "mapTo": to}))
}

#[test]
fn the_section_at_the_mapped_root_is_the_one_refreshed() {
    // Radarr sees the library at /data/media/movies, Plex at /media/movies.
    let roots = [RootFolder {
        id: 1,
        path: "/data/media/movies".into(),
    }];
    let sections = [
        section(4, &["/media/kids"]),
        section(1, &["/media/movies-4k", "/media/movies"]),
    ];
    let settings = mapping("/data/media/movies", "/media/movies");
    assert_eq!(
        section_updates(
            &settings,
            "/data/media/movies/Candyman (1992)",
            &sections,
            &roots,
            "Movie"
        ),
        [(1, "/media/movies/Candyman (1992)".to_string())]
    );
    // Trailing separators on either side of the mapping change nothing.
    assert_eq!(
        section_updates(
            &mapping("/data/media/movies/", "/media/movies/"),
            "/data/media/movies/Candyman (1992)",
            &sections,
            &roots,
            "Movie"
        ),
        [(1, "/media/movies/Candyman (1992)".to_string())]
    );
    // A mapping from a parent folder carries the rest of the root along.
    assert_eq!(
        section_updates(
            &mapping("/data/media", "/media"),
            "/data/media/movies/Candyman (1992)",
            &sections,
            &roots,
            "Movie"
        ),
        [(1, "/media/movies/Candyman (1992)".to_string())]
    );
    // Without a mapping the root folder itself is looked for.
    assert_eq!(
        section_updates(
            &mapping("", ""),
            "/data/media/movies/Candyman (1992)",
            &[section(2, &["/data/media/movies"])],
            &roots,
            "Movie"
        ),
        [(2, "/data/media/movies/Candyman (1992)".to_string())]
    );
}

#[test]
fn every_location_is_refreshed_when_none_is_the_mapped_root() {
    let roots = [RootFolder {
        id: 1,
        path: "/data/media/movies".into(),
    }];
    let sections = [
        section(1, &["/elsewhere/movies"]),
        section(3, &["D:\\Films", "/mnt/films"]),
    ];
    assert_eq!(
        section_updates(
            &mapping("/data/media/movies", "/media/movies"),
            "/data/media/movies/Candyman (1992)",
            &sections,
            &roots,
            "Movie"
        ),
        [
            (1, "/elsewhere/movies/Candyman (1992)".to_string()),
            (3, "D:\\Films\\Candyman (1992)".to_string()),
            (3, "/mnt/films/Candyman (1992)".to_string()),
        ]
    );
}

#[test]
fn covers_get_the_address_they_are_served_at() {
    let mut images = vec![
        Image {
            cover_type: "poster".into(),
            remote_url: Some("https://image.tmdb.org/t/p/original/jQtgkgDZE7egMq532sOt83DnT83.jpg".into()),
            url: None,
        },
        Image {
            cover_type: "clearlogo".into(),
            remote_url: None,
            url: None,
        },
        Image {
            cover_type: "unknown".into(),
            remote_url: Some("https://image.tmdb.org/x.jpg".into()),
            url: None,
        },
    ];
    convert_to_local_urls("/radarr", 20, &mut images, &|_| true);
    // The hash is the one the real Radarr sent for this cover.
    assert_eq!(
        images[0].url.as_deref(),
        Some("/radarr/MediaCover/20/poster.jpg?h=a75b3e969b8fb4de9865")
    );
    assert_eq!(
        images[1].url.as_deref(),
        Some("/radarr/MediaCover/20/clearlogo.png")
    );
    assert_eq!(images[2].url, None);
}

fn context<'a>(identity: &'a Identity, tags: &'a [Tag]) -> Context<'a> {
    Context {
        identity,
        application_url: "http://radarr.test",
        tags,
        now: now(),
    }
}

#[test]
fn a_grab_is_told_to_the_webhook_as_the_original_builds_it() {
    let identity = Identity {
        instance_name: "Radarr".into(),
        ..Identity::default()
    };
    let tags = [
        Tag {
            id: 5,
            label: "ita".into(),
        },
        Tag {
            id: 6,
            label: "4k".into(),
        },
    ];
    let mut m = movie(1, "Candyman", 1992);
    m.tags = vec![5];
    m.metadata.in_cinemas = Some("1992-10-16T00:00:00Z".parse().unwrap());
    let mut remote = RemoteMovie::new(crate::indexers::rss::ReleaseInfo {
        title: "Candyman.1992.1080p.BluRay.x264-GRP".into(),
        indexer: "Indexer".into(),
        size: 7_979_321_856,
        indexer_flags: 1 | 32,
        ..Default::default()
    });
    remote.languages = vec![1, 5];
    remote.custom_format_score = 10;
    remote.custom_formats = vec![CustomFormat {
        id: 54,
        name: "DD".into(),
        include_when_renaming: false,
        specifications: json!([]),
    }];
    let message = GrabMessage {
        movie: &m,
        remote: &remote,
        quality: QualityModel {
            quality_id: 7,
            version: 2,
            real: 0,
            is_repack: false,
        },
        download_client_name: "Mock SABnzbd",
        download_client_type: "SABnzbd",
        download_id: Some("SABnzbd_nzo_1"),
    };
    let payload = webhook::grab_payload(&message, &context(&identity, &tags));
    assert_eq!(
        payload,
        json!({
            "eventType": "Grab",
            "instanceName": "Radarr",
            "applicationUrl": "http://radarr.test",
            "movie": {
                "id": 1,
                "title": "Candyman",
                "year": 1992,
                // Ninety days after the cinemas, without a physical release.
                "releaseDate": "1993-01-14",
                "folderPath": "/movies/Candyman (1992)",
                "tmdbId": 1001,
                "imdbId": "tt0001001",
                "genres": [],
                "images": [],
                "tags": ["ita"],
                "originalLanguage": {"id": 1, "name": "English"},
            },
            "remoteMovie": {"tmdbId": 1001, "imdbId": "tt0001001", "title": "Candyman", "year": 1992},
            "release": {
                "quality": "Bluray-1080p",
                "qualityVersion": 2,
                "releaseTitle": "Candyman.1992.1080p.BluRay.x264-GRP",
                "indexer": "Indexer",
                "size": 7_979_321_856_i64,
                "customFormatScore": 10,
                "customFormats": ["DD"],
                "languages": [{"id": 1, "name": "English"}, {"id": 5, "name": "Italian"}],
                "indexerFlags": ["G_Freeleech", "G_Internal"],
            },
            "downloadClient": "Mock SABnzbd",
            "downloadClientType": "SABnzbd",
            "downloadId": "SABnzbd_nzo_1",
            "customFormatInfo": {"customFormats": [{"id": 54, "name": "DD"}], "customFormatScore": 10},
        })
    );
}

#[test]
fn a_movie_without_any_release_date_is_released_at_the_end_of_time() {
    let identity = Identity::default();
    let m = movie(1, "Lamb", 2021);
    let message = MovieFileDeleteMessage {
        movie: &m,
        file: &file(1, 1),
        reason: DeleteReason::Manual,
    };
    let payload = webhook::file_delete_payload(&message, &context(&identity, &[]));
    assert_eq!(payload["movie"]["releaseDate"], "9999-12-31");
    assert_eq!(payload["deleteReason"], "manual");
    assert_eq!(payload["movieFile"]["indexerFlags"], "G_Freeleech");
    assert_eq!(
        payload["movieFile"]["path"],
        "/movies/Lamb (2021)/Candyman (1992) [Bluray-1080p].mkv"
    );
    assert_eq!(payload["movieFile"].get("mediaInfo"), None);
}

#[test]
fn discord_titles_escape_backticks_and_stop_at_256_characters() {
    let mut m = movie(1, "The `Burbs", 1989);
    assert_eq!(discord::title(&m), "The \\`Burbs (1989)");
    m.metadata.year = 0;
    assert_eq!(discord::title(&m), "The \\`Burbs");
    m.metadata.title = "a".repeat(300);
    let cut = discord::title(&m);
    assert_eq!(cut.chars().count(), 256);
    assert!(cut.ends_with("aaa..."));
    // A backslash the cut would leave dangling goes too.
    m.metadata.title = format!("{}`{}", "a".repeat(252), "b".repeat(10));
    assert_eq!(discord::title(&m), format!("{}...", "a".repeat(252)));
}

#[test]
fn discord_links_follow_what_the_metadata_has() {
    let mut m = movie(1, "Candyman", 1992);
    assert_eq!(
        discord::links(&m),
        "[TMDb](https://themoviedb.org/movie/1001) / [Trakt](https://trakt.tv/movies/tt0001001) / \
         [IMDb](https://imdb.com/title/tt0001001/)"
    );
    m.metadata.imdb_id = None;
    m.metadata.youtube_trailer_id = Some("CeLZMhkFTuU".into());
    m.metadata.website = Some("https://example.test/candyman".into());
    assert_eq!(
        discord::links(&m),
        "[TMDb](https://themoviedb.org/movie/1001) / [YouTube](https://www.youtube.com/watch?v=CeLZMhkFTuU) / \
         [Website](https://example.test/candyman)"
    );
}

#[test]
fn the_grab_embed_lists_the_fields_the_settings_name() {
    let identity = Identity {
        instance_name: "Radarr".into(),
        ..Identity::default()
    };
    let tags = [Tag {
        id: 5,
        label: "ita".into(),
    }];
    let mut m = movie(1, "Candyman", 1992);
    m.tags = vec![5];
    m.metadata.ratings = json!({"tmdb": {"votes": 10, "value": 6.7, "type": "user"}});
    let remote = RemoteMovie::new(crate::indexers::rss::ReleaseInfo {
        title: "Candyman.1992.1080p.BluRay.x264-GRP".into(),
        indexer: "Indexer".into(),
        size: 7_979_321_856,
        ..Default::default()
    });
    let message = GrabMessage {
        movie: &m,
        remote: &remote,
        quality: QualityModel {
            quality_id: 7,
            version: 1,
            real: 0,
            is_repack: false,
        },
        download_client_name: "Mock SABnzbd",
        download_client_type: "SABnzbd",
        download_id: None,
    };
    // Rating, quality, size, custom formats (none: left out), tags; no
    // poster asked for, so no thumbnail.
    let settings = crate::notifications::discord::Settings::from_value(&json!({
        "webHookUrl": "http://discord.test/hook",
        "grabFields": [1, 3, 5, 11, 13],
    }));
    let payload = discord::grab_payload(&settings, &message, &context(&identity, &tags));
    let embed = &payload["embeds"][0];
    assert_eq!(embed["author"]["name"], "Radarr");
    assert_eq!(embed["description"], "Movie Grabbed");
    assert_eq!(embed["title"], "Candyman (1992)");
    assert_eq!(embed["url"], "https://www.themoviedb.org/movie/1001");
    assert_eq!(embed["color"], 16761392);
    assert_eq!(embed.get("thumbnail"), None);
    assert_eq!(
        embed["fields"],
        json!([
            {"name": "Rating", "value": "6.7", "inline": false},
            {"name": "Quality", "value": "Bluray-1080p", "inline": true},
            {"name": "Size", "value": "7.4 GB", "inline": true},
            {"name": "Tags", "value": "ita", "inline": false},
        ])
    );
    // The author the settings name wins over the instance's name.
    let named = crate::notifications::discord::Settings::from_value(&json!({
        "webHookUrl": "http://discord.test/hook", "author": "Films", "grabFields": [],
    }));
    let payload = discord::grab_payload(&named, &message, &context(&identity, &tags));
    assert_eq!(payload["embeds"][0]["author"]["name"], "Films");
}

/// A webhook connection posting to `url` with the flags of `attributes`.
fn webhook_provider(id: i64, url: &str, attributes: Value) -> Provider {
    Provider {
        kind: ProviderKind::Notification,
        id,
        name: format!("Webhook {id}"),
        implementation: "Webhook".into(),
        config_contract: "WebhookSettings".into(),
        settings: json!({"url": url, "method": 1}),
        tags: Vec::new(),
        attributes,
    }
}

/// A local receiver of webhooks: every body posted, by path, in order. A
/// payload whose movie is titled `refused` gets a 500.
async fn receiver(refused: &'static str) -> (String, Arc<std::sync::Mutex<Vec<(String, Value)>>>) {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    type Seen = Arc<std::sync::Mutex<Vec<(String, Value)>>>;
    async fn hook(
        State((seen, refused)): State<(Seen, &'static str)>,
        Path(path): Path<String>,
        axum::Json(body): axum::Json<Value>,
    ) -> StatusCode {
        let refuse = body["movie"]["title"] == refused;
        seen.lock().unwrap().push((path, body));
        if refuse {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::OK
        }
    }
    let seen: Seen = Arc::default();
    let app = axum::Router::new()
        .route("/{*path}", axum::routing::post(hook))
        .with_state((seen.clone(), refused));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn added(id: i64, title: &str, year: i64, method: &str) -> Movie {
    let mut m = movie(id, title, year);
    m.add_options = Some(crate::movies::library::add_options(None, method));
    m
}

#[tokio::test]
async fn a_list_of_added_movies_stops_a_connection_at_its_first_failure() {
    let (url, seen) = receiver("Candyman").await;
    let n = notifier(vec![
        webhook_provider(1, &format!("{url}/one"), json!({"onMovieAdded": true})),
        webhook_provider(2, &format!("{url}/two"), json!({"onMovieAdded": true})),
        // Not for additions.
        webhook_provider(3, &format!("{url}/three"), json!({"onMovieDelete": true})),
        // Plex has nothing to do for an addition.
        plex(4, "plex.test", &[], json!({"onMovieAdded": true})),
    ]);
    let movies = [added(1, "Candyman", 1992, "list"), added(2, "Lamb", 2021, "list")];
    n.on_movies_added(&movies, now()).await;
    let seen = seen.lock().unwrap().clone();
    let titles = |path: &str| -> Vec<String> {
        seen.iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| b["movie"]["title"].as_str().unwrap().to_string())
            .collect()
    };
    // `MoviesImportedEvent` wraps each connection's list in one `try`:
    // every connection meets the refused movie first, and stops there.
    assert_eq!(titles("one"), ["Candyman"]);
    assert_eq!(titles("two"), ["Candyman"]);
    assert!(titles("three").is_empty());
    assert!(queued(&n, "plex.test").is_empty());
    assert!(seen
        .iter()
        .all(|(_, b)| b["eventType"] == "MovieAdded" && b["addMethod"] == "list"));

    // A movie the others do not refuse goes through to both.
    let (url, seen) = receiver("nothing").await;
    let n = notifier(vec![
        webhook_provider(1, &format!("{url}/one"), json!({"onMovieAdded": true})),
        webhook_provider(2, &format!("{url}/two"), json!({"onMovieAdded": true})),
    ]);
    n.on_movies_added(&movies, now()).await;
    assert_eq!(seen.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_deleted_movie_reaches_every_connection_and_plex_only_with_its_files() {
    let (url, seen) = receiver("nothing").await;
    let on = json!({"onMovieDelete": true});
    let n = notifier(vec![
        webhook_provider(1, &format!("{url}/hook"), on.clone()),
        plex(2, "plex.test", &[], on.clone()),
        plex(3, "tagged.test", &[7], on.clone()),
        // Enabled, but for file deletions only.
        plex(4, "files.test", &[], json!({"onMovieFileDelete": true})),
    ]);
    let kept = movie(1, "Candyman", 1992);
    n.on_movies_deleted(std::slice::from_ref(&kept), &HashMap::new(), false, now())
        .await;
    // The files stayed: Plex has nothing to refresh.
    assert!(queued(&n, "plex.test").is_empty());
    let gone = [movie(2, "Lamb", 2021), movie(3, "Pearl", 2022)];
    let sizes = HashMap::from([(2, 7_979_321_856_i64)]);
    n.on_movies_deleted(&gone, &sizes, true, now()).await;
    assert_eq!(queued(&n, "plex.test"), [2, 3]);
    assert!(queued(&n, "tagged.test").is_empty());
    assert!(queued(&n, "files.test").is_empty());
    let seen = seen.lock().unwrap().clone();
    let told: Vec<(String, bool, i64)> = seen
        .iter()
        .map(|(_, b)| {
            (
                b["movie"]["title"].as_str().unwrap().to_string(),
                b["deletedFiles"].as_bool().unwrap(),
                b["movieFolderSize"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        told,
        [
            ("Candyman".to_string(), false, 0),
            ("Lamb".to_string(), true, 7_979_321_856),
            // No file: nothing to measure.
            ("Pearl".to_string(), true, 0),
        ]
    );
}

#[test]
fn the_library_payloads_are_the_originals() {
    let identity = Identity {
        instance_name: "Radarr".into(),
        ..Identity::default()
    };
    let tags = [Tag {
        id: 5,
        label: "ita".into(),
    }];
    let mut m = added(1, "The `Burbs", 1989, "Collection");
    m.tags = vec![5];
    m.metadata.images = vec![Image {
        cover_type: "poster".into(),
        remote_url: Some("https://image.tmdb.org/t/p/original/poster.jpg".into()),
        url: None,
    }];
    let ctx = context(&identity, &tags);
    // The cover's local address, hashed as `covers_get_the_address_they_are_served_at` checks.
    let local = {
        let mut images = m.metadata.images.clone();
        convert_to_local_urls("", 1, &mut images, &|_| true);
        images[0].url.clone().unwrap()
    };
    let expected_movie = json!({
        "id": 1,
        "title": "The `Burbs",
        "year": 1989,
        "releaseDate": "9999-12-31",
        "folderPath": "/movies/The `Burbs (1989)",
        "tmdbId": 1001,
        "imdbId": "tt0001001",
        "genres": [],
        "images": [{
            "coverType": "poster",
            "url": local,
            "remoteUrl": "https://image.tmdb.org/t/p/original/poster.jpg",
        }],
        "tags": ["ita"],
        "originalLanguage": {"id": 1, "name": "English"},
    });

    // `BuildOnMovieAdded`: the add method as the enum serialises.
    assert_eq!(
        webhook::movie_added_payload(&m, &ctx).unwrap(),
        json!({
            "eventType": "MovieAdded",
            "instanceName": "Radarr",
            "applicationUrl": "http://radarr.test",
            "movie": expected_movie,
            "addMethod": "collection",
        })
    );
    // Without options the original dereferences a null.
    let mut bare = m.clone();
    bare.add_options = None;
    assert!(webhook::movie_added_payload(&bare, &ctx).is_err());

    // `BuildOnMovieDelete`: the size only when the files went.
    let message = MovieDeleteMessage {
        movie: &m,
        deleted_files: true,
        file_size: Some(1106640),
    };
    assert_eq!(
        webhook::movie_delete_payload(&message, &ctx),
        json!({
            "eventType": "MovieDelete",
            "instanceName": "Radarr",
            "applicationUrl": "http://radarr.test",
            "movie": expected_movie,
            "deletedFiles": true,
            "movieFolderSize": 1106640,
        })
    );
    let kept = MovieDeleteMessage {
        deleted_files: false,
        ..message
    };
    assert_eq!(webhook::movie_delete_payload(&kept, &ctx)["movieFolderSize"], 0);

    // Discord: the title as it is, the links, the poster the import
    // fields ask for, no timestamp.
    let settings = crate::notifications::discord::Settings::from_value(&json!({
        "webHookUrl": "http://discord.test/hook",
        "importFields": [11],
    }));
    let embed = |payload: Value| payload["embeds"][0].clone();
    assert_eq!(
        embed(discord::movie_added_payload(&settings, &m, &ctx)),
        json!({
            "author": {
                "name": "Radarr",
                "icon_url": "https://raw.githubusercontent.com/Radarr/Radarr/develop/Logo/256.png",
            },
            "url": "https://www.themoviedb.org/movie/1001",
            "title": "The `Burbs",
            "description": "Movie Added",
            "color": 2605644,
            "fields": [{
                "name": "Links",
                "value": "[TMDb](https://themoviedb.org/movie/1001) / [Trakt](https://trakt.tv/movies/tt0001001) / \
                          [IMDb](https://imdb.com/title/tt0001001/)",
                "inline": false,
            }],
            "thumbnail": {"url": "https://image.tmdb.org/t/p/original/poster.jpg"},
        })
    );
    let deleted = embed(discord::movie_delete_payload(&settings, &message, &ctx));
    assert_eq!(deleted["description"], "Movie removed and all files were deleted");
    assert_eq!(deleted["color"], 15749200);
    assert_eq!(deleted["title"], "The `Burbs");
    assert_eq!(deleted.get("timestamp"), None);
    assert_eq!(deleted.get("image"), None);
    let kept_embed = embed(discord::movie_delete_payload(&settings, &kept, &ctx));
    assert_eq!(kept_embed["description"], "Movie removed, files were not deleted");
}

/// `MediaCoverServiceFixture`, Radarr 7: a movie added within the day gets
/// the cache breaker only once its cover is on disk; an older one always.
#[test]
fn a_recent_movie_gets_the_cache_breaker_once_its_cover_exists() {
    use super::cover_exists;
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-23T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let root = std::env::temp_dir().join(format!("unarr-covers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let recent = Some(now - chrono::Duration::hours(2));
    let old = Some(now - chrono::Duration::days(2));
    let convert = |added| {
        let mut images = vec![Image {
            cover_type: "poster".into(),
            remote_url: Some("https://image.tmdb.org/t/p/original/poster.jpg".into()),
            url: None,
        }];
        convert_to_local_urls("", 7, &mut images, &|t| {
            cover_exists(Some(&root), 7, t, added, now)
        });
        images[0].url.clone().unwrap()
    };
    // should_convert_cover_urls_to_local_without_hash_if_cover_has_not_been_downloaded
    assert_eq!(convert(recent), "/MediaCover/7/poster.jpg");
    // should_not_check_if_cover_exists_for_movie_added_more_than_a_day_ago
    assert!(convert(old).starts_with("/MediaCover/7/poster.jpg?h="));
    assert!(convert(None).starts_with("/MediaCover/7/poster.jpg?h="));
    // should_add_hash_to_cover_url_once_cover_exists
    std::fs::create_dir_all(root.join("7")).unwrap();
    std::fs::write(root.join("7").join("poster.jpg"), b"jpg").unwrap();
    assert_eq!(convert(recent), convert(old));
    assert!(
        !cover_exists(None, 7, "poster", recent, now),
        "no folder, no cover"
    );
    std::fs::remove_dir_all(&root).ok();
}
