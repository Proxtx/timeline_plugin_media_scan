use raqote::{DrawTarget, Source, SolidSource, DrawOptions, Image};
use std::pin::Pin;
use shared::timeline::types::api::CompressedEvent;
use rand::Rng;

pub struct PluginRenderer {}

impl crate::renderer::PluginRenderer for PluginRenderer {
    async fn new() -> PluginRenderer {
        PluginRenderer {}
    }

    fn render(&self, dimensions: (i32, i32), _event: &CompressedEvent) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u32>, String>> + Send>> {
        Box::pin(async move {
            let mut rng = rand::thread_rng();
            let mut target = DrawTarget::new(dimensions.0, dimensions.1);
            target.fill_rect(0., 0., dimensions.0 as f32, dimensions.1 as f32, &Source::Solid(SolidSource::from_unpremultiplied_argb(255, rng.gen(), 0, 0)), &DrawOptions::new());
            Ok(target.into_vec())
        })
    }
}
