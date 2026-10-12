use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Manager, Runtime,
};

pub struct Recording<R: Runtime>(PluginHandle<R>);
impl<R: Runtime> Recording<R> {
    pub fn transfer<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        payload: serde_json::Value,
    ) -> Result<T, String> {
        self.0
            .run_mobile_plugin(command, payload)
            .map_err(|e| e.to_string())
    }
    pub fn run<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        payload: serde_json::Value,
    ) -> Result<T, String> {
        self.0
            .run_mobile_plugin::<T>(command, payload)
            .map_err(|error| recording_error(&error.to_string()))
    }
    pub async fn run_async<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        payload: serde_json::Value,
    ) -> Result<T, String> {
        self.0
            .run_mobile_plugin_async::<T>(command, payload)
            .await
            .map_err(|error| recording_error(&error.to_string()))
    }
}
fn recording_error(detail: &str) -> String {
    for key in [
        "microphonePermissionDenied",
        "recordingEmpty",
        "recordingUnavailable",
        "recordingInterrupted",
        "recordingStart",
        "recordingStop",
        "operationBusy",
        "audioShare",
        "textShare",
        "exportWrite",
        "audioPlayback",
    ] {
        let localized = format!("errors.{key}");
        if detail.contains(&localized) {
            return localized;
        }
    }
    "errors.recordingUnavailable".into()
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

#[cfg(test)]
mod tests {
    use super::recording_error;
    #[test]
    fn claimed_activity_destruction_preserves_interrupted_start_error() {
        assert_eq!(
            recording_error("Native callback: errors.recordingInterrupted"),
            "errors.recordingInterrupted"
        );
        assert_eq!(
            recording_error("Native callback: errors.recordingUnavailable"),
            "errors.recordingUnavailable"
        );
        assert_eq!(
            recording_error("unrecognized native failure"),
            "errors.recordingUnavailable"
        );
    }
}
