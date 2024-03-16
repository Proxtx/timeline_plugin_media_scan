use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use mongodb::{
    bson::{doc, Document},
    Collection, Cursor,
};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::{
    collections::HashMap,
    future::IntoFuture,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::fs::{self, File};

use crate::{
    cache::{self, Cache},
    db::Event,
    AvailablePlugins, Plugin as PluginTrait, PluginData,
};

use types::Timing;

pub struct Plugin {
    plugin_data: PluginData,
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

impl crate::Plugin for Plugin {
    async fn new(data: PluginData) -> Self
    where
        Self: Sized,
    {
        let mut config: ConfigData = toml::Value::try_into(
            data.config
                .clone().expect("Failed to init media_scan plugin! No config was provided!")
                ,
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

    fn request_loop_mut<'a>(
        &'a mut self,
    ) -> core::pin::Pin<Box<dyn futures::Future<Output = Option<chrono::Duration>> + Send + 'a>>
    {
        Box::pin(async move {
            self.update_all_locations().await;
            Some(chrono::Duration::try_minutes(1).unwrap())
        })
    }
}

impl Plugin {
    async fn update_all_locations(&mut self) {
        let calls = self
            .config
            .locations
            .iter()
            .map(|v| v.1.location.clone())
            .collect::<Vec<PathBuf>>();
        for path in calls {
            self.update_media_directory(&path).await;
        }
    }

    async fn update_media_directory(&mut self, location: &Path) {
        let latest_time = match self.cache.get().timing_cache.get(location) {
            Some(v) => v,
            None => {
                match self.cache.modify::<Plugin>(|cache| {
                cache.timing_cache.insert(
                    location.to_path_buf(),
                    DateTime::from_timestamp_millis(0).unwrap(),
                ); //0 is a valid time-stamp
                }) {
                    Ok(()) => {},
                    Err(e) => {
                        eprintln!("Unable to save to cache: {}", e);
                        return;
                    }
                }
                self.cache
                    .get()
                    .timing_cache
                    .get(location)
                    .expect("Unable to cache: Probably an error inside the cache")
                //we just updated the cache;
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
        let media: Vec<MediaEvent> = media
            .into_iter()
            .map(|media| Event {
                timing: Timing::Instant(media.time_created.clone()),
                id: media.path.clone(),
                plugin: Plugin::get_type(),
                event: media,
            })
            .collect();

        let paths: Vec<&str> = media.iter().map(|v| v.event.path.as_str()).collect();

        let already_found_media: Vec<MediaEvent> = match self
            .plugin_data
            .database
            .get_events()
            .find(
                doc! {
                    "event.path": {
                        "$in": paths
                    }
                },
                None,
            )
            .await
        {
            Ok(v) => match v.try_collect().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Unable to collect all matching paths: {}", e);
                    return;
                }
            },
            Err(e) => {
                eprintln!("Error fetching already found media from database: {}", e);
                return;
            }
        };

        let mut insert: Vec<MediaEvent> = Vec::new();

        for media in media {
            if !already_found_media.contains(&media) {
                insert.push(media)
            }
        }
        if !insert.is_empty() {
            match self.plugin_data.database.register_events(&insert).await {
                Ok(_t) => {
                    self.cache.modify::<Plugin>(move |data| {
                        data.timing_cache.insert(location.to_path_buf(), new_latest_time);
                    }).unwrap_or_else(|e| eprintln!("Unable to save cache (media scan plugin): {e}"));
                }
                Err(e) => {
                    eprintln!("Unable to add MediaEvent to Database: {}", e)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Media {
    path: String,
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
    while let Ok(Some(ref entry)) = next_result {
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
                            let file_creation_time =
                                match File::open(entry.path()).await?.metadata().await?.created() {
                                    Ok(v) => v,
                                    Err(e) => {
                                        eprintln!(
                                        "Unable to find creation time for file: {:?}\nError: {}",
                                        entry.path(),
                                        e
                                    );
                                        continue;
                                    }
                                };
                            let creation_time: DateTime<Utc> = file_creation_time.into();
                            if &creation_time > current_newest {
                                found_media.push(Media {
                                    path: entry.path().to_str().unwrap_or("default").to_string(),
                                    time_created: creation_time,
                                });
                                if creation_time > updated_newest {
                                    updated_newest = creation_time;
                                }
                            }
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
