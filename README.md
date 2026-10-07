<img src="src-tauri/icons/128x128.png" alt="Hearfolio logo" width="64" height="64">

# Hearfolio

Turn audio recordings into text and keep them in a personal archive.

Hearfolio is a compact desktop app for transcribing meetings, interviews, and voice notes. Import a recording, choose an on-device model or OpenRouter, and read the transcript as it becomes available. Return to recordings later to listen, rename them, export their text, or transcribe them again.

Built with Tauri 2, Rust, TypeScript, and Vite. The desktop workflow is currently developed and verified on macOS. The interface is currently in Russian; this README is in English.

## Features

- Import M4A, MP3, WAV, AAC, FLAC, OGG, and MP4 files.
- Transcribe locally with Whisper Tiny, Base, or Small, or Nemotron 3.5 Streaming.
- Download models from the app with download size, percentage, and estimated time remaining.
- Connect OpenRouter using a base URL, API token, and model ID.
- See processing stages, elapsed time, and partial transcripts. Available progress details depend on the engine.
- Browse and search recordings by name, play the original audio, and rename recordings.
- Copy or export transcripts as `.txt` files. Renamed recordings use the new name when exporting text.
- Run transcription again with another model. A failed retry preserves the previous saved transcript.
- Delete an individual recording or clear the archive with confirmation.
- Switch between light and dark themes. The selected engine and model persist across launches.

## Getting started on macOS

### Prerequisites

To develop and build the app, install:

- Node.js 22.12 or later and npm.
- Rust and Cargo through [rustup](https://rustup.rs/).
- Xcode Command Line Tools: `xcode-select --install`.
- [Homebrew](https://brew.sh/) for the audio runtime tools below.

The packaged app also needs audio runtime tools installed on the computer. `ffmpeg` is used to prepare audio for both local and cloud transcription. Whisper additionally requires `whisper-cli`; Nemotron requires `nemo-speech`.

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

This is a private repository, so cloning requires GitHub access to it.

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

## Transcription engines

| Engine | Where audio is processed | How text appears |
| --- | --- | --- |
| Whisper | On your computer via `whisper-cli` | Completed transcript segments appear as the CLI processes the file. |
| Nemotron 3.5 Streaming | On your computer via NeMo-Speech.cpp | Text arrives through the local streaming WebSocket API. |
| OpenRouter: streaming text | The configured remote API | Compatible audio models return text through Chat Completions streaming. |
| OpenRouter: transcription | The configured remote API | A specialized speech-to-text endpoint returns completed audio chunks. |

Whisper and Nemotron can run locally after their runtimes and models are installed. Model support, processing speed, and recognition quality depend on the selected model and hardware.

## OpenRouter setup

Open Settings and enter:

1. The API base URL, normally `https://openrouter.ai/api/v1`.
2. Your API token.
3. A compatible model ID. You can load available models or enter an ID manually.
4. The text delivery mode, then apply the settings.

Choose **streaming text** for audio-capable chat models using `/chat/completions`. Choose **transcription** for specialized speech-to-text models using `/audio/transcriptions`, such as `fish-audio/transcribe-1-pro`. The mode must match the model's endpoint.

Hearfolio converts audio to mono WAV at 16 kHz and sends sequential chunks of up to 30 seconds. Streaming models display text during each response; transcription models display completed chunks. Chunk boundaries can affect recognition of words near the cut.

The saved token can be replaced or removed in Settings. Model availability and pricing are determined by the configured service.

API references: [Audio input](https://openrouter.ai/docs/guides/overview/multimodal/audio), [Speech-to-Text](https://openrouter.ai/docs/guides/overview/multimodal/stt), and [Streaming](https://openrouter.ai/docs/api_reference/streaming).

## Archive and storage

Hearfolio stores its data in `~/.hearing/`:

| Path | Contents |
| --- | --- |
| `input/` | Copies of imported audio recordings. |
| `output/` | Text from completed transcriptions. |
| `models/` | Downloaded Whisper models. Nemotron uses the NeMo-Speech.cpp model cache. |
| `temp/` | Temporary processing files. |
| `history.json` | Recording names, model information, and result paths. |
| `settings.json` | Engine, model, and connection settings. |

In on-device mode, audio is processed locally. When OpenRouter is selected, audio is sent to the configured API. The archive remains on your computer in both modes.

The OpenRouter token is stored in `settings.json`, with owner-only file permissions (`0600`) on macOS and Linux. It is not returned to the frontend after saving.

Renaming updates the recording's display name without moving its audio or transcript files. Deletion only removes files recorded in Hearfolio's history; unrelated files in the archive directories are left untouched.

## Project structure

```text
src/                 Desktop interface, API bridge, icons, and theme styles
src-tauri/src/       Native commands, local engines, cloud integration, and history
src-tauri/icons/     Application icons
docs/                Design references and product notes
```

Additional notes: [Interface design](docs/design.md) and [Product experience plan](docs/product-plan.md) (in Russian).
