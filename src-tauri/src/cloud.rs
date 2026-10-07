use std::{fs, io::{BufRead, BufReader, Write}, path::Path, time::Duration};
use serde::{Deserialize, Serialize};
use base64::Engine;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all="camelCase")]
pub struct Settings {
    pub provider: String,
    pub local_model: String,
    pub openrouter_url: String,
    pub openrouter_model: String,
    #[serde(default="default_mode")]
    pub openrouter_mode: String,
    #[serde(default, skip_serializing_if="String::is_empty")]
    token: String,
}
fn default_mode() -> String { "streaming".into() }
impl Default for Settings {
    fn default() -> Self { Self { provider:"local".into(), local_model:"whisper-base".into(), openrouter_url:"https://openrouter.ai/api/v1".into(), openrouter_model:"google/gemini-2.5-flash".into(), openrouter_mode:default_mode(), token:String::new() } }
}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub struct PublicSettings {
    provider:String, local_model:String, openrouter_url:String, openrouter_model:String, openrouter_mode:String, has_token:bool,
}
impl Settings {
    pub fn public(&self) -> PublicSettings { PublicSettings { provider:self.provider.clone(), local_model:self.local_model.clone(), openrouter_url:self.openrouter_url.clone(), openrouter_model:self.openrouter_model.clone(), openrouter_mode:self.openrouter_mode.clone(), has_token:!self.token.is_empty() } }
    fn validate(&mut self) -> Result<(), String> {
        if !["streaming","transcription"].contains(&self.openrouter_mode.as_str()) { return Err("Неизвестный режим OpenRouter".into()); }
        if !["local","openrouter"].contains(&self.provider.as_str()) { return Err("Неизвестный способ распознавания".into()); }
        if !super::MODELS.iter().any(|(id,_,_)| *id == self.local_model) { return Err("Неизвестная локальная модель".into()); }
        self.openrouter_url = self.openrouter_url.trim().trim_end_matches('/').into();
        let url = reqwest::Url::parse(&self.openrouter_url).map_err(|_| "Введите корректный URL API")?;
        let local = matches!(url.host_str(),Some("localhost" | "127.0.0.1" | "::1"));
        if url.scheme() != "https" && !(local && url.scheme() == "http") { return Err("URL API должен использовать HTTPS".into()); }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() { return Err("URL API не должен содержать пароль, параметры или фрагмент".into()); }
        self.openrouter_model = self.openrouter_model.trim().into();
        if self.openrouter_model.is_empty() { return Err("Укажите модель OpenRouter".into()); }
        Ok(())
    }
}
pub fn load() -> Result<Settings,String> {
    let path = super::base()?.join("settings.json");
    if !path.exists() { return Ok(Settings::default()); }
    let mut settings:Settings = serde_json::from_slice(&fs::read(path).map_err(|_| "Не удалось прочитать настройки")?).map_err(|_| "Повреждён файл настроек")?;
    settings.validate()?; Ok(settings)
}
fn persist(path:&Path, settings:&Settings) -> Result<(),String> {
    let temp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new(); options.write(true).create(true).truncate(true);
    #[cfg(unix)] { use std::os::unix::fs::{OpenOptionsExt,PermissionsExt}; options.mode(0o600); if temp.exists() { fs::set_permissions(&temp,fs::Permissions::from_mode(0o600)).map_err(|_| "Не удалось защитить настройки")?; } }
    let mut file = options.open(&temp).map_err(|_| "Не удалось сохранить настройки")?;
    file.write_all(&serde_json::to_vec_pretty(settings).map_err(|_| "Не удалось сохранить настройки")?).map_err(|_| "Не удалось сохранить настройки")?;
    file.sync_all().map_err(|_| "Не удалось сохранить настройки")?;
    fs::rename(temp,path).map_err(|_| "Не удалось сохранить настройки".to_string())
}
#[tauri::command]
pub fn get_settings() -> Result<PublicSettings,String> { Ok(load()?.public()) }
#[tauri::command]
pub fn save_settings(mut settings:Settings, token:Option<String>) -> Result<PublicSettings,String> {
    settings.token = match token { Some(token)=>token, None=>load()?.token }.trim().into(); settings.validate()?;
    persist(&super::base()?.join("settings.json"),&settings)?; Ok(settings.public())
}
fn client() -> Result<reqwest::blocking::Client,String> {
    reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(20)).timeout(Duration::from_secs(900)).build().map_err(|_| "Не удалось создать HTTP клиент".to_string())
}
fn api_error(body:&str, token:&str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v|v["error"]["message"].as_str().map(str::to_owned)).unwrap_or_else(||"Сервис вернул ошибку".into());
    let redacted = if token.is_empty() { message } else { message.replace(token,"[скрыто]") };
    redacted.chars().take(500).collect()
}
#[derive(Serialize)]
pub struct CloudModel { id:String, name:String }
#[tauri::command]
pub async fn list_openrouter_models() -> Result<Vec<CloudModel>,String> {
    tauri::async_runtime::spawn_blocking(|| {
        let s=load()?;
        let mut request=client()?.get(format!("{}/models",s.openrouter_url));
        if !s.token.is_empty() { request=request.bearer_auth(&s.token); }
        let response=request.send().map_err(|_| "Не удалось загрузить список моделей")?;
        if !response.status().is_success() { return Err(format!("Список моделей: HTTP {}",response.status().as_u16())); }
        let body:serde_json::Value=response.json().map_err(|_|"Некорректный список моделей")?;
        let mut models=body["data"].as_array().ok_or("Некорректный список моделей")?.iter().filter(|v| v["architecture"]["output_modalities"].as_array().is_some_and(|a| a.iter().any(|m| m == if s.openrouter_mode == "transcription" {"transcription"} else {"text"}))).filter(|v| v["architecture"]["input_modalities"].as_array().is_some_and(|a|a.iter().any(|m|m=="audio"))).filter_map(|v|Some(CloudModel{id:v["id"].as_str()?.into(),name:v["name"].as_str().unwrap_or(v["id"].as_str()?).into()})).collect::<Vec<_>>();
        models.sort_by(|a,b|a.name.cmp(&b.name)); Ok(models)
    }).await.map_err(|_| "Не удалось загрузить модели")?
}
fn consume_sse(reader:impl BufRead, token:&str, mut update:impl FnMut(&str)) -> Result<String,String> {
    let mut text=String::new(); let mut data=Vec::new(); let mut done=false;
    let mut event = |data:&mut Vec<String>| -> Result<bool,String> {
        if data.is_empty() { return Ok(false); }
        let payload=data.join("\n"); data.clear();
        if payload.trim()=="[DONE]" { return Ok(true); }
        let value:serde_json::Value=serde_json::from_str(&payload).map_err(|_|"Некорректный поток OpenRouter")?;
        if !value["error"].is_null() { return Err(api_error(&payload,token)); }
        if value["choices"][0]["finish_reason"]=="error" { return Err("OpenRouter прервал распознавание".into()); }
        if matches!(value["choices"][0]["finish_reason"].as_str(), Some("length" | "content_filter")) { return Err("OpenRouter вернул неполную транскрипцию".into()); }
        if let Some(delta)=value["choices"][0]["delta"]["content"].as_str() { text.push_str(delta); update(&text); }
        Ok(false)
    };
    for line in reader.lines() {
        let line=line.map_err(|_|"Соединение OpenRouter прервано")?;
        if line.is_empty() { if event(&mut data)? { done=true; break; } }
        else if let Some(value)=line.strip_prefix("data:") { data.push(value.strip_prefix(' ').unwrap_or(value).into()); }
    }
    if !done { done=event(&mut data)?; }
    if !done { return Err("OpenRouter завершил соединение до окончания ответа".into()); }
    if text.trim().is_empty() { return Err("OpenRouter вернул пустой текст".into()); }
    Ok(text.trim().into())
}
pub fn transcribe(s:&Settings, audio:&Path, mut update:impl FnMut(&str)) -> Result<String,String> {
    if s.token.is_empty() { return Err("Добавьте API токен OpenRouter в настройках".into()); }
    let size=fs::metadata(audio).map_err(|_|"Аудиофайл не найден")?.len();
    if size > 100*1024*1024 { return Err("Для OpenRouter запись должна быть меньше 100 МБ; выберите локальную модель или сократите запись".into()); }
    let data=base64::engine::general_purpose::STANDARD.encode(fs::read(audio).map_err(|_|"Не удалось прочитать аудио")?);
    let format=audio.extension().and_then(|e|e.to_str()).unwrap_or("wav");
    if s.openrouter_mode == "transcription" {
        let response=client()?.post(format!("{}/audio/transcriptions",s.openrouter_url)).bearer_auth(&s.token).timeout(Duration::from_secs(75)).json(&serde_json::json!({"model":s.openrouter_model,"input_audio":{"data":data,"format":format}})).send().map_err(|_| "Не удалось связаться с OpenRouter; проверьте URL и подключение")?;
        let status=response.status(); let body=response.text().map_err(|_|"Не удалось прочитать ответ OpenRouter")?;
        if !status.is_success() { return Err(format!("OpenRouter HTTP {}: {}",status.as_u16(),api_error(&body,&s.token))); }
        let value:serde_json::Value=serde_json::from_str(&body).map_err(|_|"Некорректный ответ OpenRouter")?;
        if !value["error"].is_null() { return Err(api_error(&body,&s.token)); }
        let text=value["text"].as_str().ok_or("OpenRouter вернул пустой текст")?.trim();
        if text.is_empty() { return Err("OpenRouter вернул пустой текст".into()); } update(text); return Ok(text.into());
    }
    let body=serde_json::json!({"model":s.openrouter_model,"stream":true,"messages":[{"role":"user","content":[{"type":"text","text":"Transcribe all speech in this audio accurately in its original language. Return only the transcript, without commentary, summaries, or invented content."},{"type":"input_audio","input_audio":{"data":data,"format":format}}]}]});
    let response=client()?.post(format!("{}/chat/completions",s.openrouter_url)).bearer_auth(&s.token).json(&body).send().map_err(|_|"Не удалось связаться с OpenRouter; проверьте URL и подключение")?;
    if !response.status().is_success() { let status=response.status().as_u16(); let body=response.text().unwrap_or_default(); return Err(format!("OpenRouter HTTP {status}: {}",api_error(&body,&s.token))); }
    if response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).is_some_and(|v|v.contains("text/event-stream")) {
        consume_sse(BufReader::new(response),&s.token,&mut update)
    } else {
        let body=response.text().map_err(|_|"Не удалось прочитать ответ OpenRouter")?;
        let v:serde_json::Value=serde_json::from_str(&body).map_err(|_|"Некорректный ответ OpenRouter")?;
        if !v["error"].is_null() { return Err(api_error(&body,&s.token)); }
        if matches!(v["choices"][0]["finish_reason"].as_str(), Some("length" | "content_filter")) { return Err("OpenRouter вернул неполную транскрипцию".into()); }
        let text=v["choices"][0]["message"]["content"].as_str().ok_or("OpenRouter вернул пустой текст")?.trim();
        if text.is_empty() { return Err("OpenRouter вернул пустой текст".into()); } update(text); Ok(text.into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn stream_handles_comments_unicode_and_done() {
        let stream=": processing\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Привет \"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"мир\"}}]}\n\ndata: [DONE]\n\n";
        let mut updates=vec![]; assert_eq!(consume_sse(stream.as_bytes(),"",|s|updates.push(s.to_owned())).unwrap(),"Привет мир"); assert_eq!(updates.len(),2);
    }
    #[test] fn stream_rejects_interruption_and_redacts_error() {
        assert!(consume_sse(b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".as_slice(),"", |_|{}).is_err());
        let err=consume_sse(b"data: {\"error\":{\"message\":\"bad secret\"}}\n\n".as_slice(),"secret", |_|{}).unwrap_err(); assert!(!err.contains("secret"));
    }
    fn mock_request(mode:&str) {
        use std::{io::{Read,Write},net::TcpListener,thread};
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let mut settings=Settings::default(); settings.token="test-token".into(); settings.openrouter_mode=mode.into();
        let stt=mode=="transcription"; settings.openrouter_url=format!("http://{}",listener.local_addr().unwrap());
        let handle=thread::spawn(move || {
            let (mut socket,_)=listener.accept().unwrap(); socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader=BufReader::new(socket.try_clone().unwrap()); let mut first=String::new(); reader.read_line(&mut first).unwrap(); assert!(first.starts_with(if stt {"POST /audio/transcriptions "} else {"POST /chat/completions "}));
            let mut length=0;
            loop { let mut line=String::new(); reader.read_line(&mut line).unwrap(); if line=="\r\n" {break;} if line.to_lowercase().starts_with("content-length:") {length=line.split(':').nth(1).unwrap().trim().parse::<usize>().unwrap();} }
            let mut body=vec![0;length]; reader.read_exact(&mut body).unwrap(); let v:serde_json::Value=serde_json::from_slice(&body).unwrap();
            if stt { assert_eq!(v["input_audio"]["format"],"wav"); assert_eq!(v["input_audio"]["data"],"c2FtcGxl"); } else {
            assert_eq!(v["stream"],true); assert_eq!(v["messages"][0]["content"][1]["input_audio"]["format"],"wav"); assert_eq!(v["messages"][0]["content"][1]["input_audio"]["data"],"c2FtcGxl"); }
            let body=if stt {r#"{"text":"hello"}"#} else {"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n"};
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",if stt {"application/json"} else {"text/event-stream"},body.len(),body).unwrap();
        });
        let path=std::env::temp_dir().join(format!("hearing-mock-{mode}-{}.wav",std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())); fs::write(&path,b"sample").unwrap();
        assert_eq!(transcribe(&settings,&path, |_|{}).unwrap(),"hello"); handle.join().unwrap(); fs::remove_file(path).unwrap();
    }
    #[test] fn mock_server_validates_stream_request() { mock_request("streaming"); }
    #[test] fn mock_server_validates_transcription_request() { mock_request("transcription"); }
    #[test] fn settings_are_private_and_token_not_public() {
        let path=std::env::temp_dir().join(format!("hearing-settings-{}.json",super::super::clock())); let mut s=Settings::default(); s.token="secret".into(); persist(&path,&s).unwrap();
        assert!(!serde_json::to_string(&s.public()).unwrap().contains("secret"));
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777,0o600); }
        fs::remove_file(path).unwrap();
    }
}
