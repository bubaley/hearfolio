use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Manager, Runtime,
};

pub struct Recording<R: Runtime>(PluginHandle<R>);
impl<R: Runtime> Recording<R> {
    pub fn transfer<T: serde::de::DeserializeOwned>(&self, command: &str, payload: serde_json::Value) -> Result<T, String> {
        self.0.run_mobile_plugin(command, payload).map_err(|e|e.to_string())
    }
    pub fn run(&self, command: &str, payload: serde_json::Value) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>(command, payload)
            .map_err(|error| {
                let detail = error.to_string();
                for key in [
                    "microphonePermissionDenied",
                    "recordingEmpty",
                    "recordingUnavailable",
                    "recordingStart",
                    "recordingStop",
                    "operationBusy",
                ] {
                    let localized = format!("errors.{key}");
                    if detail.contains(&localized) {
                        return localized;
                    }
                }
                "errors.recordingUnavailable".into()
            })
    }
}
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("recording")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.bubaley.hearfolio.recording", "RecordingPlugin")?;
            app.manage(Recording(handle));
            Ok(())
        })
        .build()
}
