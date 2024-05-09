use {
    chrono::{DateTime, Utc},
    futures::{StreamExt, TryStreamExt},
    mongodb::bson::doc,
    rocket::{get, http::{CookieJar, Status}, routes, State},
    serde::{Deserialize, Serialize},
    std::{
        pin::Pin, str::FromStr,
        collections::HashMap,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU32, Ordering}
    },
    tokio::{fs::{self, File}, sync::RwLock},
    crate::{
        api::auth, cache::Cache, config::Config, db::{Database, Event}, AvailablePlugins, Plugin as PluginTrait, PluginData, plugin_manager::PluginManager
    },
    types::{api::CompressedEvent, timing::Timing}
};

pub struct Plugin {
    plugin_data: PluginData,
    config: ConfigData,
    cache: RwLock<Cache<LocationIndexingCache>>,
    full_reload_remaining: AtomicU32,
    current_status: RwLock<ScanStatus>
}

#[derive(Debug)]
enum ScanStatus {
    Busy(String),
    Waiting
}

impl std::fmt::Display for ScanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(w) => write!(f, "Busy with: {}", w),
            Self::Waiting => write!(f, "Waiting")
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ConfigData {
    pub locations: HashMap<String, MediaLocation>,
    pub interval: u32,
    pub full_reload_interval: Option<u32>,
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
            full_reload_remaining: match config.full_reload_interval {
                Some(v) => AtomicU32::from(v),
                None => AtomicU32::from(0)
            },
            config,
            cache: RwLock::new(cache),
            current_status: RwLock::new(ScanStatus::Waiting)
        }
    }

    fn get_type() -> crate::AvailablePlugins
    where
    Self: Sized,
    {
        AvailablePlugins::timeline_plugin_media_scan
    }
    
    fn request_loop<'a>(
        &'a self,
    ) -> core::pin::Pin<Box<dyn futures::Future<Output = Option<chrono::Duration>> + Send + 'a>>
    {
        Box::pin(async move {
            self.update_all_locations().await;
            Some(chrono::Duration::try_minutes(self.config.interval as i64).unwrap())
        })
    }

    fn get_compressed_events (&self, query_range: &types::timing::TimeRange) -> Pin<Box<dyn futures::Future<Output = types::api::APIResult<Vec<types::api::CompressedEvent>>> + Send>> {
        let filter = Database::generate_range_filter(query_range);
        let plg_filter = Database::generate_find_plugin_filter(AvailablePlugins::timeline_plugin_media_scan);
        let filter = Database::combine_documents(filter, plg_filter);
        let database = self.plugin_data.database.clone();
        Box::pin(async move {
            let mut cursor = database.get_events::<Media>().find(filter, None).await?;
            let mut result = Vec::new();
            while let Some(v) = cursor.next().await {
                let t = v?;
                result.push(CompressedEvent {
                    title: t.event.location_name,
                    time: t.timing,
                    data: Box::new(t.event.path)
                })
            }

            Ok(result)
        })
    }

    fn get_routes () -> Vec<rocket::Route> {
        routes![get_file, get_status]
    }
}

#[get("/file/<file>")]
async fn get_file (file: String, cookies: &CookieJar<'_>, config: &State<Config>) -> (Status, Option<Result<File, std::io::Error>>) {
    if auth(cookies, config).is_err() {
        return (Status::Unauthorized, None)
    }
    match PathBuf::from_str(&file) {
        Ok(v) => {
            (Status::Ok, (Some(File::open(v).await)))
        }
        Err(_) => {
            (Status::BadRequest, None)
        }
    }
}

#[get("/status")]
async fn get_status(cookies: &CookieJar<'_>, config: &State<Config>, plugin_manager: &State<PluginManager>) -> (Status, String) {
    let plg = plugin_manager.plugins.get("timeline_plugin_media_scan").unwrap().read().await;
    (Status::Ok, format!("{}\nFull reload remaining: {}", plg.))
}

impl Plugin {
    async fn update_all_locations(&self) {
        let ignore_cache = match self.config.full_reload_interval {
            Some(v) => {
                let current_remain = self.full_reload_remaining.load(Ordering::Relaxed);
                match current_remain == 0 {
                    true => {
                        self.full_reload_remaining.store(v, Ordering::Relaxed);
                        true
                    }
                    false => {
                        self.full_reload_remaining.store(current_remain-1, Ordering::Relaxed);
                        false
                    }
                }
            }
            None => false
        };
        for (name, location) in self.config.locations.iter() {
            {
                let mut status = self.current_status.write().await; 
                *status = ScanStatus::Busy(name.clone());
            }
            self.update_media_directory(name, &location.location, ignore_cache).await;
        }

        let mut status = self.current_status.write().await;
        *status = ScanStatus::Waiting;
    }

    async fn update_media_directory(&self, name: &str, location: &Path, full_reload: bool) {
        let last_cache = self.cache.read().await.get().timing_cache.get(location).cloned();
        let latest_time = match (full_reload, last_cache) {
            (false, Some(v)) => v,
            (false, None) => {
                let mod_result = self.cache.write().await.modify::<Plugin>(|cache| {
                cache.timing_cache.insert(
                    location.to_path_buf(),
                    DateTime::from_timestamp_millis(0).unwrap(),
                ); //0 is a valid time-stamp
                });
                match mod_result  {
                    Ok(()) => {},
                    Err(e) => {
                        self.plugin_data.report_error_string(format!("Unable to save to cache: {}", e));
                        return;
                    }
                }
                *self.cache.read().await
                    .get()
                    .timing_cache
                    .get(location)
                    .expect("Unable to cache: Probably an error inside the cache")
                //we just updated the cache;
            },
            (true, _) => {
                DateTime::from_timestamp_millis(0).unwrap()
            }
        };
        let (media, new_latest_time) = match recursive_directory_scan(name, location, &latest_time).await
        {
            Ok(v) => v,
            Err(e) => {
                self.plugin_data.report_error_string(format!("The Media Scan plugin was unable to scan a directory: {:?} \n Error: {}", location, e));
                return;
            }
        };
        let media: Vec<MediaEvent> = media
            .into_iter()
            .map(|media| Event {
                timing: Timing::Instant(media.time_modified),
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
                Database::combine_documents(Database::generate_find_plugin_filter(AvailablePlugins::timeline_plugin_media_scan), 
                doc! {
                    "event.path": {
                        "$in": paths
                    }
                }),
                None,
            )
            .await
        {
            Ok(v) => match v.try_collect().await {
                Ok(v) => v,
                Err(e) => {
                    self.plugin_data.report_error_string(format!("Unable to collect all matching paths: {}", e));
                    return;
                }
            },
            Err(e) => {
                self.plugin_data.report_error_string(format!("Error fetching already found media from database: {}", e));
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
                    self.cache.write().await.modify::<Plugin>(move |data| {
                        data.timing_cache.insert(location.to_path_buf(), new_latest_time);
                    }).unwrap_or_else(|e| {
                        self.plugin_data.report_error_string(format!("Unable to save cache (media scan plugin): {e}"));
                });
                }
                Err(e) => {
                    self.plugin_data.report_error_string(format!("Unable to add MediaEvent to Database: {}", e));
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Media {
    path: String,
    time_modified: DateTime<Utc>,
    location_name: String
}

type MediaEvent = Event<Media>;

const SUPPORTED_EXTENSIONS: [&str; 9] = ["png", "jpg", "mp4", "mkv", "webm", "jpeg", "mov", "heic", "gif"];

pub async fn recursive_directory_scan(
    location_name: &str,
    path: &Path,
    current_newest: &DateTime<Utc>,
) -> Result<(Vec<Media>, DateTime<Utc>), std::io::Error> {
    let mut found_media = Vec::new();
    let mut updated_newest = *current_newest;
    let mut dir = fs::read_dir(path).await?;
    let mut next_result = dir.next_entry().await;
    while let Ok(Some(ref entry)) = next_result {
        let file_type = entry.file_type().await?;
        if file_type.is_dir() {
            let (mut found_media_recusion, updated_time) =
                Box::pin(recursive_directory_scan(location_name, &entry.path(), current_newest)).await?;
            found_media.append(&mut found_media_recusion);
            if updated_time > updated_newest {
                updated_newest = updated_time;
            }
        } else if file_type.is_file() {
            if let Some(ex) = entry.path().extension() {
                match ex.to_str() {
                    Some(ex) => {
                        if SUPPORTED_EXTENSIONS.contains(&ex.to_lowercase().as_str()) {
                            let file_creation_time =
                                match File::open(entry.path()).await?.metadata().await?.modified() {
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
                                    time_modified: creation_time,
                                    location_name: location_name.to_string()
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
