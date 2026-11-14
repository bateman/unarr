//! Managing the movies of the library: what `tv::series` is to Sonarr.
//! Looking a movie up, adding it, editing, moving and deleting it, after
//! Radarr's `SkyHookProxy.SearchForNewMovie`, `AddMovieService`,
//! `MovieService` and the validators of its `MovieController`.

use std::sync::LazyLock;

use fancy_regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::Db;
use crate::movies::metadata::{MapContext, MovieInfo, MovieMetadataError, MovieMetadataProvider};
use crate::movies::model::{Movie, MovieStatus};
use crate::movies::parser::parse_movie_title;
use crate::movies::store;
use crate::validation::ValidationFailure;

/// A movie a lookup found: the library's own when it is there, else what
/// the provider says.
#[derive(Debug, Clone)]
pub enum Found {
    Library(Box<Movie>),
    New(Box<MovieInfo>),
}

impl Found {
    pub fn tmdb_id(&self) -> i64 {
        match self {
            Found::Library(movie) => movie.metadata.tmdb_id,
            Found::New(info) => info.metadata.tmdb_id,
        }
    }
}

/// `SkyHookException`: the search could not be made. The API answers 503
/// with this message.
#[derive(Debug, Clone, PartialEq)]
pub struct LookupFailed(pub String);

impl std::fmt::Display for LookupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

static IMDB_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bimdb\.com/title/(tt\d{7,})\b").unwrap());
static TMDB_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bthemoviedb\.org/movie/(\d+)\b").unwrap());

/// `StripTrailingTheFromTitle`.
fn strip_trailing_the(title: &str) -> &str {
    title
        .strip_suffix(",the")
        .or_else(|| title.strip_suffix(", the"))
        .unwrap_or(title)
}

/// The library's movie with this TMDB id, else the provider's.
async fn library_or(db: &Db, info: MovieInfo) -> anyhow::Result<Found> {
    let tmdb_id = info.metadata.tmdb_id;
    let existing = db
        .call(move |conn| Ok(store::movies_by_tmdb_id(conn, tmdb_id)?.into_iter().next()))
        .await?;
    Ok(match existing {
        Some(movie) => Found::Library(Box::new(movie)),
        None => Found::New(Box::new(info)),
    })
}

async fn by_imdb_id(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    imdb_id: &str,
) -> Result<Vec<Found>, MovieMetadataError> {
    // `Parser.NormalizeImdbId`: an id that is none finds nothing.
    let Some(imdb_id) = crate::parser::title::normalize_imdb_id(imdb_id) else {
        return Ok(Vec::new());
    };
    match provider.movie_by_imdb_id(&imdb_id, ctx).await? {
        Some(info) => Ok(vec![library_or(db, info)
            .await
            .map_err(|e| MovieMetadataError::Invalid(e.to_string()))?]),
        None => Ok(Vec::new()),
    }
}

async fn by_tmdb_id(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    tmdb_id: i64,
) -> Result<Vec<Found>, MovieMetadataError> {
    let info = provider.movie_info(tmdb_id, ctx).await?;
    Ok(vec![library_or(db, info)
        .await
        .map_err(|e| MovieMetadataError::Invalid(e.to_string()))?])
}

/// `SearchForNewMovie`: a term, a `tmdb:` or `imdb:` id, a link to either
/// site, or a release name whose title and year are searched for.
pub async fn lookup(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    term: Option<&str>,
) -> Result<Vec<Found>, LookupFailed> {
    let invalid = |title: &str, detail: &str| {
        LookupFailed(format!(
            "Search for '{title}' failed. Invalid response received from RadarrAPI. {detail}"
        ))
    };
    // No term is a null the original's first pattern refuses.
    let Some(term) = term.filter(|t| !t.is_empty()) else {
        return Err(invalid("", "Value cannot be null. (Parameter 'input')"));
    };
    let linked = |pattern: &Regex| {
        pattern
            .captures(term)
            .ok()
            .flatten()
            .and_then(|found| found.get(1).map(|id| id.as_str().to_string()))
    };
    let title = if let Some(id) = linked(&IMDB_URL) {
        format!("imdb:{id}")
    } else if let Some(id) = linked(&TMDB_URL) {
        format!("tmdb:{id}")
    } else {
        term.to_string()
    };
    let failed = |e: MovieMetadataError| match e {
        MovieMetadataError::Unavailable(detail) | MovieMetadataError::Unreachable(detail) => LookupFailed(
            format!("Search for '{title}' failed. Unable to communicate with RadarrAPI. {detail}"),
        ),
        other => invalid(&title, &other.to_string()),
    };
    let lower = title.to_lowercase().replace('.', "");
    let mut parser_title = lower.clone();
    let mut year_term = String::new();
    // A release name: its title and year are what is searched for, an id
    // it carries finds the movie at once, and whatever goes wrong with
    // that id finds nothing.
    if let Some(parsed) = parse_movie_title(&title, true).filter(|p| p.primary_movie_title() != title) {
        parser_title = parsed.primary_movie_title().to_lowercase().replace('.', " ");
        if parsed.year > 1800 {
            year_term = parsed.year.to_string();
        }
        if !parsed.imdb_id.trim().is_empty() {
            return Ok(by_imdb_id(db, provider, ctx, &parsed.imdb_id)
                .await
                .unwrap_or_default());
        }
        if parsed.tmdb_id > 0 {
            return Ok(by_tmdb_id(db, provider, ctx, parsed.tmdb_id)
                .await
                .unwrap_or_default());
        }
    }
    let parser_title = strip_trailing_the(&parser_title);
    if lower.starts_with("imdb:") || lower.starts_with("imdbid:") {
        let slug = lower.split(':').nth(1).unwrap_or("").trim();
        if slug.is_empty() || slug.contains(char::is_whitespace) {
            return Ok(Vec::new());
        }
        return match by_imdb_id(db, provider, ctx, slug).await {
            Ok(found) => Ok(found),
            Err(MovieMetadataError::ImdbNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(failed(e)),
        };
    }
    if lower.starts_with("tmdb:") || lower.starts_with("tmdbid:") {
        let slug = lower.split(':').nth(1).unwrap_or("").trim();
        let Some(tmdb_id) = slug
            .parse::<i32>()
            .ok()
            .filter(|_| !slug.is_empty() && !slug.contains(char::is_whitespace))
        else {
            return Ok(Vec::new());
        };
        return match by_tmdb_id(db, provider, ctx, i64::from(tmdb_id)).await {
            Ok(found) => Ok(found),
            Err(MovieMetadataError::MovieNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(failed(e)),
        };
    }
    let search_term = parser_title.replace(['_', ' ', '.'], "+");
    let results = provider
        .search(&search_term, &year_term, ctx)
        .await
        .map_err(failed)?;
    let mut out = Vec::with_capacity(results.len());
    for info in results {
        out.push(
            library_or(db, info)
                .await
                .map_err(|e| invalid(&title, &e.to_string()))?,
        );
    }
    Ok(out)
}

/// What a client sends to add a movie: the fields `ApplyChanges` takes
/// from the resource. The rest comes from the provider.
#[derive(Debug, Clone)]
pub struct NewMovie {
    pub tmdb_id: i64,
    pub title: Option<String>,
    pub path: Option<String>,
    pub root_folder_path: Option<String>,
    pub quality_profile_id: i64,
    pub monitored: bool,
    pub minimum_availability: MovieStatus,
    pub tags: Vec<i64>,
    /// `AddMovieOptions` as it is stored: see [`add_options`].
    pub add_options: Option<Value>,
}

/// Why an add or an edit was refused.
#[derive(Debug)]
pub enum WriteError {
    /// FluentValidation's failures, as the 400 lists them.
    Invalid(Vec<ValidationFailure>),
    /// A `ValidationException` raised by the service with plain failures
    /// (the movie the provider does not know).
    InvalidPlain(Vec<ValidationFailure>),
    /// A null the original dereferences on the way: a 500.
    NullReference,
    /// `Path.Combine` given no root folder at all: the
    /// `ArgumentNullException` the original lets escape, a 500.
    NoRootFolder,
    /// A movie the library has, refused by the unique index when the
    /// movies are inserted, after every one of them validated: the
    /// `SQLiteException` the original answers 409 for.
    AlreadyAdded,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for WriteError {
    fn from(e: anyhow::Error) -> Self {
        WriteError::Other(e)
    }
}

/// `AddMovieOptions` as the original stores it: every member written, the
/// defaults where the client said nothing. `method` is how the movie came
/// (`manual`, `list`, `collection`) when the client does not say.
pub fn add_options(sent: Option<&Value>, method: &str) -> Value {
    let sent = sent.cloned().unwrap_or_else(|| json!({}));
    let flag = |key: &str| sent[key].as_bool().unwrap_or(false);
    json!({
        "ignoreEpisodesWithFiles": flag("ignoreEpisodesWithFiles"),
        "ignoreEpisodesWithoutFiles": flag("ignoreEpisodesWithoutFiles"),
        "monitor": sent["monitor"].as_str().unwrap_or("movieOnly"),
        "searchForMovie": flag("searchForMovie"),
        "addMethod": sent["addMethod"].as_str().unwrap_or(method),
    })
}

fn blank(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(|v| v.trim().is_empty())
}

/// `PathValidator`: an absolute path of the current OS.
fn valid_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0')
}

fn path_failure(property: &str, path: &str) -> ValidationFailure {
    let mut f = ValidationFailure::rule(
        property,
        "PathValidator",
        &format!("Invalid Path: '{path}'"),
        json!(path),
    );
    f.placeholders = vec![("path".into(), json!(path))];
    f
}

/// Whose rules a path is checked with: the two lists differ in their
/// order and in the system folders, which only the controller looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRules {
    /// `MovieController`'s `SharedValidator`: root folder, another
    /// movie's path, an ancestor of one, the recycling bin, a system
    /// folder.
    Controller,
    /// `AddMovieValidator`: root folder, the recycling bin, another
    /// movie's path, an ancestor of one.
    Service,
}

/// `IsValidPath` and the validators that follow it in `rules`: the first
/// that fails (the cascade stops). `movie_id` is the movie being edited,
/// whose own path is no clash.
pub fn path_rules(
    conn: &Connection,
    movie_id: i64,
    path: &str,
    rules: PathRules,
) -> anyhow::Result<Option<ValidationFailure>> {
    use crate::mediafiles::{is_parent_path, path_equals, relationship_to, system_folder_of};
    if !valid_path(path) {
        return Ok(Some(path_failure("Path", path)));
    }
    let with_path = |code: &str, message: String, extra: Vec<(&str, &str)>| {
        let mut f = ValidationFailure::rule("Path", code, &message, json!(path));
        f.placeholders = vec![("path".into(), json!(path))];
        f.placeholders
            .extend(extra.into_iter().map(|(k, v)| (k.to_string(), json!(v))));
        f
    };
    if crate::store::catalog::list_root_folders(conn)?
        .iter()
        .any(|r| path_equals(&r.path, path))
    {
        return Ok(Some(with_path(
            "RootFolderValidator",
            format!("Path '{path}' is already configured as a root folder"),
            vec![],
        )));
    }
    let recycle_bin = || -> anyhow::Result<Option<ValidationFailure>> {
        let bin = crate::store::config::config(conn, "recyclebin")?.unwrap_or_default();
        if bin.trim().is_empty() {
            return Ok(None);
        }
        Ok(relationship_to(&bin, path).map(|relationship| {
            with_path(
                "RecycleBinValidator",
                format!("Path '{path}' is {relationship} configured recycle bin folder"),
                vec![("relationship", relationship)],
            )
        }))
    };
    let other_movies = || -> anyhow::Result<Option<ValidationFailure>> {
        let others = store::all_movie_paths(conn)?;
        if others
            .iter()
            .any(|(id, p)| *id != movie_id && path_equals(p, path))
        {
            return Ok(Some(with_path(
                "MoviePathValidator",
                format!("Path '{path}' is already configured for an existing movie"),
                vec![],
            )));
        }
        if others.iter().any(|(_, p)| is_parent_path(path, p)) {
            return Ok(Some(with_path(
                "MovieAncestorValidator",
                format!("Path '{path}' is an ancestor of an existing movie"),
                vec![],
            )));
        }
        Ok(None)
    };
    match rules {
        PathRules::Service => {
            if let Some(f) = recycle_bin()? {
                return Ok(Some(f));
            }
            other_movies()
        }
        PathRules::Controller => {
            if let Some(f) = other_movies()? {
                return Ok(Some(f));
            }
            if let Some(f) = recycle_bin()? {
                return Ok(Some(f));
            }
            // The last system folder tried stays in the message's
            // arguments; the one that matched is the one it names.
            Ok(system_folder_of(path).map(|(relationship, system)| {
                with_path(
                    "SystemFolderValidator",
                    format!("Path '{path}' is {relationship} system folder {system}"),
                    vec![("systemFolder", system), ("relationship", relationship)],
                )
            }))
        }
    }
}

/// The rules of `MovieController` shared by POST and PUT, on the path and
/// the quality profile.
fn shared_rules(
    conn: &Connection,
    movie_id: i64,
    path: Option<&str>,
    quality_profile_id: i64,
    failures: &mut Vec<ValidationFailure>,
) -> anyhow::Result<()> {
    if let Some(path) = path.filter(|p| !p.trim().is_empty()) {
        if let Some(f) = path_rules(conn, movie_id, path, PathRules::Controller)? {
            failures.push(f);
        }
    }
    // `ValidId`, then `QualityProfileExistsValidator`.
    if quality_profile_id <= 0 {
        failures.push(ValidationFailure::greater_than(
            "QualityProfileId",
            json!(quality_profile_id),
            0,
        ));
    } else if !crate::store::catalog::list_quality_profiles(conn)?
        .iter()
        .any(|p| p.id == quality_profile_id)
    {
        failures.push(ValidationFailure::rule(
            "QualityProfileId",
            "QualityProfileExistsValidator",
            "Quality Profile does not exist",
            json!(quality_profile_id),
        ));
    }
    Ok(())
}

/// `MovieFolderAsRootFolderValidator`: a root folder named as the movie's
/// own folder would be, or nearly, is a movie folder taken for a root.
fn root_folder_looks_like_movie_folder(root: &str, movie_folder: &str) -> bool {
    let root_name = root.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if movie_folder == root_name {
        return true;
    }
    let distance = crate::decision::scene::levenshtein(movie_folder, root_name, 1, 1, 1) as f64;
    distance < f64::max(1.0, movie_folder.chars().count() as f64 * 0.2)
}

/// The folder the naming gives a movie known only by what the client
/// sent: its title, year and TMDB id.
fn folder_of_sent(conn: &Connection, m: &NewMovie, year: i64) -> anyhow::Result<String> {
    let mut metadata = blank_metadata(m.tmdb_id);
    metadata.title = m.title.clone().unwrap_or_default();
    metadata.year = year;
    Ok(crate::movies::naming::movie_folder(
        &crate::movies::naming::NamingMovie {
            metadata: &metadata,
            translations: &[],
        },
        &store::naming(conn)?,
    ))
}

fn blank_metadata(tmdb_id: i64) -> crate::movies::model::MovieMetadata {
    crate::movies::model::MovieMetadata {
        id: 0,
        tmdb_id,
        imdb_id: None,
        title: String::new(),
        sort_title: None,
        clean_title: None,
        original_title: None,
        clean_original_title: None,
        original_language_id: 1,
        status: MovieStatus::Tba,
        overview: None,
        images: Vec::new(),
        genres: Vec::new(),
        keywords: Vec::new(),
        ratings: json!({}),
        recommendations: Vec::new(),
        runtime: 0,
        in_cinemas: None,
        physical_release: None,
        digital_release: None,
        year: 0,
        secondary_year: None,
        certification: None,
        youtube_trailer_id: None,
        studio: None,
        website: None,
        popularity: 0.0,
        collection_tmdb_id: 0,
        collection_title: None,
        last_info_sync: None,
    }
}

/// `MovieController`'s POST rules, in the order FluentValidation runs
/// them. `year` is the year the client sent, which the folder check names
/// the movie's folder with.
pub fn validate_post(
    conn: &Connection,
    m: &NewMovie,
    year: i64,
) -> Result<Vec<ValidationFailure>, WriteError> {
    let mut failures = Vec::new();
    shared_rules(conn, 0, m.path.as_deref(), m.quality_profile_id, &mut failures)?;
    if blank(&m.root_folder_path) {
        if blank(&m.path) {
            failures.push(ValidationFailure::not_empty(
                "Path",
                m.path.clone().map_or(Value::Null, Value::from),
            ));
        } else if !valid_path(m.path.as_deref().unwrap_or("")) {
            failures.push(path_failure("Path", m.path.as_deref().unwrap_or("")));
        }
    }
    if blank(&m.path) {
        let root = m.root_folder_path.clone().unwrap_or_default();
        if blank(&m.root_folder_path) {
            failures.push(ValidationFailure::not_empty(
                "RootFolderPath",
                m.root_folder_path.clone().map_or(Value::Null, Value::from),
            ));
        } else if !valid_path(&root) {
            failures.push(path_failure("RootFolderPath", &root));
        } else if !crate::store::catalog::list_root_folders(conn)?
            .iter()
            .any(|r| crate::mediafiles::path_equals(&r.path, &root))
        {
            let mut f = ValidationFailure::rule(
                "RootFolderPath",
                "RootFolderExistsValidator",
                &format!("Root folder '{root}' does not exist"),
                json!(root),
            );
            f.placeholders = vec![("path".into(), json!(root))];
            failures.push(f);
        } else {
            // The folder is named from the title, which the original
            // dereferences without looking.
            if m.title.is_none() {
                return Err(WriteError::NullReference);
            }
            let folder = folder_of_sent(conn, m, year)?;
            if root_folder_looks_like_movie_folder(&root, &folder) {
                let mut f = ValidationFailure::rule(
                    "RootFolderPath",
                    "MovieFolderAsRootFolderValidator",
                    &format!("Root folder path '{root}' contains movie folder '{folder}'"),
                    json!(root),
                );
                f.placeholders = vec![
                    ("rootFolderPath".into(), json!(root)),
                    ("movieFolder".into(), json!(folder)),
                ];
                failures.push(f);
            }
        }
    }
    if m.tmdb_id <= 0 && blank(&m.title) {
        failures.push(ValidationFailure::not_empty(
            "Title",
            m.title.clone().map_or(Value::Null, Value::from),
        ));
    }
    if m.tmdb_id == 0 {
        failures.push(ValidationFailure::not_empty("TmdbId", json!(0)));
    } else if !store::movies_by_tmdb_id(conn, m.tmdb_id)?.is_empty() {
        failures.push(ValidationFailure::rule(
            "TmdbId",
            "MovieExistsValidator",
            "This movie has already been added",
            json!(m.tmdb_id),
        ));
    }
    Ok(failures)
}

/// `AddSkyhookData` and `SetPropertiesAndValidate`: the provider's movie
/// with the client's choices applied, its folder named, validated again.
async fn prepare(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    new: NewMovie,
) -> Result<Added, WriteError> {
    let info = match provider.movie_info(new.tmdb_id, ctx).await {
        Ok(info) => info,
        Err(MovieMetadataError::MovieNotFound(_)) => {
            tracing::error!(
                "TmdbId {} was not found, it may have been removed from TMDb. Path: {}",
                new.tmdb_id,
                new.path.as_deref().unwrap_or("")
            );
            return Err(WriteError::InvalidPlain(vec![ValidationFailure::setting(
                "TmdbId",
                &format!(
                    "A movie with this ID was not found. Path: {}",
                    new.path.as_deref().unwrap_or("")
                ),
                json!(new.tmdb_id),
            )]));
        }
        Err(e) => return Err(WriteError::Other(anyhow::anyhow!("{e}"))),
    };
    let now = ctx.now;
    db.call(move |conn| {
        let mut metadata = info.metadata;
        metadata.clean_title = Some(crate::movies::parser::clean_movie_title(&metadata.title));
        metadata.sort_title = Some(crate::movies::metadata::sort_title(
            &metadata.title,
            metadata.tmdb_id,
        ));
        let path = match new.path.clone().filter(|p| !p.trim().is_empty()) {
            Some(path) => path,
            None => {
                let translations: Vec<(i64, String)> = info
                    .translations
                    .iter()
                    .filter_map(|t| Some((t.language_id, t.title.clone()?)))
                    .collect();
                let folder = crate::movies::naming::movie_folder(
                    &crate::movies::naming::NamingMovie {
                        metadata: &metadata,
                        translations: &translations,
                    },
                    &store::naming(conn)?,
                );
                // `Path.Combine(RootFolderPath, folder)`: with no root
                // folder the folder's name is all there is, a relative
                // path the validation below refuses.
                // `Path.Combine` takes the root as sent, spaces and all: a
                // root with a space in front makes a path the rules refuse.
                match new.root_folder_path.as_deref() {
                    Some(root) if !root.is_empty() => {
                        format!("{}/{folder}", root.trim_end_matches('/'))
                    }
                    Some(_) => folder,
                    None => return Ok(Err(WriteError::NoRootFolder)),
                }
            }
        };
        // `AddMovieValidator`, on the path the movie ends up with.
        if let Some(failure) = path_rules(conn, 0, &path, PathRules::Service)? {
            return Ok(Err(WriteError::Invalid(vec![failure])));
        }
        Ok(Ok(Added {
            alternative_titles: info.alternative_titles,
            movie: Movie {
                id: 0,
                metadata,
                path,
                monitored: new.monitored,
                quality_profile_id: new.quality_profile_id,
                minimum_availability: new.minimum_availability,
                movie_file_id: 0,
                tags: new.tags,
                added: Some(now),
                add_options: new.add_options,
                last_search_time: None,
            },
        }))
    })
    .await?
}

/// A movie that was added, with the alternative titles the provider
/// lists for it: the model the original answers an import with. The
/// titles become rows with the movie's first refresh.
#[derive(Debug, Clone)]
pub struct Added {
    pub movie: Movie,
    pub alternative_titles: Vec<crate::movies::model::AlternativeTitle>,
}

/// `MovieMetadataService.Upsert` and `MovieRepository.Insert`.
fn insert(conn: &Connection, mut movie: Movie) -> anyhow::Result<Movie> {
    // What the library already knows of this movie's metadata (a part of
    // a collection, a list's item) keeps its row.
    store::upsert_metadata(conn, &mut movie.metadata)?;
    store::add_movie(conn, &mut movie)?;
    Ok(movie)
}

/// `AddMovieService.AddMovie` after the controller's validation. The
/// caller publishes `MovieAddedEvent`.
pub async fn add(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    new: NewMovie,
) -> Result<Movie, WriteError> {
    let movie = prepare(db, provider, ctx, new).await?.movie;
    tracing::info!("Adding Movie {} Path: [{}]", movie.metadata.title, movie.path);
    // The provider was asked between the validation and here: a second
    // request for the same movie may have added it meanwhile, and is
    // told what the validation would have said, in one transaction with
    // the insert, before the metadata row is touched.
    let added = db
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if !store::movies_by_tmdb_id(&tx, movie.metadata.tmdb_id)?.is_empty() {
                return Ok(Err(movie.metadata.tmdb_id));
            }
            let movie = insert(&tx, movie)?;
            tx.commit()?;
            Ok(Ok(movie))
        })
        .await?;
    added.map_err(|tmdb_id| {
        WriteError::Invalid(vec![ValidationFailure::rule(
            "TmdbId",
            "MovieExistsValidator",
            "This movie has already been added",
            json!(tmdb_id),
        )])
    })
}

/// `AddMovieService.AddMovies`: several movies, added together at one
/// moment. With `ignore_errors` a movie that does not validate is left
/// out; without, the first one refuses them all. A movie the library has
/// already is no validation failure: the insert meets the unique index
/// and refuses them all ([`WriteError::AlreadyAdded`]), unless
/// `skip_existing`, for the syncs (lists, collections) that left out what
/// the library had and only lose a race to a request. The caller
/// publishes `MoviesImportedEvent`.
pub async fn add_many(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    new: Vec<NewMovie>,
    ignore_errors: bool,
    skip_existing: bool,
) -> Result<Vec<Added>, WriteError> {
    let mut prepared: Vec<Added> = Vec::new();
    for movie in new {
        let tmdb_id = movie.tmdb_id;
        match prepare(db, provider, ctx, movie).await {
            // A path two of them would share is the second one's failure;
            // the same movie named twice goes on to the insert, which
            // refuses it as the library's own.
            Ok(one)
                if prepared.iter().any(|p| {
                    p.movie.metadata.tmdb_id != one.movie.metadata.tmdb_id
                        && crate::mediafiles::path_equals(&p.movie.path, &one.movie.path)
                }) =>
            {
                let path = &one.movie.path;
                let mut failure = ValidationFailure::rule(
                    "Path",
                    "MoviePathValidator",
                    &format!("Path '{path}' is already configured for an existing movie"),
                    json!(path),
                );
                failure.placeholders = vec![("path".into(), json!(path))];
                if !ignore_errors {
                    return Err(WriteError::Invalid(vec![failure]));
                }
            }
            Ok(one) => prepared.push(one),
            Err(WriteError::Invalid(failures)) | Err(WriteError::InvalidPlain(failures)) if ignore_errors => {
                tracing::debug!(
                    "TmdbId {tmdb_id} was not added due to validation failures. {}",
                    failures
                        .iter()
                        .map(|f| f.error_message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
            Err(e) => return Err(e),
        }
    }
    db.call(move |conn| {
        let tx = conn.unchecked_transaction()?;
        let mut added = Vec::with_capacity(prepared.len());
        for one in prepared {
            let tmdb_id = one.movie.metadata.tmdb_id;
            if !store::movies_by_tmdb_id(&tx, tmdb_id)?.is_empty() {
                if !skip_existing {
                    // Dropped, the transaction takes back what it inserted.
                    return Ok(Err(WriteError::AlreadyAdded));
                }
                // The provider was asked since the sync looked: a movie a
                // request added meanwhile is left out.
                tracing::debug!("TmdbId {tmdb_id} was not added: it is in the library already");
                continue;
            }
            added.push(Added {
                movie: insert(&tx, one.movie)?,
                alternative_titles: one.alternative_titles,
            });
        }
        tx.commit()?;
        Ok(Ok(added))
    })
    .await?
}

/// What `PUT /movie/{id}` changes: the fields `ApplyChanges` takes.
#[derive(Debug, Clone)]
pub struct MovieEdit {
    pub id: i64,
    pub path: Option<String>,
    pub quality_profile_id: i64,
    pub monitored: bool,
    pub minimum_availability: MovieStatus,
    pub tags: Vec<i64>,
    pub add_options: Option<Value>,
}

/// `MovieController`'s PUT rules.
pub fn validate_put(conn: &Connection, edit: &MovieEdit) -> anyhow::Result<Vec<ValidationFailure>> {
    let mut failures = Vec::new();
    shared_rules(
        conn,
        edit.id,
        edit.path.as_deref(),
        edit.quality_profile_id,
        &mut failures,
    )?;
    if blank(&edit.path) {
        failures.push(ValidationFailure::not_empty(
            "Path",
            edit.path.clone().map_or(Value::Null, Value::from),
        ));
    } else if !valid_path(edit.path.as_deref().unwrap_or("")) {
        failures.push(path_failure("Path", edit.path.as_deref().unwrap_or("")));
    }
    Ok(failures)
}

/// `MovieService.UpdateMovie`: the movie as it was and as it is now, or
/// `None` when there is no such movie. The caller publishes
/// `MovieEditedEvent`.
pub fn update(conn: &Connection, edit: &MovieEdit) -> anyhow::Result<Option<(Movie, Movie)>> {
    let Some(before) = store::get_movie(conn, edit.id)? else {
        return Ok(None);
    };
    let mut movie = before.clone();
    movie.path = edit.path.clone().unwrap_or_default();
    movie.quality_profile_id = edit.quality_profile_id;
    movie.monitored = edit.monitored;
    movie.minimum_availability = edit.minimum_availability;
    movie.tags = edit.tags.clone();
    movie.add_options = edit.add_options.clone();
    store::update_movie(conn, &movie)?;
    Ok(Some((before, movie)))
}

/// How the editor's tags are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyTags {
    Add,
    Remove,
    Replace,
}

/// `MovieEditorResource`: what the editor of the movie index changes on
/// every movie it names.
#[derive(Debug, Clone)]
pub struct BulkEdit {
    pub movie_ids: Vec<i64>,
    pub monitored: Option<bool>,
    pub quality_profile_id: Option<i64>,
    pub minimum_availability: Option<MovieStatus>,
    pub root_folder_path: Option<String>,
    pub tags: Option<Vec<i64>>,
    pub apply_tags: ApplyTags,
    pub move_files: bool,
}

/// `MoviePathBuilder.BuildPath`: a movie's path under another root
/// folder, with the folder it has now or the one the naming gives.
fn build_path(
    conn: &Connection,
    movie: &Movie,
    root: &str,
    use_existing_folder: bool,
) -> anyhow::Result<String> {
    let root = root.trim_end_matches('/');
    if use_existing_folder && !movie.path.trim().is_empty() {
        let roots: Vec<String> = crate::store::catalog::list_root_folders(conn)?
            .into_iter()
            .map(|r| r.path)
            .collect();
        let best = crate::health::best_root_folder(&movie.path, &roots);
        let best = best.trim_end_matches('/');
        // `get`, not an index: `is_parent_path` compares the paths with
        // their `..` resolved, so the root's length as written may fall
        // past the movie's path or inside one of its characters.
        let inside = crate::mediafiles::is_parent_path(best, &movie.path)
            .then(|| movie.path.get(best.len()..))
            .flatten();
        let relative = if let Some(rest) = inside {
            rest.trim_start_matches('/').to_string()
        } else {
            let name = movie.path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            tracing::warn!(
                "Unable to get relative path for movie path {}, using movie folder name {name}",
                movie.path
            );
            name.to_string()
        };
        return Ok(format!("{root}/{relative}"));
    }
    let folder = crate::movies::naming::movie_folder(
        &crate::movies::naming::NamingMovie {
            metadata: &movie.metadata,
            translations: &store::translation_titles(conn, movie.metadata.id)?,
        },
        &store::naming(conn)?,
    );
    Ok(format!("{root}/{folder}"))
}

/// What the editor did: the movies as they are now and, for those given
/// another root folder, `(movie id, the path it had)`.
pub type BulkEdited = (Vec<Movie>, Vec<(i64, String)>);

/// `MovieEditorController.SaveAll`: every movie changed and validated,
/// then all of them stored. Returns the movies as they are now and, for
/// those given another root folder, `(movie id, the path it had)`.
pub fn update_many(conn: &Connection, edit: &BulkEdit) -> Result<BulkEdited, WriteError> {
    let mut movies = store::movies_by_ids(conn, &edit.movie_ids)?;
    let mut moves = Vec::new();
    let root = edit.root_folder_path.clone().filter(|r| !r.trim().is_empty());
    for movie in &mut movies {
        if let Some(monitored) = edit.monitored {
            movie.monitored = monitored;
        }
        if let Some(profile) = edit.quality_profile_id {
            movie.quality_profile_id = profile;
        }
        if let Some(availability) = edit.minimum_availability {
            movie.minimum_availability = availability;
        }
        if root.is_some() {
            moves.push((movie.id, movie.path.clone()));
        }
        if let Some(tags) = &edit.tags {
            match edit.apply_tags {
                ApplyTags::Add => {
                    for tag in tags {
                        if !movie.tags.contains(tag) {
                            movie.tags.push(*tag);
                        }
                    }
                }
                ApplyTags::Remove => movie.tags.retain(|t| !tags.contains(t)),
                ApplyTags::Replace => {
                    let mut replaced: Vec<i64> = Vec::new();
                    for tag in tags {
                        if !replaced.contains(tag) {
                            replaced.push(*tag);
                        }
                    }
                    movie.tags = replaced;
                }
            }
        }
        // `MovieEditorValidator`.
        let mut failures = Vec::new();
        if let Some(root) = &root {
            if !valid_path(root) {
                failures.push(path_failure("RootFolderPath", root));
            } else if !crate::store::catalog::list_root_folders(conn)?
                .iter()
                .any(|r| crate::mediafiles::path_equals(&r.path, root))
            {
                let mut f = ValidationFailure::rule(
                    "RootFolderPath",
                    "RootFolderExistsValidator",
                    &format!("Root folder '{root}' does not exist"),
                    json!(root),
                );
                f.placeholders = vec![("path".into(), json!(root))];
                failures.push(f);
            }
        }
        if movie.quality_profile_id <= 0 {
            failures.push(ValidationFailure::greater_than(
                "QualityProfileId",
                json!(movie.quality_profile_id),
                0,
            ));
        } else if !crate::store::catalog::list_quality_profiles(conn)?
            .iter()
            .any(|p| p.id == movie.quality_profile_id)
        {
            failures.push(ValidationFailure::rule(
                "QualityProfileId",
                "QualityProfileExistsValidator",
                "Quality Profile does not exist",
                json!(movie.quality_profile_id),
            ));
        }
        if !failures.is_empty() {
            return Err(WriteError::Invalid(failures));
        }
    }
    let tx = conn.unchecked_transaction().map_err(anyhow::Error::from)?;
    for movie in &mut movies {
        if let Some(root) = &root {
            movie.path = build_path(&tx, movie, root, !edit.move_files)?;
        }
        store::update_movie(&tx, movie)?;
    }
    tx.commit().map_err(anyhow::Error::from)?;
    Ok((movies, moves))
}

/// What a deletion took with it.
#[derive(Debug, Clone)]
pub struct Deleted {
    pub movies: Vec<Movie>,
    /// The collections left without a movie of the library, removed.
    pub collections: Vec<i64>,
}

/// `MovieService.DeleteMovies` and the handlers of `MoviesDeletedEvent`
/// that work on the database: the movies with their files, history,
/// blocklist, extras, pending releases, titles, translations and credits;
/// the list exclusions asked for; the collections no movie belongs to any
/// more. `Err` with the id that is not a movie.
pub fn delete(conn: &Connection, ids: &[i64], add_exclusion: bool) -> anyhow::Result<Result<Deleted, i64>> {
    let mut movies = Vec::new();
    for &id in ids {
        match store::get_movie(conn, id)? {
            Some(movie) => movies.push(movie),
            None => return Ok(Err(id)),
        }
    }
    let tx = conn.unchecked_transaction()?;
    for movie in &movies {
        store::delete_movie_cascade(&tx, movie)?;
    }
    if add_exclusion {
        let mut excluded: std::collections::HashSet<i64> = store::list_import_exclusions(&tx)?
            .into_iter()
            .map(|e| e.tmdb_id)
            .collect();
        for movie in &movies {
            if !excluded.insert(movie.metadata.tmdb_id) {
                continue;
            }
            store::add_import_exclusion(
                &tx,
                &mut crate::movies::model::ImportExclusion {
                    id: 0,
                    tmdb_id: movie.metadata.tmdb_id,
                    movie_title: Some(movie.metadata.title.clone()),
                    movie_year: movie.metadata.year,
                },
            )?;
        }
    }
    // `MovieCollectionService.HandleAsync(MoviesDeletedEvent)`.
    let mut collections = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for movie in &movies {
        let tmdb_id = movie.metadata.collection_tmdb_id;
        if tmdb_id == 0 || !seen.insert(tmdb_id) || !store::movies_by_collection(&tx, tmdb_id)?.is_empty() {
            continue;
        }
        if let Some(collection) = store::collections_by_tmdb_id(&tx, tmdb_id)?.pop() {
            store::delete_collection(&tx, collection.id)?;
            collections.push(collection.id);
        }
    }
    tx.commit()?;
    for movie in &movies {
        tracing::info!("Deleted movie {}", movie.metadata.title);
    }
    Ok(Ok(Deleted { movies, collections }))
}

/// `MediaFileDeletionService.HandleAsync(MoviesDeletedEvent)`: a deleted
/// movie's folder to the recycling bin, or gone. Not when another movie
/// lives in it or under it.
pub fn delete_movie_folder(
    config: &crate::store::config::MediaManagementConfig,
    movie: &Movie,
    remaining_paths: &[(i64, String)],
) {
    for (_, p) in remaining_paths {
        if crate::mediafiles::is_parent_path(&movie.path, p) {
            tracing::error!(
                "Movie path: '{}' is a parent of another movie, not deleting files.",
                movie.path
            );
            return;
        }
        if crate::mediafiles::path_equals(&movie.path, p) {
            tracing::error!(
                "Movie path: '{}' is the same as another movie, not deleting files.",
                movie.path
            );
            return;
        }
    }
    let path = std::path::Path::new(&movie.path);
    if path.is_dir() {
        if let Err(e) = crate::mediafiles::transfer::recycle_folder(config, path) {
            tracing::error!(error = %e, "Unable to delete the folder of {}", movie.metadata.title);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_name_their_ids() {
        let id = |pattern: &Regex, text: &str| {
            pattern
                .captures(text)
                .unwrap()
                .map(|found| found.get(1).unwrap().as_str().to_string())
        };
        assert_eq!(
            id(&IMDB_URL, "https://www.imdb.com/title/tt1160419/").as_deref(),
            Some("tt1160419")
        );
        assert_eq!(
            id(&TMDB_URL, "https://www.themoviedb.org/movie/438631-dune").as_deref(),
            Some("438631")
        );
        assert_eq!(id(&IMDB_URL, "imdb.com/title/tt123"), None);
    }

    /// `RecycleBinValidator` and `SystemFolderValidator` on a movie's
    /// path: both for the controller, the bin alone for the service, and
    /// each list in its own order.
    #[test]
    fn a_movies_path_is_not_the_recycling_bin_nor_a_system_folder() {
        let db = crate::db::Db::open_in_memory_for(crate::Personality::Radarr).unwrap();
        db.call_blocking(|conn| {
            crate::store::config::set_config(conn, "recyclebin", "/data/bin")?;
            let failure = |path: &str, rules: PathRules| {
                path_rules(conn, 0, path, rules)
                    .unwrap()
                    .map(|f| (f.error_code.unwrap_or_default(), f.error_message))
            };
            for rules in [PathRules::Controller, PathRules::Service] {
                assert_eq!(failure("/movies/Dune (2021)", rules), None);
                assert_eq!(
                    failure("/data/bin", rules),
                    Some((
                        "RecycleBinValidator".into(),
                        "Path '/data/bin' is set to configured recycle bin folder".into()
                    ))
                );
                assert_eq!(
                    failure("/data/bin/Dune (2021)", rules),
                    Some((
                        "RecycleBinValidator".into(),
                        "Path '/data/bin/Dune (2021)' is child of configured recycle bin folder".into()
                    ))
                );
                // A folder the bin is inside of is no concern of this rule.
                assert_eq!(failure("/data", rules), None);
            }
            assert_eq!(
                failure("/usr/bin/Dune (2021)", PathRules::Controller),
                Some((
                    "SystemFolderValidator".into(),
                    "Path '/usr/bin/Dune (2021)' is child of system folder /usr/bin".into()
                ))
            );
            assert_eq!(
                failure("/proc", PathRules::Controller),
                Some((
                    "SystemFolderValidator".into(),
                    "Path '/proc' is set to system folder /proc".into()
                ))
            );
            // `AddMovieValidator` has no such rule.
            assert_eq!(failure("/usr/bin/Dune (2021)", PathRules::Service), None);
            // A relative path, which is what a library import without a
            // root folder comes to, is refused before any of them.
            assert_eq!(
                failure("Dune (2021)", PathRules::Service).map(|f| f.0),
                Some("PathValidator".into())
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_trailing_article_is_dropped() {
        assert_eq!(strip_trailing_the("holdovers, the"), "holdovers");
        assert_eq!(strip_trailing_the("holdovers,the"), "holdovers");
        assert_eq!(strip_trailing_the("the holdovers"), "the holdovers");
    }
}
