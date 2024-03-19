use leptos::view;

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

    fn get_component(&self, data: PluginEventData) -> crate::event_manager::EventResult<Box<dyn Fn() -> leptos::View>> {
        let _ = data;
        Ok(|| {
            view! { <h1>Hello</h1> }.into_view()
        })
    }
}
