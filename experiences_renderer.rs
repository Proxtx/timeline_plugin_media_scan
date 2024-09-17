use std::path::PathBuf;
use std::pin::Pin;
use shared::timeline::types::api::CompressedEvent;
use crate::renderer::render_image;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SignedMedia {
    path: String,
    signature: String
}

pub struct PluginRenderer {}

impl crate::renderer::PluginRenderer for PluginRenderer {
    async fn new() -> PluginRenderer {
        PluginRenderer {}
    }

    fn render(&self, dimensions: (i32, i32), event: &CompressedEvent) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u32>, String>> + Send>> {
        let data = event.data.clone();

        Box::pin(async move {
            let path = match serde_json::from_str::<SignedMedia>(&data) {
                Ok(v) => v,
                Err(e) => {
                    return Err(format!("Unable to read CompressedEvent: {}", e))
                }
            }.path;
            render_image(dimensions, &PathBuf::from(path)).await.map(|v|v.into_vec())
        })
    }
}
