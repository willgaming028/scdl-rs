pub mod archive;
pub mod client;
pub mod config;
pub mod download;
pub mod error;
pub mod model;
pub mod naming;
pub mod pipeline;
pub mod resolve;
pub mod stream;
pub mod tag;

pub use client::{Client, ClientConfig, SearchKind, UserCollection};
pub use error::{Error, Result};
pub use model::{Codec, Entity, Format, Playlist, Protocol, Track, User};
