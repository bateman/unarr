//! The movie side of the domain, Radarr's: what `tv`, the series store and
//! the series model are to Sonarr. The two families sit next to each other
//! and share the pieces that carry no media notion (qualities' revisions,
//! custom formats, profiles, providers); there is no common abstraction
//! over a series and a movie, because the originals' rules live exactly
//! where the two differ.

pub mod availability;
pub mod commands;
pub mod decision;
pub mod downloads;
pub mod formats;
pub mod model;
pub mod naming;
pub mod parser;
pub mod quality;
pub mod request;
pub mod search;
pub mod store;
