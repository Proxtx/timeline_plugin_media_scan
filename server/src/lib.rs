use {
    base64::Engine,
    rsa::{
        pkcs1v15::{Signature, SigningKey, VerifyingKey},
        sha2::Sha256,
        signature::{Keypair, RandomizedSigner, SignatureEncoding, Verifier},
        RsaPrivateKey,
    },
    serde::{Deserialize, Serialize},
    server_api::{
        cache::Cache,
        config::Config,
        db::{Database, Event},
        external::{
            futures::{self, StreamExt, TryStreamExt},
            rocket::{
                self, get,
                http::{CookieJar, Status},
                routes, Build, Rocket, State,
            },
            tokio::{
                fs::{self, File},
                sync::RwLock,
            },
            toml,
            types::{
                self,
                api::CompressedEvent,
                available_plugins::AvailablePlugins,
                external::{
                    chrono::{self, DateTime, Utc},
                    mongodb::bson::doc,
                    serde_json,
                },
                timing::Timing,
            },
        },
        plugin::{PluginData, PluginTrait},
        web::auth,
    },
    std::{
        collections::HashMap,
        path::{Path, PathBuf},
        pin::Pin,
        str::FromStr,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        },
    },
};

pub struct Plugin {
    plugin_data: PluginData,
    config: ConfigData,
    cache: RwLock<Cache<LocationIndexingCache>>,
    full_reload_remaining: AtomicU32,
    current_status: Arc<RwLock<ScanStatus>>,
    signing_key: SigningKey<Sha256>,
    verifying_key: VerifyingKey<Sha256>,
}

#[derive(Debug)]
enum ScanStatus {
    Busy(String),
    Waiting(chrono::DateTime<chrono::Utc>),
}

impl std::fmt::Display for ScanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(w) => write!(f, "Busy with: {}", w),
            Self::Waiting(since) => write!(f, "Waiting since: {}", since),
        }
    }
}

struct VerifyingKeyWrapper(pub VerifyingKey<Sha256>);

#[derive(Serialize, Deserialize)]
struct ConfigData {
    pub locations: HashMap<String, MediaLocation>,
    pub interval: u32,
    pub full_reload_interval: Option<u32>,
    pub signing_key: RsaPrivateKey,
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

impl server_api::plugin::PluginTrait for Plugin {
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

        let signing_key = SigningKey::new(config.signing_key.clone());
        let verifying_key = signing_key.verifying_key();

        Plugin {
            plugin_data: data,
            full_reload_remaining: match config.full_reload_interval {
                Some(v) => AtomicU32::from(v),
                None => AtomicU32::from(0),
            },
            config,
            cache: RwLock::new(cache),
            current_status: Arc::new(RwLock::new(ScanStatus::Waiting(chrono::Utc::now()))),
            signing_key,
            verifying_key,
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

    fn get_compressed_events(
        &self,
        query_range: &types::timing::TimeRange,
    ) -> Pin<
        Box<
            dyn futures::Future<Output = types::api::APIResult<Vec<types::api::CompressedEvent>>>
                + Send,
        >,
    > {
        let filter = Database::generate_range_filter(query_range);
        let plg_filter =
            Database::generate_find_plugin_filter(AvailablePlugins::timeline_plugin_media_scan);
        let filter = Database::combine_documents(filter, plg_filter);
        let database = self.plugin_data.database.clone();
        let singing_key = self.signing_key.clone();
        Box::pin(async move {
            let mut cursor = database.get_events::<Media>().find(filter, None).await?;
            let mut result = Vec::new();
            while let Some(v) = cursor.next().await {
                let t = v?;
                result.push(CompressedEvent {
                    title: t.event.location_name,
                    time: t.timing,
                    data: serde_json::to_value(SignedMedia {
                        signature: sign_string(&singing_key, &t.event.path),
                        path: t.event.path,
                    })
                    .unwrap(),
                })
            }

            Ok(result)
        })
    }

    fn get_routes() -> Vec<rocket::Route> {
        routes![get_file, get_status]
    }

    fn rocket_build_access(&self, rocket: Rocket<Build>) -> Rocket<Build> {
        rocket
            .manage(self.current_status.clone())
            .manage(VerifyingKeyWrapper(self.verifying_key.clone()))
    }
}

#[get("/file/<file>/<signature>")]
async fn get_file(
    file: &str,
    signature: &str,
    verifying_key: &State<VerifyingKeyWrapper>,
) -> (Status, Option<Result<File, std::io::Error>>) {
    if !verify_string(&verifying_key.inner().0, file, signature) {
        return (Status::Unauthorized, None);
    }
    match PathBuf::from_str(file) {
        Ok(v) => (Status::Ok, (Some(File::open(v).await))),
        Err(_) => (Status::BadRequest, None),
    }
}

#[get("/status")]
async fn get_status(
    cookies: &CookieJar<'_>,
    config: &State<Config>,
    current_status: &State<Arc<RwLock<ScanStatus>>>,
) -> (Status, Option<String>) {
    if auth(cookies, config).is_err() {
        return (Status::Unauthorized, None);
    }
    let status = current_status.read().await;
    (Status::Ok, Some(format!("{}", status)))
}

fn sign_string(signing_key: &SigningKey<Sha256>, string: &str) -> String {
    let mut rng = rand::thread_rng();
    let signature = signing_key.sign_with_rng(&mut rng, string.as_bytes());
    base64::prelude::BASE64_STANDARD.encode(signature.to_vec())
}

fn verify_string(verifying_key: &VerifyingKey<Sha256>, string: &str, signature: &str) -> bool {
    let bytes = match base64::prelude::BASE64_STANDARD.decode(signature) {
        Ok(v) => v,
        Err(_e) => return false,
    };
    let bytes_slice: &[u8] = &bytes;
    verifying_key
        .verify(
            string.as_bytes(),
            &match Signature::try_from(bytes_slice) {
                Ok(v) => v,
                Err(_e) => return false,
            },
        )
        .is_ok()
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
                        self.full_reload_remaining
                            .store(current_remain - 1, Ordering::Relaxed);
                        false
                    }
                }
            }
            None => false,
        };
        for (name, location) in self.config.locations.iter() {
            {
                let mut status = self.current_status.write().await;
                *status = ScanStatus::Busy(name.clone());
            }
            self.update_media_directory(name, &location.location, ignore_cache)
                .await;
        }

        let mut status = self.current_status.write().await;
        *status = ScanStatus::Waiting(chrono::Utc::now());
    }

    async fn update_media_directory(&self, name: &str, location: &Path, full_reload: bool) {
        let last_cache = self
            .cache
            .read()
            .await
            .get()
            .timing_cache
            .get(location)
            .cloned();
        let latest_time = match (full_reload, last_cache) {
            (false, Some(v)) => v,
            (false, None) => {
                let mod_result = self.cache.write().await.modify::<Plugin>(|cache| {
                    cache.timing_cache.insert(
                        location.to_path_buf(),
                        DateTime::from_timestamp_millis(0).unwrap(),
                    ); //0 is a valid time-stamp
                });
                match mod_result {
                    Ok(()) => {}
                    Err(e) => {
                        self.plugin_data
                            .report_error_string(format!("Unable to save to cache: {}", e));
                        return;
                    }
                }
                *self
                    .cache
                    .read()
                    .await
                    .get()
                    .timing_cache
                    .get(location)
                    .expect("Unable to cache: Probably an error inside the cache")
                //we just updated the cache;
            }
            (true, _) => DateTime::from_timestamp_millis(0).unwrap(),
        };
        let (media, new_latest_time) =
            match recursive_directory_scan(name, location, &latest_time).await {
                Ok(v) => v,
                Err(e) => {
                    self.plugin_data.report_error_string(format!(
                        "The Media Scan plugin was unable to scan a directory: {:?} \n Error: {}",
                        location, e
                    ));
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
                Database::combine_documents(
                    Database::generate_find_plugin_filter(
                        AvailablePlugins::timeline_plugin_media_scan,
                    ),
                    doc! {
                        "event.path": {
                            "$in": paths
                        }
                    },
                ),
                None,
            )
            .await
        {
            Ok(v) => match v.try_collect().await {
                Ok(v) => v,
                Err(e) => {
                    self.plugin_data.report_error_string(format!(
                        "Unable to collect all matching paths: {}",
                        e
                    ));
                    return;
                }
            },
            Err(e) => {
                self.plugin_data.report_error_string(format!(
                    "Error fetching already found media from database: {}",
                    e
                ));
                return;
            }
        };

        let mut insert: Vec<MediaEvent> = Vec::new();

        for media in media {
            let mut found = false;
            for existing in already_found_media.iter() {
                if existing.id == media.id {
                    found = true;
                    break;
                }
            }
            if !found {
                insert.push(media)
            }
        }
        if !insert.is_empty() {
            match self.plugin_data.database.register_events(&insert).await {
                Ok(_t) => {
                    self.cache
                        .write()
                        .await
                        .modify::<Plugin>(move |data| {
                            data.timing_cache
                                .insert(location.to_path_buf(), new_latest_time);
                        })
                        .unwrap_or_else(|e| {
                            self.plugin_data.report_error_string(format!(
                                "Unable to save cache (media scan plugin): {e}"
                            ));
                        });
                }
                Err(e) => {
                    self.plugin_data.report_error_string(format!(
                        "Unable to add MediaEvent to Database: {}",
                        e
                    ));
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Media {
    path: String,
    time_modified: DateTime<Utc>,
    location_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedMedia {
    path: String,
    signature: String,
}

type MediaEvent = Event<Media>;

const SUPPORTED_EXTENSIONS: [&str; 12] = [
    "png", "jpg", "mp4", "mkv", "webm", "jpeg", "mov", "heic", "gif", "mp3", "opus", "m4a",
];

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
            let (mut found_media_recusion, updated_time) = Box::pin(recursive_directory_scan(
                location_name,
                &entry.path(),
                current_newest,
            ))
            .await?;
            found_media.append(&mut found_media_recusion);
            if updated_time > updated_newest {
                updated_newest = updated_time;
            }
        } else if file_type.is_file() {
            if let Some(ex) = entry.path().extension() {
                match ex.to_str() {
                    Some(ex) => {
                        if SUPPORTED_EXTENSIONS.contains(&ex.to_lowercase().as_str()) {
                            let file_creation_time = match File::open(entry.path())
                                .await?
                                .metadata()
                                .await?
                                .modified()
                            {
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
                                    location_name: location_name.to_string(),
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
