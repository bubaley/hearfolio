<img src="src-tauri/icons/128x128.png" alt="Hearfolio logo" width="64" height="64">

# Hearfolio

Turn audio recordings into text and keep them in a personal archive.

Hearfolio is a compact desktop and Android app for transcribing meetings, interviews, and voice notes. Record audio or import a file, choose an on-device model or OpenRouter, and read the transcript as it becomes available. Return to recordings later to listen, rename them, export their text, or transcribe them again.

Built with Tauri 2, Rust, TypeScript, and Vite. The interface supports English and Russian, follows the device language by default, and can be changed in Settings.

## Downloads and updates

Download installers from [GitHub Releases](https://github.com/bubaley/hearfolio/releases/latest): macOS (Apple Silicon and Intel), Windows x86_64 (NSIS installer), Linux x86_64, and Android arm64. Android uses OpenRouter; on-device Whisper and Nemotron are desktop features. iOS is not distributed.

Desktop apps check GitHub for signed updates automatically. Installation and restart are explicit actions, and are unavailable while a recording is being processed. Android offers the new APK for installation through the operating system.

Releases use SemVer derived from Conventional Commits, with a changelog and downloadable artifacts. See [Release pipeline](docs/releases.md) for the build, signing, and publishing process.

## Features

- Import M4A, MP3, WAV, AAC, FLAC, OGG, and MP4 files.
- Record with the microphone, watch a measured waveform and timer, pause, resume, save, or cancel. Android and desktop record mono PCM WAV.
- Transcribe locally with Whisper Tiny, Base, or Small, or Nemotron 3.5 Streaming.
- Download models from the app with download size, percentage, and estimated time remaining.
- Connect OpenRouter using a base URL and API token, then choose an audio model from the searchable catalog in the recording. The supported endpoint is selected automatically.
- See processing stages, elapsed time, and partial transcripts. Available progress details depend on the engine.
- Browse and search recordings by name, play the original audio, and rename the title with its audio extension preserved.
- Copy or export transcripts as `.txt` files. Renamed recordings use the new name when exporting text.
- Run transcription again with another model. A failed retry preserves the previous saved transcript.
- Create reusable post-processing rules, such as a brief summary, meeting decisions, or action items. Apply a rule with a text model from OpenRouter and keep its result alongside the original transcript.
- Share audio, transcripts, and processed results through Android's system share sheet, or save audio to a location chosen with the system picker.
- Delete an individual recording or clear the archive with confirmation.
- Switch between light and dark themes, and choose English, Russian, or the device language.
- Keep engine, model, and recognition mode with each recording. New recordings use the last configuration that successfully started returning text.
- Choose the archive parent folder; Hearfolio creates a `.hearfolio` directory inside it.

## Getting started on macOS

### Prerequisites

To develop and build the app, install:

- Node.js 22.12 or later and npm.
- Rust and Cargo through [rustup](https://rustup.rs/).
- Xcode Command Line Tools: `xcode-select --install`.
- [Homebrew](https://brew.sh/) for the audio runtime tools below.

On-device transcription needs audio runtime tools installed on the computer. `ffmpeg` prepares local audio, Whisper additionally requires `whisper-cli`, and Nemotron requires `nemo-speech`. OpenRouter uses the built-in audio decoder and needs no external audio tools.

```sh
brew install ffmpeg whisper.cpp
```

The [Homebrew whisper.cpp package](https://formulae.brew.sh/formula/whisper.cpp) provides `whisper-cli`. Model weights are downloaded separately through Hearfolio.

For Nemotron, install the CLI from the [official NVIDIA NeMo-Speech.cpp project](https://github.com/NVIDIA/NeMo-Speech.cpp):

```sh
curl -fsSL https://github.com/NVIDIA/NeMo-Speech.cpp/raw/main/scripts/install.sh | sh
```

Open a new terminal after installation so the updated `PATH` takes effect, then restart Hearfolio.

### Run from source

```sh
git clone git@github.com:bubaley/hearfolio.git
cd hearfolio
npm ci
npm run tauri dev
```

The repository is public. HTTPS cloning is also available: `git clone https://github.com/bubaley/hearfolio.git`.

### Build

```sh
npm run tauri build
```

On macOS, the build produces an application bundle and a DMG under:

```text
src-tauri/target/release/bundle/macos/
src-tauri/target/release/bundle/dmg/
```

### Checks

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

## Windows, Linux, and Android

On Windows x86_64, run `Hearfolio_VERSION_windows_x86_64-setup.exe` from Releases. The installer downloads WebView2 if it is missing. Updates use the signed installer through the app's update settings. The installer currently has no Microsoft code signing certificate, so SmartScreen can warn on the initial download; see [release signing](docs/releases.md#signing-setup).

Windows source builds require [Tauri's Windows prerequisites](https://tauri.app/start/prerequisites/#windows): Visual Studio C++ Build Tools, WebView2, Node.js, and Rust. Run `npm ci` and `npm run tauri -- build --bundles nsis` in the repository. Local recognition also needs the selected engine runtime on `PATH`.

On Linux x86_64, use the AppImage or install the DEB/RPM package from Releases. For an AppImage, make it executable and run it:

```sh
chmod +x Hearfolio_*_linux_x86_64.AppImage
./Hearfolio_*_linux_x86_64.AppImage
```

OpenRouter works without additional audio tools. Local recognition needs `ffmpeg` and a built `whisper-cli` or `nemo-speech` on `PATH`; see the [whisper.cpp build instructions](https://github.com/ggml-org/whisper.cpp#quick-start) and [NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp). Building the app itself requires the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux).

On Android arm64, install the signed APK from Releases. Record a voice note or choose an audio document with the system picker, configure OpenRouter, and select a model in the recording. Audio is decoded in the app and text can be exported through the system document picker. The archive is stored in the app's private storage; choosing another archive parent folder is a desktop feature. Removing the Android app removes its private archive, so export transcripts you want to keep.

Android source builds require the [Tauri Android prerequisites](https://v2.tauri.app/start/prerequisites/#android), then `npm run tauri -- android init`, `node scripts/prepare-android.mjs`, and `npm run tauri -- android build --apk --target aarch64`.

Android playback uses the system media player, including seeking and duration, rather than depending on WebView audio codecs. The system share sheet lets you send a recording to installed apps such as Telegram. Hearfolio grants the selected recipient temporary access to the shared file; the original remains in its private archive.

## Settings

On a phone, Settings opens a list of categories with a back button inside each category. On desktop, categories remain visible beside the selected settings. Unsaved connection and language edits are protected when switching sections.

## Organizations (Hearfolio Cloud)

Open **Settings → Cloud**, enter your team's Hearfolio Cloud address (any domain, self-hosted), and sign in in the browser. Hearfolio checks that the server is compatible first and signs in with OAuth and PKCE; the access key is kept in the system keychain (Android Keystore on Android) and is revoked when you disconnect. Several servers and accounts can be connected; each shows its organizations.

Choose **To** on the start screen or in the recorder: this device, or an organization with an optional project and shared/personal visibility. Recordings for an organization are uploaded in the background with resumable multipart uploads that survive restarts and wait for the network; the server transcribes them. **Share → Copy to organization** sends an existing recording, with its transcript if it has one. Tabs above the recording list show each organization's recordings with upload and transcription status, transcripts and summaries. Cloud audio stays on the device after upload or playback and is removed after the period set in **Keep audio on device** (30 days by default); text is kept. Details: [Organizations](docs/organizations.md) (in Russian).

## Microphone recording

Select **Record audio** on the new recording screen. Allow microphone access when the operating system asks, then use **Pause**, **Resume**, or **Save recording**. Cancel discards the capture after you confirm it; stopping a recording longer than five minutes also asks first, and the microphone keeps recording while the question is open. A failed archive save keeps the recording available for another attempt. Open **Share** on a saved recording to send its audio or text, save a copy, or copy the transcript. Text actions appear once a transcript is available.

The recorder shows measured audio history to the left of the current-time marker. On Android, its timer and waveform come from captured PCM, survive returning from the background, and exclude paused time. Recoverable microphone failures recreate the audio input while keeping the same WAV file and sample rate. If recovery fails, the captured audio is preserved with an interruption warning. A pending disk write must finish before the recording becomes available for playback or export.

Android uses foreground services with persistent notifications for recording, transcription, text processing, and uploads to an organization. You can lock the screen after starting an operation. Returning to Hearfolio restores its progress or committed result without repeating a cloud request. If the process is killed, recoverable WAV audio is retained and unfinished processing is reported once as interrupted: the recording list shows how many segments are done, **Archive** lists audio that was never saved, and temporary files of the interrupted job are removed at the next start. Recording audio does not send it to OpenRouter until you start transcription with that service selected.

Android capture is limited to four hours per session and background processing to twelve hours. System restrictions can stop work earlier. **Settings → Background tasks** links to the app's Android battery settings if the device interrupts recording. Force-stopping the app ends its work; it does not restart paid processing automatically.

Android captures uncompressed mono samples through `AudioRecord`, using the standard `MIC` source with device preprocessing and a `DEFAULT` fallback. System automatic gain control is enabled when available; devices without it can still record. The waveform shows measured microphone input. Silence or quiet speech does not trigger a microphone warning. On macOS, Hearfolio explicitly requests microphone permission before starting capture. If access was previously denied, enable Hearfolio under **System Settings → Privacy & Security → Microphone**.

## Post-processing rules

Open **Settings → Rules** to create a named instruction. Start from **Brief summary**, **Meeting decisions**, or **Action items**, or write your own instruction.

After transcription, select **Process text**, choose a saved rule and a text model from the searchable catalog, and run it. This sends the transcript and instruction to the configured OpenRouter API. The original audio is not sent again for this step. The response appears progressively and is saved only after a successful completion.

The **Results** tab keeps each output with its rule, model, and time. Copy, share, or export it independently of the original transcript. Editing or deleting a rule does not change previous results. New processing uses the last text model that completed successfully.

## Transcription engines

| Engine | Where audio is processed | How text appears |
| --- | --- | --- |
| Whisper | On your computer via `whisper-cli` | Completed transcript segments appear as the CLI processes the file. |
| Nemotron 3.5 Streaming | On your computer via NeMo-Speech.cpp | Text arrives through the local streaming WebSocket API. |
| OpenRouter: streaming text | The configured remote API | Compatible audio models return text through Chat Completions streaming. |
| OpenRouter: transcription | The configured remote API | A specialized speech-to-text endpoint returns completed audio chunks. |

Long recordings are sent to OpenRouter in segments (two minutes by default, configurable in **Settings → Recognition**). Each finished segment is saved before the next request, so a retry continues where the previous attempt stopped. A dropped connection, a rate limit or a provider outage repeats only the current segment, up to five times with growing pauses. If a model stops at its output limit, the same audio is sent again as two shorter pieces cut at a pause; a passage it still cannot finish, or answers with nothing although the audio is audible, keeps whatever text it produced and is listed with the record as a time range to check by ear. Segments measured as silent are skipped without a request being repeated. A model that returns no text at all stops after the first couple of segments instead of spending a request on every one. The requested output size never exceeds the limit the catalog states for the model, and segments finished before an app update are continued with the cut that produced them.

Whisper and Nemotron can run locally after their runtimes and models are installed. Model support, processing speed, and recognition quality depend on the selected model and hardware.

## OpenRouter setup

Open **Settings → Recognition** and enter:

1. The API base URL, normally `https://openrouter.ai/api/v1`.
2. Your API token.
3. Apply the connection settings.
4. In a recording, select OpenRouter and choose a model from the catalog. You can search by model name or ID.

The app chooses the supported text delivery mode from the model catalog. Audio-capable chat models use `/chat/completions` with streaming text. Specialized speech-to-text models, such as `fish-audio/transcribe-1-pro`, use `/audio/transcriptions` and return completed chunks. Models and delivery modes are selected in the recording, rather than in global Settings.

Hearfolio decodes audio to mono PCM WAV at its original sample rate and sends sequential chunks of up to 30 seconds. AAC/M4A, MP3, PCM/WAV, FLAC, and Vorbis/OGG codecs are supported by the built-in decoder. Streaming models display text during each response; transcription models display completed chunks. Chunk boundaries can affect recognition of words near the cut.

The saved token can be replaced or removed in Settings. Model availability and pricing are determined by the configured service.

API references: [Audio input](https://openrouter.ai/docs/guides/overview/multimodal/audio), [Speech-to-Text](https://openrouter.ai/docs/guides/overview/multimodal/stt), and [Streaming](https://openrouter.ai/docs/api_reference/streaming).

## Archive and storage

Choose an archive parent folder in Settings. Hearfolio stores recording data in `.hearfolio/` inside that folder (for example, `~/Documents/.hearfolio/`). The default parent is your home directory. Existing data from the former `~/.hearing/` archive is copied safely on upgrade; the original archive remains available.

| Path | Contents |
| --- | --- |
| `input/` | Copies of imported audio recordings. |
| `output/` | Text from completed transcriptions. |
| `models/` | Downloaded Whisper models. Nemotron uses the NeMo-Speech.cpp model cache. |
| `temp/` | Temporary processing files. |
| `history.json` | Recording names, recognition configurations, model information, and result paths. |
| `postprocess-rules.json` | Saved names and instructions for post-processing. |

In on-device mode, audio is processed locally. When OpenRouter is selected, audio is sent to the configured API. The archive remains on your computer in both modes.

Language, connection settings, the current archive location, and the last working recognition configuration are stored separately in the application configuration directory. The OpenRouter token is kept in its settings file with owner-only permissions (`0600`) on macOS and Linux and is not returned to the frontend after saving.

On macOS, the configuration directory is `~/Library/Application Support/Hearfolio/`. It contains `settings.json` and `storage.json`; choosing another archive folder does not reset the connection or language.

Changing the archive folder copies the current archive and updates recording paths before switching to the new location. Existing source files remain available. An occupied destination archive is rejected to prevent accidental overwrites.

Renaming updates the recording's display name without moving its audio or transcript files. Deletion only removes files recorded in Hearfolio's history; unrelated files in the archive directories are left untouched.

Processed results and the instructions used to produce them are stored with each recording in `history.json`. They move with the archive and remain separate from the original transcript in `output/`.

## Project structure

```text
src/                 Desktop interface, API bridge, icons, and theme styles
src-tauri/src/       Native commands, local engines, cloud integration, and history
src-tauri/icons/     Application icons
docs/                Design references and product notes
```

Additional notes: [Interface design](docs/design.md) and [Product experience plan](docs/product-plan.md) (in Russian).
