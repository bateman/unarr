//! Radarr's `ImportDecisionMaker`: every video file of a download or a
//! folder parsed, augmented and run through the specifications.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::aggregate;
use super::local::{LocalMovie, MovieImportDecision, MovieImportRejection, MovieRejectionReason as R};
use super::specs::{self, ImportSources, StoreImportSources};
use crate::customformat::Input;
use crate::decision::upgradable;
use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::decide::Probes;
use crate::mediafiles::sample::DetectSampleResult;
use crate::mediafiles::{file_name, is_parent_path, join, path_equals};
use crate::model::{CustomFormat, ReleaseType};
use crate::movies::decision::{MovieCatalog, MovieLookup, StoreMovieLookup};
use crate::movies::formats::facts;
use crate::movies::model::{Movie, MovieFile, MovieNamingConfig};
use crate::movies::parser::{self, ParsedMovieInfo};
use crate::movies::store;
use crate::store::config::MediaManagementConfig;

pub use super::sample::is_sample;

/// What one import run reads once.
#[derive(Debug, Clone)]
pub struct MovieEnvironment {
    pub catalog: MovieCatalog,
    pub config: MediaManagementConfig,
    pub naming: MovieNamingConfig,
    pub now: DateTime<Utc>,
}

impl MovieEnvironment {
    pub fn load(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<MovieEnvironment> {
        Ok(MovieEnvironment {
            catalog: MovieCatalog::load(conn, now)?,
            config: MediaManagementConfig::load(conn)?,
            naming: store::naming(conn)?,
            now,
        })
    }
}

/// `ParseCustomFormat(LocalMovie, fileName)`: the scene name, else
/// `file_name_for_formats`, simplified, with what the aggregators settled
/// on; the formats that look at a file's name read `file_name_for_formats`.
pub fn local_formats(catalog: &MovieCatalog, local: &LocalMovie, file_name_for_formats: &str) -> Vec<CustomFormat> {
    let Some(movie) = &local.movie else {
        return Vec::new();
    };
    let scene_name = local.scene_name.as_deref().filter(|s| !s.trim().is_empty());
    let title = parser::simplify_release_title(scene_name.unwrap_or(file_name_for_formats));
    let input = Input {
        release_title: &title,
        filename: Some(file_name_for_formats),
        release_group: local.release_group.as_deref(),
        quality: local.quality,
        languages: &local.languages,
        size: local.size,
        indexer_flags: local.indexer_flags,
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&local.quality, local.edition.as_deref(), movie)),
    };
    catalog
        .base
        .formats
        .evaluate(&input)
        .into_iter()
        .cloned()
        .collect()
}

/// `BuildFileName(movie, localMovie.ToMovieFile())`: the name the import
/// would give the file, without folder nor extension. A format the
/// naming cannot build leaves the file's own name.
pub fn renamed_file_name(env: &MovieEnvironment, translations: &[(i64, String)], local: &LocalMovie) -> String {
    let movie = local.movie_ref();
    let file = local.to_movie_file(env.now);
    let formats = crate::movies::formats::file_formats(&env.catalog.base.formats, &file, movie);
    super::approve::build_file_name(env, movie, translations, &file, &local.path, &formats).unwrap_or_else(|e| {
        tracing::debug!("Unable to build the file name of {}: {e}", local.path);
        crate::mediafiles::file_name(&local.path).to_string()
    })
}

/// `UpdateMovieCustomFormats` (Radarr 7): the custom formats of a local
/// movie under the name the import will give it (`renamed`) and under
/// the name it has now, each with its score under the movie's profile.
pub fn score_formats(catalog: &MovieCatalog, local: &mut LocalMovie, renamed: &str) {
    let score = |local: &LocalMovie, formats: &[CustomFormat]| {
        local
            .movie
            .as_ref()
            .and_then(|m| catalog.base.profile(m.quality_profile_id))
            .map(|p| upgradable::format_score(p, formats))
            .unwrap_or(0)
    };
    local.custom_formats = local_formats(catalog, local, renamed);
    local.file_name_used_for_custom_format_calculation = Some(renamed.to_string());
    local.custom_format_score = score(local, &local.custom_formats);
    let own_name = file_name(&local.path).to_string();
    local.original_file_name_custom_formats = local_formats(catalog, local, &own_name);
    local.original_file_name_custom_format_score = score(local, &local.original_file_name_custom_formats);
}

/// [`score_formats`] under the name [`renamed_file_name`] builds, the
/// movie's translations read from `conn`.
pub fn score_formats_renamed(conn: &Connection, env: &MovieEnvironment, local: &mut LocalMovie) -> anyhow::Result<()> {
    if local.movie.is_none() {
        score_formats(&env.catalog, local, "");
        return Ok(());
    }
    let translations = store::translation_titles(conn, local.movie_ref().metadata.id)?;
    let renamed = renamed_file_name(env, &translations, local);
    score_formats(&env.catalog, local, &renamed);
    Ok(())
}

/// `GetImportDecisions(videoFiles, movie, downloadClientItem, folderInfo, sceneSource, filterExistingFiles)`.
/// `probes` holds the media info of each file, read before the database is
/// asked anything; `tracked_flags` are the indexer flags of the release
/// the tracked download of `item` was grabbed as, when there is one.
#[allow(clippy::too_many_arguments)]
pub fn get_import_decisions(
    conn: &Connection,
    env: &MovieEnvironment,
    probes: &Probes,
    video_files: &[String],
    movie: &Movie,
    item: Option<&DownloadClientItem>,
    folder_info: Option<&ParsedMovieInfo>,
    scene_source: bool,
    filter_existing_files: bool,
    tracked_flags: Option<i64>,
) -> anyhow::Result<Vec<MovieImportDecision>> {
    let new_files: Vec<String> = if filter_existing_files {
        let existing = store::movie_files_of(conn, movie.id)?;
        filter_existing(video_files, &existing, movie)
    } else {
        video_files.to_vec()
    };
    tracing::debug!("Analyzing {}/{} files.", new_files.len(), video_files.len());
    let item_info = item.and_then(|i| parser::parse_movie_title(&i.title, false));
    // `GetNonSampleVideoFileCount`: a file that cannot be measured counts.
    let non_sample_count = new_files
        .iter()
        .filter(|f| is_sample(&movie.metadata, f, probes.get(f).as_ref()) != DetectSampleResult::Sample)
        .count();
    let mut decisions = Vec::new();
    for file in &new_files {
        let local = LocalMovie {
            movie: Some(movie.clone()),
            download_client_movie_info: item_info.clone(),
            download_item: item.cloned(),
            folder_movie_info: folder_info.cloned(),
            path: file.clone(),
            scene_source,
            existing_file: is_parent_path(&movie.path, file),
            other_video_files: non_sample_count > 1,
            ..Default::default()
        };
        decisions.push(decide(conn, env, probes, local, item, tracked_flags));
    }
    Ok(decisions)
}

/// `GetDecision(localMovie, downloadClientItem)`: the specifications only,
/// for a local movie the caller filled in (its media info included: the
/// sample check reads the runtime from it).
pub fn specs_only(
    lookup: &dyn MovieLookup,
    sources: &dyn ImportSources,
    env: &MovieEnvironment,
    local: LocalMovie,
    item: Option<&DownloadClientItem>,
) -> MovieImportDecision {
    let ctx = specs::Context {
        lookup,
        sources,
        catalog: &env.catalog,
        config: &env.config,
        now: env.now,
    };
    let rejections = specs::evaluate(&ctx, &local, item);
    MovieImportDecision::new(local, rejections)
}

/// `GetDecision(localMovie, downloadClientItem, otherFiles)`: parse,
/// augment, then the specifications; a parse failure or an error is a
/// rejection, as the original turns its exceptions into one.
fn decide(
    conn: &Connection,
    env: &MovieEnvironment,
    probes: &Probes,
    mut local: LocalMovie,
    item: Option<&DownloadClientItem>,
    tracked_flags: Option<i64>,
) -> MovieImportDecision {
    local.file_movie_info = parser::parse_movie_path(&local.path);
    let decision = match std::fs::metadata(&local.path) {
        // `DiskProvider.GetFileSize` throws for a file that is not there.
        Err(e) => {
            tracing::error!("Couldn't import file. {}: {e}", local.path);
            MovieImportDecision::rejected(
                Some(local),
                MovieImportRejection::new(R::Error, "Unexpected error processing file"),
            )
        }
        Ok(metadata) => {
            local.size = metadata.len() as i64;
            let media_info = if !local.existing_file || env.config.enable_media_info {
                probes.get(&local.path)
            } else {
                None
            };
            match aggregate::augment(conn, &mut local, item, media_info) {
                Err(e) => {
                    tracing::debug!("{}", e.0);
                    MovieImportDecision::rejected(
                        Some(local),
                        MovieImportRejection::new(R::UnableToParse, "Unable to parse file"),
                    )
                }
                Ok(()) if local.movie.is_none() => MovieImportDecision::rejected(
                    Some(local),
                    MovieImportRejection::new(R::InvalidMovie, "Invalid movie"),
                ),
                Ok(()) => {
                    let has_download_id = item.is_some_and(|i| !i.download_id.trim().is_empty());
                    if let (true, Some(flags)) = (has_download_id, tracked_flags) {
                        local.indexer_flags = flags;
                    }
                    if let Err(e) = score_formats_renamed(conn, env, &mut local) {
                        tracing::error!("Couldn't import file. {}: {e:#}", local.path);
                        return MovieImportDecision::rejected(
                            Some(local),
                            MovieImportRejection::new(R::Error, "Unexpected error processing file"),
                        );
                    }
                    let lookup = StoreMovieLookup { conn };
                    let sources = StoreImportSources { conn };
                    specs_only(&lookup, &sources, env, local, item)
                }
            }
        }
    };
    if decision.rejections.is_empty() {
        tracing::debug!("File accepted");
    } else {
        tracing::debug!(
            "File rejected for the following reasons: {}",
            decision
                .rejections
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    decision
}

/// `MediaFileService.FilterExistingFiles`: the paths the library does not
/// hold yet.
pub fn filter_existing(files: &[String], existing: &[MovieFile], movie: &Movie) -> Vec<String> {
    let known: Vec<String> = existing
        .iter()
        .map(|f| join(&movie.path, &f.relative_path))
        .collect();
    if known.is_empty() {
        return files.to_vec();
    }
    files
        .iter()
        .filter(|f| !known.iter().any(|k| path_equals(k, f)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    //! The original's `ImportDecisionMakerFixture` swaps the
    //! specifications, the aggregation and the disk for mocks; what it
    //! asserts is checked here against the real ones, over files in a
    //! temporary folder and a library in memory.

    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::db::Db;
    use crate::downloadclients::{ClientKind, ItemClientInfo};
    use crate::mediafiles::mediainfo::{format_timespan, MediaInfo};
    use crate::model::Protocol;
    use crate::movies::decision::testing;
    use crate::movies::parser::language::lang;
    use crate::movies::parser::quality::{BLURAY1080P, DVD};
    use crate::Personality;

    const FILE: &str = "The.Movie.2018.DVDRip.XviD-OSiTV.avi";

    /// A folder of its own for one test, removed when the test ends.
    struct Unsorted(PathBuf);

    impl Unsorted {
        fn new(test: &str) -> Unsorted {
            let dir = std::env::temp_dir().join(format!("unarr-movie-import-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Unsorted(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }

        fn file(&self, name: &str) -> String {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"video").unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for Unsorted {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn movie() -> Movie {
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.path = "/C/Test/Movie".into();
        movie
    }

    /// Media info of a file with sound that runs `seconds`.
    fn probe(seconds: f64) -> Option<MediaInfo> {
        Some(MediaInfo {
            model: json!({
                "runTime": format_timespan(seconds),
                "audioStreamCount": 1,
                "audioLanguages": ["eng"],
                "width": 720,
                "height": 480,
            }),
            title: None,
        })
    }

    fn probes(files: &[(&String, Option<MediaInfo>)]) -> Probes {
        Probes {
            by_path: files
                .iter()
                .map(|(path, info)| ((*path).clone(), info.clone()))
                .collect(),
        }
    }

    fn item(title: &str) -> DownloadClientItem {
        DownloadClientItem::new(
            ItemClientInfo {
                protocol: Protocol::Usenet,
                kind: ClientKind::Sabnzbd,
                id: 1,
                name: "SABnzbd".into(),
                remove_completed_downloads: true,
                has_post_import_category: false,
            },
            "DownloadId1",
            title,
        )
    }

    /// The decisions for `files` of the movie, which the library holds.
    fn decisions(
        files: Vec<String>,
        probes: Probes,
        item: Option<DownloadClientItem>,
        scene_source: bool,
        tracked_flags: Option<i64>,
    ) -> Vec<MovieImportDecision> {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        db.call_blocking(move |conn| {
            let movie = movie();
            store::insert_metadata(conn, &movie.metadata)?;
            store::insert_movie(conn, &movie)?;
            let mut env = MovieEnvironment::load(conn, Utc::now())?;
            env.config.skip_free_space_check_when_importing = true;
            get_import_decisions(
                conn,
                &env,
                &probes,
                &files,
                &movie,
                item.as_ref(),
                None,
                scene_source,
                true,
                tracked_flags,
            )
        })
        .unwrap()
    }

    fn reasons(decision: &MovieImportDecision) -> Vec<R> {
        decision.rejections.iter().map(|r| r.reason).collect()
    }

    #[test]
    fn should_return_approved_if_all_specs_pass() {
        let unsorted = Unsorted::new("approved");
        let file = unsorted.file(FILE);
        let result = decisions(
            vec![file.clone()],
            probes(&[(&file, probe(6000.0))]),
            None,
            true,
            None,
        );
        assert_eq!(result.len(), 1);
        assert!(result[0].approved(), "{:?}", result[0].rejections);
        let local = result[0].local_movie.as_ref().unwrap();
        assert_eq!(local.path, file);
        assert_eq!(local.size, 5);
        assert_eq!(local.quality.quality_id, DVD);
        assert_eq!(local.release_group.as_deref(), Some("OSiTV"));
        assert_eq!(local.languages, vec![lang::ENGLISH]);
        assert_eq!(
            local.scene_name.as_deref(),
            Some("The.Movie.2018.DVDRip.XviD-OSiTV")
        );
        assert!(!local.existing_file);
        assert!(!local.other_video_files);
        assert!(local.media_info.is_some());
        assert_eq!(local.movie_ref().id, 1);
    }

    #[test]
    fn should_return_rejected_if_single_specs_fail() {
        let unsorted = Unsorted::new("single");
        let file = unsorted.file(FILE);
        // Nothing could measure the file.
        let result = decisions(vec![file.clone()], probes(&[(&file, None)]), None, false, None);
        assert_eq!(result.len(), 1);
        assert!(!result[0].approved());
        assert_eq!(reasons(&result[0]), vec![R::SampleIndeterminate]);
        assert_eq!(
            result[0].local_movie.as_ref().unwrap().scene_name,
            None,
            "not a scene source"
        );
    }

    #[test]
    fn should_have_same_number_of_rejections_as_specs_that_failed() {
        let unsorted = Unsorted::new("several");
        let file = unsorted.file(FILE);
        let mut silent = probe(30.0).unwrap();
        silent.model["audioStreamCount"] = json!(0);
        let result = decisions(
            vec![file.clone()],
            probes(&[(&file, Some(silent))]),
            None,
            false,
            None,
        );
        assert_eq!(reasons(&result[0]), vec![R::NoAudio, R::Sample]);
        assert_eq!(result[0].rejections[1].message, "Sample");
    }

    #[test]
    fn should_not_blowup_the_process_due_to_failed_parse() {
        let unsorted = Unsorted::new("missing");
        // Three files that are not there: each still gets its decision.
        let files: Vec<String> = (1..=3)
            .map(|n| unsorted.path(&format!("The.Movie.2021.DVDRip.XviD-OSiTV.{n}.avi")))
            .collect();
        let result = decisions(files.clone(), Probes::default(), None, false, None);
        assert_eq!(result.len(), files.len());
        for decision in &result {
            assert_eq!(reasons(decision), vec![R::Error]);
            assert_eq!(decision.rejections[0].message, "Unexpected error processing file");
        }
    }

    #[test]
    fn should_return_a_decision_when_exception_is_caught() {
        let unsorted = Unsorted::new("caught");
        let file = unsorted.path("The.Movie.2021.DVDRip.XviD-OSiTV");
        let result = decisions(vec![file.clone()], Probes::default(), None, false, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].local_movie.as_ref().unwrap().path, file);
    }

    #[test]
    fn a_file_nothing_parses_for_is_rejected_as_unparseable() {
        let unsorted = Unsorted::new("unparseable");
        let file = unsorted.file("-/-.mkv");
        assert!(parser::parse_movie_path(&file).is_none());
        let result = decisions(
            vec![file.clone()],
            probes(&[(&file, probe(6000.0))]),
            None,
            true,
            None,
        );
        assert_eq!(reasons(&result[0]), vec![R::UnableToParse]);
        assert_eq!(result[0].rejections[0].message, "Unable to parse file");
    }

    #[test]
    fn the_download_client_item_names_the_file_and_its_tracked_release_flags_it() {
        let unsorted = Unsorted::new("client");
        let file = unsorted.file("abc.mkv");
        let item = item("The.Movie.2018.1080p.BluRay.x264-GRP");
        // The frame size outranks the names: it has to agree with them.
        let mut full_hd = probe(6000.0).unwrap();
        full_hd.model["width"] = json!(1920);
        full_hd.model["height"] = json!(1080);
        let result = decisions(
            vec![file.clone()],
            probes(&[(&file, Some(full_hd))]),
            Some(item.clone()),
            true,
            Some(8),
        );
        assert!(result[0].approved(), "{:?}", result[0].rejections);
        let local = result[0].local_movie.as_ref().unwrap();
        assert_eq!(local.quality.quality_id, BLURAY1080P);
        assert_eq!(local.release_group.as_deref(), Some("GRP"));
        assert_eq!(
            local.scene_name.as_deref(),
            Some("The.Movie.2018.1080p.BluRay.x264-GRP")
        );
        assert_eq!(local.indexer_flags, 8);
        assert_eq!(local.download_item.as_ref(), Some(&item));

        // Two files that are no samples: the client's title names neither.
        let other = unsorted.file("def.mkv");
        let result = decisions(
            vec![file.clone(), other.clone()],
            probes(&[(&file, probe(6000.0)), (&other, probe(6000.0))]),
            Some(item.clone()),
            true,
            None,
        );
        assert_eq!(result.len(), 2);
        for decision in &result {
            let local = decision.local_movie.as_ref().unwrap();
            assert!(local.other_video_files);
            assert_eq!(local.scene_name, None);
            assert_eq!(local.indexer_flags, 0);
        }

        // A sample next to the file does not count as another video file.
        let result = decisions(
            vec![file.clone(), other.clone()],
            probes(&[(&file, probe(6000.0)), (&other, probe(30.0))]),
            Some(item),
            true,
            None,
        );
        assert!(!result[0].local_movie.as_ref().unwrap().other_video_files);
        assert!(result[0].approved());
        assert_eq!(reasons(&result[1]), vec![R::Sample]);
    }

    #[test]
    fn files_the_library_holds_are_filtered_out() {
        let movie = movie();
        let held = MovieFile {
            id: 1,
            movie_id: movie.id,
            relative_path: "The Movie (2018).mkv".into(),
            original_file_path: None,
            size: 1,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            edition: None,
            quality: testing::quality(DVD, 1),
            languages: vec![lang::ENGLISH],
            media_info: None,
            indexer_flags: 0,
        };
        let files = vec![
            "/C/Test/Movie/The Movie (2018).mkv".to_string(),
            "/c/test/movie/the movie (2018).MKV".to_string(),
            "/C/Test/Movie/The Movie (2018) - extra.mkv".to_string(),
        ];
        assert_eq!(filter_existing(&files, &[], &movie), files);
        assert_eq!(
            filter_existing(&files, &[held], &movie),
            vec!["/C/Test/Movie/The Movie (2018) - extra.mkv".to_string()]
        );
    }

    /// `LocalMovieCustomFormatCalculationServiceFixture.should_update_custom_formats_on_local_movie`
    /// (Radarr 7): the formats under the name the import gives the file,
    /// and those under the name it has, each with its score.
    #[test]
    fn custom_formats_are_read_from_the_name_the_import_gives_the_file() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let local = db
            .call_blocking(move |conn| {
                let mut env = MovieEnvironment::load(conn, Utc::now())?;
                env.naming.rename_movies = true;
                env.naming.standard_movie_format = "{Movie Title} {Release Year} DVDRip English XviD-OSiTV".into();
                env.catalog.base.profiles = vec![testing::profile(&[], BLURAY1080P, true)];
                env.catalog.base.profiles[0].format_items = vec![
                    crate::model::ProfileFormatItem { format: 1, score: 10 },
                    crate::model::ProfileFormatItem { format: 2, score: 2 },
                ];
                let format = |id: i64, name: &str| CustomFormat {
                    id,
                    name: name.into(),
                    include_when_renaming: false,
                    specifications: json!([{"type": "ReleaseTitleSpecification", "body": {"value": name, "negate": false, "required": false}}]),
                };
                env.catalog.base.formats = vec![format(1, "English"), format(2, "Spanish")].into();
                let mut local = LocalMovie {
                    path: "/C/Test/Unsorted/The.Movie.2026.DVDRip.Spanish.XviD-OSiTV.avi".into(),
                    scene_name: Some("The.Movie.2026.DVDRip.XviD-OSiTV".into()),
                    movie: Some(movie()),
                    quality: testing::quality(DVD, 1),
                    languages: vec![lang::SPANISH],
                    ..Default::default()
                };
                score_formats_renamed(conn, &env, &mut local)?;
                Ok(local)
            })
            .unwrap();
        let names = |formats: &[CustomFormat]| formats.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            local.file_name_used_for_custom_format_calculation.as_deref(),
            Some("The Movie 2018 DVDRip English XviD-OSiTV")
        );
        assert_eq!(names(&local.custom_formats), vec!["English"]);
        assert_eq!(local.custom_format_score, 10);
        assert_eq!(names(&local.original_file_name_custom_formats), vec!["Spanish"]);
        assert_eq!(local.original_file_name_custom_format_score, 2);
    }

    #[test]
    fn custom_formats_are_read_from_the_scene_name_or_the_file_name() {
        let mut catalog = MovieCatalog::default();
        catalog.base.profiles = vec![testing::profile(&[], BLURAY1080P, true)];
        catalog.base.profiles[0].format_items =
            vec![crate::model::ProfileFormatItem { format: 1, score: 25 }];
        catalog.base.formats = vec![CustomFormat {
            id: 1,
            name: "Group".into(),
            include_when_renaming: false,
            specifications: json!([{"type": "ReleaseTitleSpecification", "body": {"value": "GRP", "negate": false, "required": false}}]),
        }].into();
        let mut local = LocalMovie {
            path: "/C/Test/Unsorted/abc.mkv".into(),
            movie: Some(movie()),
            quality: testing::quality(BLURAY1080P, 1),
            languages: vec![lang::ENGLISH],
            ..Default::default()
        };
        let own = file_name(&local.path).to_string();
        score_formats(&catalog, &mut local, &own);
        assert!(local.custom_formats.is_empty());
        assert_eq!(local.custom_format_score, 0);
        local.scene_name = Some("The.Movie.2018.1080p.BluRay.x264-GRP".into());
        let own = file_name(&local.path).to_string();
        score_formats(&catalog, &mut local, &own);
        assert_eq!(local.custom_formats.len(), 1);
        assert_eq!(local.custom_format_score, 25);
        local.scene_name = None;
        local.path = "/C/Test/Unsorted/The.Movie.2018.1080p.BluRay.x264-GRP.mkv".into();
        let own = file_name(&local.path).to_string();
        score_formats(&catalog, &mut local, &own);
        assert_eq!(local.custom_format_score, 25);
        local.movie = None;
        let own = file_name(&local.path).to_string();
        score_formats(&catalog, &mut local, &own);
        assert_eq!(local.custom_format_score, 0);
    }
}
