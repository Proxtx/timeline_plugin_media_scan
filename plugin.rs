use chrono::{DateTime, Utc};
use futures::StreamExt;
use mongodb::Cursor;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tokio::fs::{self, File};

use crate::{
    cache::{self, Cache},
    db::Event,
    AvailablePlugins, PluginData,
};

pub struct Plugin<'a> {
    plugin_data: PluginData<'a>,
    config: ConfigData,
    cache: Cache<LocationIndexingCache>,
}

#[derive(Serialize, Deserialize)]
struct ConfigData {
    pub locations: HashMap<String, MediaLocation>,
}

#[derive(Serialize, Deserialize)]
struct MediaLocation {
    location: PathBuf,
    #[serde(default)]
    name: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct LocationIndexingCache {
    timing_cache: HashMap<PathBuf, DateTime<Utc>>,
}

impl<'a> crate::Plugin<'a> for Plugin<'a> {
    async fn new(data: PluginData<'a>) -> Self
    where
        Self: Sized,
    {
        let mut config: ConfigData = toml::Value::try_into(
            data.config
                .expect("Failed to init media_scan plugin! No config was provided!")
                .clone(),
        )
        .unwrap_or_else(|e| panic!("Unable to init media_scan plugin! Provided config does not fit the requirements: {}", e));
        for (name, instance) in config.locations.iter_mut() {
            instance.name = name.to_string();
        }

        let cache: Cache<LocationIndexingCache> =
            Cache::load::<Plugin>().await.unwrap_or_else(|e| {
                panic!(
                    "Failed to init media_scan plugin! Unable to load cache: {}",
                    e
                )
            });

        Plugin {
            plugin_data: data,
            config,
            cache,
        }
    }

    fn get_type() -> crate::AvailablePlugins
    where
        Self: Sized,
    {
        AvailablePlugins::timeline_plugin_media_scan
    }
}

impl<'a> Plugin<'a> {
    async fn updated_media_directory(&mut self, location: &Path) {
        let latest_time = match self.cache.get().timing_cache.get(location) {
            Some(v) => v,
            None => {
                let mut updated_cache = self.cache.get().clone();
                updated_cache.timing_cache.insert(
                    location.to_path_buf(),
                    DateTime::from_timestamp_millis(0).unwrap(),
                ); //0 is a valid time-stamp
                self.cache.update::<Plugin>(updated_cache);
                self.cache.get().timing_cache.get(location).unwrap()
            }
        };
        let (media, new_latest_time) = match recursive_directory_scan(&location, latest_time).await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "The Media Scan plugin was unable to scan a directory: {:?} \n Error: {}",
                    location, e
                );
                return;
            }
        };

        let events_cursor: Cursor<MediaEvent> = self
            .plugin_data
            .database
            .get_events()
            .await
            .unwrap_or_else(|e| panic!("Database Error: {}", e));
    }
}

#[derive(Clone, Debug)]
pub struct Media {
    path: PathBuf,
    time_created: DateTime<Utc>,
}

type MediaEvent = Event<Media>;

const SUPPORTED_EXTENSIONS: [&str; 6] = ["png", "jpg", "mp4", "mkv", "webm", "jpeg"];

pub async fn recursive_directory_scan(
    path: &Path,
    current_newest: &DateTime<Utc>,
) -> Result<(Vec<Media>, DateTime<Utc>), std::io::Error> {
    let mut found_media = Vec::new();
    let mut updated_newest = current_newest.clone();
    let mut dir = fs::read_dir(path).await?;
    let mut next_result = dir.next_entry().await;
    while let Ok(Some(entry)) = next_result {
        let file_type = entry.file_type().await?;
        if file_type.is_dir() {
            let (mut found_media_recusion, updated_time) =
                Box::pin(recursive_directory_scan(&entry.path(), current_newest)).await?;
            found_media.append(&mut found_media_recusion);
            if updated_time > updated_newest {
                updated_newest = updated_time;
            }
        } else if file_type.is_file() {
            if let Some(ex) = entry.path().extension() {
                match ex.to_str() {
                    Some(ex) => {
                        if SUPPORTED_EXTENSIONS.contains(&ex) {
                            let creation_time = DateTime::from_timestamp_millis(
                                File::open(entry.path())
                                    .await?
                                    .metadata()
                                    .await?
                                    .creation_time() as i64,
                            )
                            .unwrap(); //I'm unsure why this is even an option. I think it's because the functions also accepts i64 values, which does not make sense because they would probably result in an error
                            found_media.push(Media {
                                path: entry.path(),
                                time_created: creation_time,
                            });
                            updated_newest = creation_time;
                        }
                    }
                    None => {
                        eprint!("The Media Scan plugin encountered an issue dealing with a filesystem file extension.");
                    }
                }
            }
        }

        next_result = dir.next_entry().await;
    }

    Ok((found_media, updated_newest))
}
