use {
    std::path::PathBuf,
    leptos::{view, IntoView},
    crate::{api, plugin_manager::{PluginEventData, Style}},
    serde::Deserialize
};

#[derive(Clone, Debug, Deserialize)]
pub struct SignedMedia {
    path: String,
    signature: String
}

pub struct Plugin {}

impl crate::Plugin for Plugin {
    async fn new(
        _data: crate::plugin_manager::PluginData,
    ) -> Self
    where
        Self: Sized,
    {
        Plugin {}
    }

    fn get_component(&self, data: PluginEventData) -> crate::plugin_manager::EventResult<Box<dyn FnOnce() -> leptos::View>> {
        let media = data.get_data::<SignedMedia>()?;
        let path = PathBuf::from(media.path);
        let extension = path.extension().unwrap().to_str().unwrap().to_lowercase().to_string();
        let path_string = path.as_os_str().to_str().unwrap().to_string();
        let path_encoded = api::encode_url_component(&path_string);
        let signature_encoded = api::encode_url_component(&media.signature);
        let url = api::relative_url("/api/plugin/timeline_plugin_media_scan/file/").unwrap().join(&format!("{}/{}", &path_encoded, &signature_encoded)).unwrap().as_str().to_string();
        Ok(Box::new(move || {
            view! {
                {match extension.as_str() {
                    "mp4" | "mkv" | "webm" | "mov" => {
                        view! {
                            <video
                                style:width="100%"
                                style:color="var(--lightColor)"
                                src=url
                                controls
                            >
                                Loading video.
                            </video>
                        }
                            .into_view()
                    }
                    "mp3" | "opus" | "m4a" => {
                        view! {
                            <audio
                                style:width="100%"
                                style:color="var(--lightColor)"
                                src=url
                                controls
                            >
                                Loading audio
                            </audio>
                        }
                            .into_view()
                    }
                    _ => view! { <img style:width="100%" src=url/> }.into_view(),
                }}
            }.into_view()
        }))
    }

    fn get_style(&self) -> crate::plugin_manager::Style {
        Style::Acc1
    }
}